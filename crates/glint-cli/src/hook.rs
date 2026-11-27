//! `glint hook`: the command a coding agent runs as a hook. Forwards the hook's stdin JSON to
//! glintd and gets out of the way: no output (Claude Code reads a hook's stdout), always exit
//! 0, and at most ~1 s even when Glint is down.

use std::io::Read;
use std::time::Duration;

pub fn run(base: &str, agent: &str) -> Result<(), String> {
    let mut body = Vec::new();
    if std::io::stdin().read_to_end(&mut body).is_err() || body.is_empty() {
        return Ok(());
    }
    let url = format!("{}/v1/hooks/{agent}", base.trim_end_matches('/'));
    let http = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_millis(300))
        .timeout(Duration::from_secs(1))
        .build();
    // Errors are deliberately ignored: a debugging aid must never block the agent.
    let _ = http.post(&url).set("Content-Type", "application/json").send_bytes(&body);
    Ok(())
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
