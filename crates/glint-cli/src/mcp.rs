//! `glint mcp`: an MCP server on stdio for agents that launch servers as processes. Each
//! JSON-RPC line is forwarded to the daemon's `/mcp` endpoint and the reply printed back.

use std::io::{BufRead, Write};
use std::time::Duration;

use serde_json::{Value, json};

pub fn run(base: &str) -> Result<(), String> {
    let url = format!("{}/mcp", base.trim_end_matches('/'));
    // glint_wait_for may hold a request for up to 120 s.
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(1))
        .timeout(Duration::from_secs(150))
        .build();
    // The agent's project (where it launched us) and its session, so Glint can tell which agent
    // looked at what (`glint stats`).
    let project = std::env::current_dir().ok().and_then(|d| {
        let cwd = d.display().to_string();
        glint_core::project::resolve(&cwd).map(|r| r.project).or_else(|| glint_core::project::folder_name(&cwd))
    })
    .filter(|p| p.is_ascii() && !p.chars().any(char::is_control));
    let mut session: Option<String> = None;
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line.map_err(|e| e.to_string())?;
        if line.trim().is_empty() {
            continue;
        }
        let mut req = agent.post(&url).set("Content-Type", "application/json");
        if let Some(p) = &project {
            req = req.set("X-Glint-Project", p);
        }
        if let Some(s) = &session {
            req = req.set("Mcp-Session-Id", s);
        }
        let reply = match req.send_string(&line) {
            Ok(res) if res.status() == 202 => None,
            Ok(res) => {
                if let Some(s) = res.header("mcp-session-id") {
                    session = Some(s.to_owned());
                }
                res.into_string().ok().filter(|s| !s.trim().is_empty())
            }
            Err(ureq::Error::Status(_, res)) => res.into_string().ok().filter(|s| !s.trim().is_empty()),
            Err(e) => unreachable_reply(&line, base, &e.to_string()),
        };
        if let Some(r) = reply {
            stdout.write_all(r.trim_end().as_bytes()).map_err(|e| e.to_string())?;
            stdout.write_all(b"\n").map_err(|e| e.to_string())?;
            stdout.flush().map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// Requests (not notifications) get a JSON-RPC error that tells the agent what to do.
fn unreachable_reply(line: &str, base: &str, err: &str) -> Option<String> {
    let msg: Value = serde_json::from_str(line).ok()?;
    let id = msg.get("id")?.clone();
    Some(
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": -32000, "message": format!("Glint is not running at {base} ({err}). Start it with `glintd`.") }
        })
        .to_string(),
    )
}
