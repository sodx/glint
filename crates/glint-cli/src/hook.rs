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
    let http = crate::client::builder()
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

/// How many failures one hook asks glintd for, and how many it lists; the rest are counted.
const FETCH: usize = 200;
const SHOWN: usize = 5;

/// The `additionalContext` reply for SessionStart and UserPromptSubmit, if there is news.
fn context(http: &ureq::Agent, base: &str, hook: &Value) -> Option<String> {
    let event = hook["hook_event_name"].as_str()?;
    // The Settings page (or config.toml, or GLINT_AGENT_CONTEXT / GLINT_STOP_GUARD) can turn these off.
    let (settings, _) = glint_core::config::load();
    if event == "Stop" {
        return settings.stop_guard.then(|| stop_guard(http, base, hook)).flatten();
    }
    if event != "SessionStart" && event != "UserPromptSubmit" {
        return None;
    }
    let cwd = hook["cwd"].as_str()?;
    let session = hook["session_id"].as_str().unwrap_or("unknown");
    let project = glint_core::project::resolve(cwd).map(|r| r.project).or_else(|| glint_core::project::folder_name(cwd))?;
    let seen = state_file(session);
    let last_seen = seen.as_deref().and_then(read_mark);
    if !settings.agent_context && event == "UserPromptSubmit" {
        // Nothing to say, but the Stop guard counts from where this turn began.
        if let (Some(f), Some(id)) = (&seen, newest_id(http, base)) {
            write_mark(f, &id);
        }
        return None;
    }

    let mut url = format!("{base}/v1/events?project={}&level=error&limit={FETCH}", enc(&project));
    if event == "UserPromptSubmit"
        && let Some(id) = &last_seen
    {
        url.push_str(&format!("&after={}", enc(id)));
    }
    let got: Value = match http.get(&url).call() {
        Ok(r) => r.into_json().ok()?,
        // Not running: the agent should still learn what Glint is, and that it is off.
        Err(ureq::Error::Transport(_)) if event == "SessionStart" => {
            let text = format!(
                "{}\nGlint is not running right now (nothing answers at {base}), so nothing is being recorded and the \
                 glint_* MCP tools will fail. Start it: open the Glint app, or run `glintd` (`glint install --only service` \
                 keeps it running at login). `glint doctor` checks the setup.",
                intro(base, &project)
            );
            return Some(json!({ "hookSpecificOutput": { "hookEventName": event, "additionalContext": text } }).to_string());
        }
        Err(_) => return None,
    };
    let events: Vec<Envelope> = serde_json::from_value(got["events"].clone()).ok()?;
    let capped = events.len() >= FETCH;
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
            None if last_seen.is_none() => newest_id(http, base),
            None => None,
        };
        if let Some(mark) = mark {
            write_mark(f, &mark);
        }
    }

    if event == "SessionStart"
        && let Some(dir) = seen.as_ref().and_then(|f| f.parent())
    {
        prune_state(dir);
    }

    let listed: Vec<(&Envelope, Option<String>)> = fails.iter().map(|e| (*e, None)).collect();
    let more = more_line(fails.len(), capped, &project, if event == "UserPromptSubmit" { last_seen.as_deref() } else { None });
    let text = if event == "SessionStart" {
        let mut t = intro(base, &project);
        if !fails.is_empty() {
            t.push_str(&format!("\nRecent failures in {project} (last 24 h):\n{}{more}", list(&listed)));
        }
        t
    } else {
        if fails.is_empty() || last_seen.is_none() {
            return None;
        }
        format!(
            "Glint recorded {}{} new failure{} in {project} since the last prompt (programs, not your tools):\n{}{more}\nglint_event <id> gives the full stack and values.",
            if capped { "at least " } else { "" },
            fails.len(),
            if fails.len() == 1 { "" } else { "s" },
            list(&listed)
        )
    };
    let ids: Vec<String> = fails.iter().rev().take(SHOWN).map(|e| e.id.clone()).collect();
    record(http, base, session, &project, glint_core::agents::HOOK_CONTEXT, &ids, json!({ "event": event, "failures": fails.len() }));
    Some(json!({ "hookSpecificOutput": { "hookEventName": event, "additionalContext": text } }).to_string())
}

/// What a session learns at its start, whether or not Glint is running.
fn intro(base: &str, project: &str) -> String {
    format!(
        "Glint (local debug desk, MCP server `glint`) is recording what programs in project `{project}` do: \
         values sent with glint(), logs, SQL, exceptions, timings, LLM calls. glint(value) works in PHP, Python \
         and Node without an import; prefer it over print/var_dump/console.log to inspect values, read them \
         back with glint_wait_for or glint_events, and remove those calls when done. Uncaught errors of \
         PHP, Python and Node processes are recorded with no code; for a web front end in development add \
         <script src=\"{base}/v1/browser.js\" data-project=\"{project}\"></script> to the dev \
         page. To prove a change works: place glint() markers, run it, confirm them with glint_wait_for. \
         When something fails at runtime, start with glint_last_error."
    )
}

/// `\n(12 more not shown: …)` when a list was cut, with the query that lists them all.
fn more_line(total: usize, capped: bool, project: &str, after: Option<&str>) -> String {
    if total <= SHOWN {
        return String::new();
    }
    let n = total - SHOWN;
    let after = after.map(|a| format!(" after={a}")).unwrap_or_default();
    format!(
        "\n({}{n} more not shown; glint_events project={project} level=error{after} lists them)",
        if capped { "at least " } else { "" }
    )
}

/// The newest stored event of any kind: where counting starts when nothing was seen yet.
fn newest_id(http: &ureq::Agent, base: &str) -> Option<String> {
    http.get(&format!("{base}/v1/events?limit=1"))
        .call()
        .ok()
        .and_then(|r| r.into_json::<Value>().ok())
        .and_then(|v| v["events"][0]["id"].as_str().map(str::to_owned))
}

fn read_mark(f: &std::path::Path) -> Option<String> {
    std::fs::read_to_string(f).ok().map(|s| s.trim().to_owned()).filter(|s| !s.is_empty())
}

fn write_mark(f: &std::path::Path, id: &str) {
    if let Some(dir) = f.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(f, id);
}

/// Failures of this project's programs since the turn began keep the agent from stopping, once.
fn stop_guard(http: &ureq::Agent, base: &str, hook: &Value) -> Option<String> {
    let cwd = hook["cwd"].as_str()?;
    let project = glint_core::project::resolve(cwd).map(|r| r.project).or_else(|| glint_core::project::folder_name(cwd))?;
    let session = hook["session_id"].as_str().unwrap_or("unknown");
    if hook["stop_hook_active"] == true {
        outcome(http, base, session, &project);
        return None;
    }
    let seen = state_file(session)?;
    let Some(mark) = read_mark(&seen) else {
        // The session's start was never marked (Glint was down then, or the hooks were added
        // mid-session): this turn's failures cannot be told from older ones, but the next
        // turn's can.
        if let Some(id) = newest_id(http, base) {
            write_mark(&seen, &id);
        }
        return None;
    };
    let url = format!("{base}/v1/events?project={}&level=error&limit={FETCH}&after={}", enc(&project), enc(&mark));
    let got: Value = http.get(&url).call().ok()?.into_json().ok()?;
    let events: Vec<Envelope> = serde_json::from_value(got["events"].clone()).ok()?;
    let capped = events.len() >= FETCH;
    if let Some(newest) = events.last() {
        write_mark(&seen, &newest.id); // reported once
    }
    let programs: Vec<&Envelope> = events.iter().filter(|e| e.origin.lang.as_deref() != Some("agent")).collect();
    if programs.is_empty() {
        return None;
    }
    // A failure whose code was edited after it happened is often the agent reproducing the bug
    // before fixing it. It is left out only when a later run of the same program passed;
    // otherwise it is listed as possibly fixed.
    let runs: Vec<Value> = http
        .get(&format!("{base}/v1/runs?project={}&limit=200", enc(&project)))
        .call()
        .ok()
        .and_then(|r| r.into_json::<Value>().ok())
        .and_then(|v| v["runs"].as_array().cloned())
        .unwrap_or_default();
    let fails: Vec<(&Envelope, Option<String>)> = programs
        .into_iter()
        .filter_map(|e| {
            let Some(file) = edited_since(e) else { return Some((e, None)) };
            if passed_later(e, &runs) {
                return None;
            }
            Some((e, Some(format!("possibly fixed: {file} changed after it; rerun to confirm"))))
        })
        .collect();
    if fails.is_empty() {
        return None;
    }
    let ids: Vec<String> = fails.iter().map(|(e, _)| e.id.clone()).collect();
    if let Some(dir) = seen.parent() {
        let _ = std::fs::write(dir.join(format!("{}.guard", safe_name(session))), ids.join("\n"));
    }
    record(http, base, session, &project, glint_core::agents::HOOK_STOP_GUARD, &ids, json!({ "failures": ids.len() }));
    let reason = format!(
        "Before you finish: Glint recorded {}{} failure{} in {project} while you worked on this turn (programs, not your tools):\n{}{}\n\
         Look at them (glint_event <id> has the stack and values) and fix what your change broke, or tell the user \
         why they are expected. You will not be stopped twice.",
        if capped { "at least " } else { "" },
        fails.len(),
        if fails.len() == 1 { "" } else { "s" },
        list(&fails),
        more_line(fails.len(), capped, &project, Some(&mark))
    );
    Some(json!({ "decision": "block", "reason": reason }).to_string())
}

/// A run of the same program (its first event came from the same file) started after the
/// failure and recorded no error.
fn passed_later(e: &Envelope, runs: &[Value]) -> bool {
    let entry = runs
        .iter()
        .find(|r| r["run"] == e.run.as_str())
        .and_then(|r| r["file"].as_str().map(str::to_owned))
        .or_else(|| e.origin.file.clone());
    let Some(entry) = entry else { return false };
    runs.iter().any(|r| {
        r["run"] != e.run.as_str()
            && r["file"].as_str() == Some(entry.as_str())
            && r["errors"].as_u64() == Some(0)
            && r["first"].as_str().and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok()).is_some_and(|t| t > e.ts)
    })
}

/// The agent stops again after the Stop guard sent it back: did it change the code a failure came
/// from (fixed) or only answer (explained)? Recorded once per guard.
fn outcome(http: &ureq::Agent, base: &str, session: &str, project: &str) {
    let Some(dir) = state_file(session).and_then(|f| f.parent().map(std::path::Path::to_path_buf)) else { return };
    let file = dir.join(format!("{}.guard", safe_name(session)));
    let Ok(ids) = std::fs::read_to_string(&file) else { return };
    let _ = std::fs::remove_file(&file);
    let ids: Vec<String> = ids.lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_owned).collect();
    let fixed = ids.iter().any(|id| {
        http.get(&format!("{base}/v1/events/{}", enc(id)))
            .call()
            .ok()
            .and_then(|r| r.into_json::<Envelope>().ok())
            .is_some_and(|e| edited_since(&e).is_some())
    });
    let what = if fixed { "fixed" } else { "explained" };
    record(http, base, session, project, glint_core::agents::HOOK_STOP_OUTCOME, &ids, json!({ "outcome": what }));
}

/// Tells glintd what a hook did (`glint stats`, the Agents view). Best effort.
fn record(http: &ureq::Agent, base: &str, session: &str, project: &str, action: &str, ids: &[String], detail: Value) {
    let body = json!({ "agent": "claude-code", "session": session, "project": project, "action": action, "events": ids, "detail": detail });
    let _ = http.post(&format!("{base}/v1/agents/activity")).set("Content-Type", "application/json").send_string(&body.to_string());
}

/// Where a failure was thrown (first app frame), else where it was reported.
fn throw_site(e: &Envelope) -> (Option<String>, Option<u64>) {
    let frame = e.payload["frames"].as_array().and_then(|fs| fs.iter().find(|f| f["vendor"] != true && f["file"].is_string()));
    match frame {
        Some(f) => (f["file"].as_str().map(str::to_owned), f["line"].as_u64()),
        None => (e.origin.file.clone(), e.origin.line.map(u64::from)),
    }
}

/// The file the failure came from, when it was changed after the failure.
fn edited_since(e: &Envelope) -> Option<String> {
    let file = throw_site(e).0?;
    let changed = std::fs::metadata(&file).and_then(|m| m.modified()).ok()?;
    (chrono::DateTime::<chrono::Utc>::from(changed) > e.ts).then_some(file)
}

/// The newest few failures, one line each, with a note when there is one.
fn list(fails: &[(&Envelope, Option<String>)]) -> String {
    fails
        .iter()
        .rev()
        .take(SHOWN)
        .map(|(e, note)| {
            let p = &e.payload;
            let what = match (p["class"].as_str(), p["message"].as_str()) {
                (Some(c), Some(m)) => format!("{c}: {m}"),
                (None, Some(m)) => m.to_owned(),
                (Some(c), None) => c.to_owned(),
                _ => e.kind.clone(),
            };
            let (file, line) = throw_site(e);
            let at = file.map(|f| format!(" at {f}{}", line.map(|l| format!(":{l}")).unwrap_or_default())).unwrap_or_default();
            let note = note.as_deref().map(|n| format!("; {n}")).unwrap_or_default();
            format!("- {}{at} ({}, id {}{note})", what.chars().take(200).collect::<String>(), e.ts.format("%H:%M:%S"), e.id)
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

fn safe_name(session: &str) -> String {
    session.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').take(80).collect()
}

fn state_file(session: &str) -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".glint/hooks").join(format!("{}.last", safe_name(session))))
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
    // Claude Code runs it through a shell: a HOME with spaces must stay one word.
    let command = format!("{} hook", crate::agents::shell_quote(binary));
    let mut hooks = serde_json::Map::new();
    for e in events {
        let needs_matcher = matches!(e, "PreToolUse" | "PostToolUse" | "PostToolUseFailure" | "PermissionRequest");
        let entry = if needs_matcher {
            serde_json::json!([{ "matcher": "*", "hooks": [{ "type": "command", "command": command, "timeout": 5 }] }])
        } else {
            serde_json::json!([{ "hooks": [{ "type": "command", "command": command, "timeout": 5 }] }])
        };
        hooks.insert(e.into(), entry);
    }
    serde_json::to_string_pretty(&serde_json::json!({ "hooks": hooks })).unwrap_or_default()
}
