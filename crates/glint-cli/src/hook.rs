//! `glint hook`: the command a coding agent runs as a hook. Forwards the hook's stdin JSON to
//! glintd, always exits 0, and takes at most ~1.5 s even when Glint is down.
//!
//! It also tells the agent what Glint saw, through Claude Code's `additionalContext`: at session
//! start, that Glint is running and which recent failures this project has; on each prompt, only
//! the failures that are new since the previous prompt. That puts runtime failures the agent
//! cannot see in a terminal (a request in the browser, a queue worker) in front of it, without
//! it having to think of asking. Nothing is printed when there is nothing new.
//!
//! And when the agent wants to stop, programs of this project that failed during its turn keep
//! it going once: it is told what failed and asked to fix it or say why it is expected (Claude
//! Code's Stop `decision: block`; never twice in a row; `GLINT_STOP_GUARD=0` turns it off).

use std::io::Read;
use std::path::PathBuf;
use std::time::Duration;

use glint_protocol::Envelope;
use serde_json::{Value, json};

pub fn run(base: &str, agent: &str) -> Result<(), String> {
    let mut body = Vec::new();
    if std::io::stdin().read_to_end(&mut body).is_err() || body.is_empty() {
        return Ok(());
    }
    let base = base.trim_end_matches('/');
    let http = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_millis(300))
        .timeout(Duration::from_secs(1))
        .build();
    // Errors are deliberately ignored: a debugging aid must never block the agent.
    let _ = http.post(&format!("{base}/v1/hooks/{agent}")).set("Content-Type", "application/json").send_bytes(&body);
    if agent == "claude-code"
        && let Ok(hook) = serde_json::from_slice::<Value>(&body)
        && let Some(out) = context(&http, base, &hook)
    {
        println!("{out}");
    }
    Ok(())
}

/// The `additionalContext` reply for SessionStart and UserPromptSubmit, if there is news.
fn context(http: &ureq::Agent, base: &str, hook: &Value) -> Option<String> {
    let event = hook["hook_event_name"].as_str()?;
    if event == "Stop" {
        return stop_guard(http, base, hook);
    }
    if event != "SessionStart" && event != "UserPromptSubmit" {
        return None;
    }
    let cwd = hook["cwd"].as_str()?;
    let session = hook["session_id"].as_str().unwrap_or("unknown");
    let project = glint_core::project::resolve(cwd).map(|r| r.project).or_else(|| glint_core::project::folder_name(cwd))?;
    let seen = state_file(session);
    let last_seen = seen.as_ref().and_then(|f| std::fs::read_to_string(f).ok()).map(|s| s.trim().to_owned()).filter(|s| !s.is_empty());

    let mut url = format!("{base}/v1/events?project={}&level=error&limit=20", enc(&project));
    if event == "UserPromptSubmit"
        && let Some(id) = &last_seen
    {
        url.push_str(&format!("&after={}", enc(id)));
    }
    let got: Value = http.get(&url).call().ok()?.into_json().ok()?;
    let events: Vec<Envelope> = serde_json::from_value(got["events"].clone()).ok()?;
    // The agent's own failed tool calls are already in front of it; only programs count.
    let mut fails: Vec<&Envelope> = events.iter().filter(|e| e.origin.lang.as_deref() != Some("agent")).collect();
    if event == "SessionStart" {
        let day_ago = chrono::Utc::now() - chrono::Duration::hours(24);
        fails.retain(|e| e.ts > day_ago);
    }
    // Remember where we are, so the next prompt reports only what is new: the newest error seen,
    // or, when there is none yet, the newest event of any kind.
    if let Some(f) = &seen {
        let mark = match events.last() {
            Some(e) => Some(e.id.clone()),
            None if last_seen.is_none() => http
                .get(&format!("{base}/v1/events?limit=1"))
                .call()
                .ok()
                .and_then(|r| r.into_json::<Value>().ok())
                .and_then(|v| v["events"][0]["id"].as_str().map(str::to_owned)),
            None => None,
        };
        if let (Some(mark), Some(dir)) = (mark, f.parent()) {
            let _ = std::fs::create_dir_all(dir);
            let _ = std::fs::write(f, mark);
        }
    }

    if event == "SessionStart"
        && let Some(dir) = seen.as_ref().and_then(|f| f.parent())
    {
        prune_state(dir);
    }

    let text = if event == "SessionStart" {
        let mut t = format!(
            "Glint (local debug desk, MCP server `glint`) is recording what programs in project `{project}` do: \
             values sent with glint(), logs, SQL, exceptions, timings, LLM calls. glint(value) works in PHP, Python \
             and Node without an import; prefer it over print/var_dump/console.log to inspect values, read them \
             back with glint_wait_for or glint_events, and remove those calls when done. Uncaught errors of \
             PHP, Python and Node processes are recorded with no code; for a web front end in development add \
             <script src=\"{base}/v1/browser.js\" data-project=\"{project}\"></script> to the dev \
             page. To prove a change works: place glint() markers, run it, confirm them with glint_wait_for. \
             When something fails at runtime, start with glint_last_error."
        );
        if !fails.is_empty() {
            t.push_str(&format!("\nRecent failures in {project} (last 24 h):\n{}", list(&fails)));
        }
        t
    } else {
        if fails.is_empty() || last_seen.is_none() {
            return None;
        }
        format!(
            "Glint recorded {} new failure{} in {project} since the last prompt (programs, not your tools):\n{}\nglint_event <id> gives the full stack and values.",
            fails.len(),
            if fails.len() == 1 { "" } else { "s" },
            list(&fails)
        )
    };
    Some(json!({ "hookSpecificOutput": { "hookEventName": event, "additionalContext": text } }).to_string())
}

/// Failures of this project's programs since the turn began keep the agent from stopping, once.
fn stop_guard(http: &ureq::Agent, base: &str, hook: &Value) -> Option<String> {
    if hook["stop_hook_active"] == true || std::env::var("GLINT_STOP_GUARD").is_ok_and(|v| v == "0") {
        return None;
    }
    let cwd = hook["cwd"].as_str()?;
    let project = glint_core::project::resolve(cwd).map(|r| r.project).or_else(|| glint_core::project::folder_name(cwd))?;
    let seen = state_file(hook["session_id"].as_str().unwrap_or("unknown"))?;
    let mark = std::fs::read_to_string(&seen).ok().map(|s| s.trim().to_owned()).filter(|s| !s.is_empty())?;
    let url = format!("{base}/v1/events?project={}&level=error&limit=20&after={}", enc(&project), enc(&mark));
    let got: Value = http.get(&url).call().ok()?.into_json().ok()?;
    let events: Vec<Envelope> = serde_json::from_value(got["events"].clone()).ok()?;
    if let Some(newest) = events.last() {
        let _ = std::fs::write(&seen, &newest.id); // reported once
    }
    // A failure whose code was edited after it happened is most likely the agent reproducing the
    // bug before fixing it (seen in a live session): not a reason to hold it back.
    let fails: Vec<&Envelope> = events
        .iter()
        .filter(|e| e.origin.lang.as_deref() != Some("agent"))
        .filter(|e| !edited_since(e))
        .collect();
    if fails.is_empty() {
        return None;
    }
    let reason = format!(
        "Before you finish: Glint recorded {} failure{} in {project} while you worked on this turn (programs, not your tools):\n{}\n\
         Look at them (glint_event <id> has the stack and values) and fix what your change broke, or tell the user \
         why they are expected. You will not be stopped twice.",
        fails.len(),
        if fails.len() == 1 { "" } else { "s" },
        list(&fails)
    );
    Some(json!({ "decision": "block", "reason": reason }).to_string())
}

/// Where a failure was thrown (first app frame), else where it was reported.
fn throw_site(e: &Envelope) -> (Option<String>, Option<u64>) {
    let frame = e.payload["frames"].as_array().and_then(|fs| fs.iter().find(|f| f["vendor"] != true && f["file"].is_string()));
    match frame {
        Some(f) => (f["file"].as_str().map(str::to_owned), f["line"].as_u64()),
        None => (e.origin.file.clone(), e.origin.line.map(u64::from)),
    }
}

/// The file the failure came from was changed after the failure.
fn edited_since(e: &Envelope) -> bool {
    let Some(file) = throw_site(e).0 else { return false };
    std::fs::metadata(&file)
        .and_then(|m| m.modified())
        .is_ok_and(|t| chrono::DateTime::<chrono::Utc>::from(t) > e.ts)
}

fn list(fails: &[&Envelope]) -> String {
    fails
        .iter()
        .rev()
        .take(5)
        .map(|e| {
            let p = &e.payload;
            let what = match (p["class"].as_str(), p["message"].as_str()) {
                (Some(c), Some(m)) => format!("{c}: {m}"),
                (None, Some(m)) => m.to_owned(),
                (Some(c), None) => c.to_owned(),
                _ => e.kind.clone(),
            };
            let (file, line) = throw_site(e);
            let at = file.map(|f| format!(" at {f}{}", line.map(|l| format!(":{l}")).unwrap_or_default())).unwrap_or_default();
            format!("- {}{at} ({}, id {})", what.chars().take(200).collect::<String>(), e.ts.format("%H:%M:%S"), e.id)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Session state files older than a week are left over from finished sessions.
fn prune_state(dir: &std::path::Path) {
    let week = std::time::Duration::from_secs(7 * 24 * 3600);
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            if e.metadata().and_then(|m| m.modified()).is_ok_and(|t| t.elapsed().is_ok_and(|age| age > week)) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

fn state_file(session: &str) -> Option<PathBuf> {
    let safe: String = session.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').take(80).collect();
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".glint/hooks").join(format!("{safe}.last")))
}

fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect()
}

/// Hook settings for `.claude/settings.json` (project) or `~/.claude/settings.json` (all).
pub fn claude_code_config(binary: &str) -> String {
    let events = [
        "SessionStart",
        "UserPromptSubmit",
        "PreToolUse",
        "PostToolUse",
        "PostToolUseFailure",
        "PermissionRequest",
        "Notification",
        "SubagentStart",
        "SubagentStop",
        "Stop",
        "PreCompact",
        "SessionEnd",
    ];
    let mut hooks = serde_json::Map::new();
    for e in events {
        let needs_matcher = matches!(e, "PreToolUse" | "PostToolUse" | "PostToolUseFailure" | "PermissionRequest");
        let entry = if needs_matcher {
            serde_json::json!([{ "matcher": "*", "hooks": [{ "type": "command", "command": format!("{binary} hook"), "timeout": 5 }] }])
        } else {
            serde_json::json!([{ "hooks": [{ "type": "command", "command": format!("{binary} hook"), "timeout": 5 }] }])
        };
        hooks.insert(e.into(), entry);
    }
    serde_json::to_string_pretty(&serde_json::json!({ "hooks": hooks })).unwrap_or_default()
}
