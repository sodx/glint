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
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line.map_err(|e| e.to_string())?;
        if line.trim().is_empty() {
            continue;
        }
        let reply = match agent.post(&url).set("Content-Type", "application/json").send_string(&line) {
            Ok(res) if res.status() == 202 => None,
            Ok(res) => res.into_string().ok().filter(|s| !s.trim().is_empty()),
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
