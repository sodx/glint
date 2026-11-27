//! Glint's MCP server. Transport-agnostic: [`McpServer::handle`] takes one JSON-RPC message
//! (or a batch) and returns the reply; `glint-server` serves it over Streamable HTTP at
//! `/mcp`, and `glint mcp` bridges stdio to that endpoint.
//!
//! Implemented by hand rather than with an SDK crate: the surface Glint needs (initialize,
//! ping, tools) is small and stable, and keeping it here avoids churn from a fast-moving
//! dependency. See `docs/adr/0004-mcp-by-hand.md`.

mod project;
mod tools;

use glint_core::Hub;
use serde_json::{Value, json};

pub use project::{line, summary};

/// Protocol revisions this server speaks, newest first.
pub const PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

const INSTRUCTIONS: &str = "Glint records what programs on this machine actually did: dumped values, logs, \
SQL queries, exceptions, timings, LLM calls. Use it instead of guessing or adding print statements:\n\
- glint_last_error first when something failed; glint_events / glint_runs to look around.\n\
- To inspect a value, add glint(value) to the code instead of print/var_dump/console.log: after \
`glint install` it is a global in PHP, Python and Node (no import; off in production), then run \
and read it back with glint_wait_for or glint_events. Remove those calls when done.\n\
- Edit → run → glint_wait_for to see the result of a change without reading terminal output.\n\
- glint_pauses / glint_resume when a program is paused waiting for an answer.\n\
- glint_annotate to leave a note the human sees next to an event; glint_show to put markdown or a \
mermaid diagram in front of them.\n\
Ids are opaque; pass them back as given. Output is compact by design: fetch one event with glint_event \
when you need its full payload.";

#[derive(Clone)]
pub struct McpServer {
    hub: Hub,
}

impl McpServer {
    pub fn new(hub: Hub) -> Self {
        Self { hub }
    }

    /// Handles a JSON-RPC request, notification or batch. `None` means nothing to send back
    /// (notifications only).
    pub async fn handle(&self, msg: Value) -> Option<Value> {
        match msg {
            Value::Array(items) => {
                let mut out = Vec::new();
                for item in items {
                    if let Some(r) = Box::pin(self.handle(item)).await {
                        out.push(r);
                    }
                }
                (!out.is_empty()).then_some(Value::Array(out))
            }
            Value::Object(_) => self.one(msg).await,
            _ => Some(error(Value::Null, -32600, "invalid request")),
        }
    }

    async fn one(&self, msg: Value) -> Option<Value> {
        let id = msg.get("id").cloned();
        let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        // Notifications (no id) never get a reply, not even an error.
        let id = id?;
        let result = match method {
            "initialize" => Ok(initialize(&params)),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": tools::definitions() })),
            "tools/call" => {
                let name = params.get("name").and_then(Value::as_str).unwrap_or("");
                let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
                Ok(match tools::call(&self.hub, name, args).await {
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
        Some(match result {
            Ok(r) => json!({ "jsonrpc": "2.0", "id": id, "result": r }),
            Err((code, m)) => error(id, code, &m),
        })
    }
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
