//! Glint's MCP server. Transport-agnostic: [`McpServer::handle`] takes one JSON-RPC message
//! (or a batch) and returns the reply; `glint-server` serves it over Streamable HTTP at
//! `/mcp`, and `glint mcp` bridges stdio to that endpoint.
//!
//! Implemented by hand rather than with an SDK crate: the surface Glint needs (initialize,
//! ping, tools) is small and stable, and keeping it here avoids churn from a fast-moving
//! dependency. See `docs/adr/0004-mcp-by-hand.md`.

mod project;
mod tools;

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use glint_core::Hub;
use glint_core::agents::{self, Activity};
use serde_json::{Value, json};

pub use project::{line, summary};

/// Protocol revisions this server speaks, newest first.
pub const PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

const INSTRUCTIONS: &str = "Glint records what programs on this machine actually did: dumped values, logs, \
SQL queries, exceptions, timings, LLM calls. Use it instead of guessing or adding print statements:\n\
- glint_last_error first when something failed; glint_events / glint_runs to look around.\n\
- To inspect a value, add glint(value) to the code instead of print/var_dump/console.log: after \
`glint install` it is a global in PHP, Python and Node (no import; off in production), then run \
and read it back with glint_wait_for or glint_events. Remove those calls when done. Uncaught \
errors of PHP, Python and Node processes are recorded without any code.\n\
- Web front end in development: add <script src=\"http://127.0.0.1:23600/v1/browser.js\" \
data-project=\"<project>\"></script> to the dev page (not production builds) and the browser's \
errors, console.error, failed requests and window.glint(value) markers land here too.\n\
- To check that a change works, place glint() markers on the path that should run, run it (or \
ask the user to click through it), then confirm with glint_wait_for that the markers arrived with \
the expected values and nothing failed.\n\
- Edit → run → glint_wait_for to see the result of a change without reading terminal output.\n\
- glint_pauses / glint_resume when a program is paused waiting for an answer.\n\
- glint_annotate to leave a note the human sees next to an event; glint_show to put markdown or a \
mermaid diagram in front of them.\n\
Ids are opaque; pass them back as given. Output is compact by design: fetch one event with glint_event \
when you need its full payload.";

#[derive(Clone)]
pub struct McpServer {
    hub: Hub,
    sessions: Arc<AtomicU64>,
}

/// Who is calling, as far as the transport knows.
#[derive(Debug, Clone, Default)]
pub struct Caller {
    /// The `Mcp-Session-Id` the client sent back (minted by [`McpServer::handle_from`] at
    /// `initialize`; it carries the client's name, so it survives a glintd restart).
    pub session: Option<String>,
    /// The project the client works in, when the transport knows it (`glint mcp` sends its
    /// working directory's project).
    pub project: Option<String>,
}

/// What a message did besides its reply.
#[derive(Debug, Default)]
pub struct Outcome {
    pub reply: Option<Value>,
    /// The session id to hand back after an `initialize`.
    pub session: Option<String>,
}

impl McpServer {
    pub fn new(hub: Hub) -> Self {
        Self { hub, sessions: Arc::new(AtomicU64::new(0)) }
    }

    /// Handles a JSON-RPC request, notification or batch. `None` means nothing to send back
    /// (notifications only).
    pub async fn handle(&self, msg: Value) -> Option<Value> {
        self.handle_from(msg, &Caller::default()).await.reply
    }

    /// Like [`Self::handle`], recording tool calls in the agent activity log (see
    /// `glint_core::agents`).
    pub async fn handle_from(&self, msg: Value, caller: &Caller) -> Outcome {
        match msg {
            Value::Array(items) => {
                let mut out = Vec::new();
                let mut session = None;
                for item in items {
                    let o = Box::pin(self.handle_from(item, caller)).await;
                    session = session.or(o.session);
                    if let Some(r) = o.reply {
                        out.push(r);
                    }
                }
                Outcome { reply: (!out.is_empty()).then_some(Value::Array(out)), session }
            }
            Value::Object(_) => self.one(msg, caller).await,
            _ => Outcome { reply: Some(error(Value::Null, -32600, "invalid request")), session: None },
        }
    }

    async fn one(&self, msg: Value, caller: &Caller) -> Outcome {
        let id = msg.get("id").cloned();
        let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        // Notifications (no id) never get a reply, not even an error.
        let Some(id) = id else { return Outcome::default() };
        let mut session = None;
        let result = match method {
            "initialize" => {
                let client = params["clientInfo"]["name"].as_str().unwrap_or("");
                let sid = self.mint_session(client);
                let mut a = Activity::new(client_of(Some(&sid)), agents::MCP_CONNECT);
                a.session = Some(sid.clone());
                a.project.clone_from(&caller.project);
                a.detail = json!({ "version": params["clientInfo"]["version"], "protocol": params["protocolVersion"] });
                self.hub.record_activity(vec![a]).await;
                session = Some(sid);
                Ok(initialize(&params))
            }
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": tools::definitions() })),
            "tools/call" => {
                let name = params.get("name").and_then(Value::as_str).unwrap_or("");
                let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
                let started = Instant::now();
                let answer = tools::call(&self.hub, name, args.clone()).await;
                let mut a = Activity::new(client_of(caller.session.as_deref()), agents::MCP_CALL);
                a.duration_ms = u64::try_from(started.elapsed().as_millis()).ok();
                a.session.clone_from(&caller.session);
                a.project = args["project"].as_str().map(str::to_owned).filter(|p| !p.is_empty()).or_else(|| caller.project.clone());
                a.tool = Some(name.to_owned());
                let (Ok(text) | Err(text)) = &answer;
                a.result_bytes = Some(text.len() as u64);
                a.is_error = answer.is_err();
                // What the agent saw: the ids in the answer, and the one it asked about. Tools
                // that only write name the event they made, which nobody needed to read.
                if !matches!(name, "glint_show" | "glint_clear") {
                    let mut ids = Vec::new();
                    if let Some(i) = args["id"].as_str() {
                        ids.push(i.to_owned());
                    }
                    ids.extend(ids_in(text));
                    a.events = ids;
                }
                self.hub.record_activity(vec![a]).await;
                Ok(match answer {
                    Ok(text) => json!({ "content": [{ "type": "text", "text": text }] }),
                    Err(text) => json!({ "content": [{ "type": "text", "text": text }], "isError": true }),
                })
            }
            // Some clients probe these even when not advertised; empty is friendlier than an error.
            "resources/list" => Ok(json!({ "resources": [] })),
            "resources/templates/list" => Ok(json!({ "resourceTemplates": [] })),
            "prompts/list" => Ok(json!({ "prompts": [] })),
            other => Err((-32601, format!("method not found: {other}"))),
        };
        let reply = match result {
            Ok(r) => json!({ "jsonrpc": "2.0", "id": id, "result": r }),
            Err((code, m)) => error(id, code, &m),
        };
        Outcome { reply: Some(reply), session }
    }

    /// `glint-<n>-<client>`: unique per connection and naming the client, so calls made after a
    /// glintd restart (the client keeps its session id) are still attributed.
    fn mint_session(&self, client: &str) -> String {
        let n = self.sessions.fetch_add(1, Ordering::Relaxed);
        let safe: String = client.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')).take(60).collect();
        format!("glint-{:x}{n:x}-{safe}", std::process::id())
    }
}

/// The client name a session id carries; `unknown` without one.
fn client_of(session: Option<&str>) -> String {
    session
        .and_then(|s| s.strip_prefix("glint-"))
        .and_then(|s| s.split_once('-'))
        .map(|(_, name)| name)
        .filter(|n| !n.is_empty())
        .unwrap_or("unknown")
        .to_owned()
}

/// Event ids (ULIDs) in a tool's answer, in order, once each.
fn ids_in(text: &str) -> Vec<String> {
    const ULID: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let mut out: Vec<String> = Vec::new();
    for word in text.split(|c: char| !c.is_ascii_alphanumeric()) {
        if word.len() == 26 && word.bytes().all(|b| ULID.contains(&b)) && word.as_bytes()[0] <= b'7' && !out.iter().any(|o| o == word) {
            out.push(word.to_owned());
            if out.len() == agents::EVENTS_MAX {
                break;
            }
        }
    }
    out
}

fn initialize(params: &Value) -> Value {
    let asked = params.get("protocolVersion").and_then(Value::as_str).unwrap_or("");
    let version = PROTOCOL_VERSIONS.iter().find(|v| **v == asked).copied().unwrap_or(PROTOCOL_VERSIONS[0]);
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": { "name": "glint", "title": "Glint", "version": env!("CARGO_PKG_VERSION") },
        "instructions": INSTRUCTIONS,
    })
}

pub fn error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

#[cfg(test)]
mod tests;
