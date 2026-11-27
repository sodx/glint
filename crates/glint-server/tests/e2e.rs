//! End to end: a real listener, a real SQLite store, HTTP and WebSocket clients.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use glint_core::{Hub, HubConfig};
use glint_store_sqlite::SqliteStore;
use serde_json::{Value, json};
use tokio::net::TcpListener;

async fn start() -> SocketAddr {
    let store = SqliteStore::open_in_memory().unwrap();
    let (hub, _writer) = Hub::start(Arc::new(store), HubConfig::default());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(glint_server::serve(listener, hub, glint_server::Options::default(), std::future::pending()));
    addr
}

#[tokio::test]
async fn posted_events_stream_live_and_are_queryable() {
    let addr = start().await;
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/v1/stream?kind=core.*"))
        .await
        .unwrap();
    let http = reqwest::Client::new();

    let res = http
        .post(format!("http://{addr}/v1/events"))
        .json(&json!([
            {"kind": "core.value", "payload": {"value": 42}, "origin": {"file": "/app/x.php", "line": 12}},
            {"kind": "sql.query", "payload": {"sql": "select 1"}},
            {"kind": "not-a-kind", "payload": 1}
        ]))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 202);
    let ack: Value = res.json().await.unwrap();
    assert_eq!(ack["accepted"], 2);
    assert_eq!(ack["rejected"][0]["index"], 2);
    let id = ack["ids"][0].as_str().unwrap().to_owned();

    // The stream filter is core.*, so only the value arrives.
    let msg = tokio::time::timeout(Duration::from_secs(5), ws.next()).await.unwrap().unwrap().unwrap();
    let live: Value = serde_json::from_str(msg.to_text().unwrap()).unwrap();
    assert_eq!(live["id"], id.as_str());
    assert_eq!(live["payload"]["value"], 42);
    assert_eq!(live["origin"]["line"], 12);

    let list: Value = http
        .get(format!("http://{addr}/v1/events?kind=sql.*"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(list["events"].as_array().unwrap().len(), 1);
    assert_eq!(list["events"][0]["payload"]["sql"], "select 1");

    let one = http.get(format!("http://{addr}/v1/events/{id}")).send().await.unwrap();
    assert_eq!(one.status(), 200);
    let missing = http.get(format!("http://{addr}/v1/events/nope")).send().await.unwrap();
    assert_eq!(missing.status(), 404);
}

#[tokio::test]
async fn ndjson_is_accepted() {
    let addr = start().await;
    let res = reqwest::Client::new()
        .post(format!("http://{addr}/v1/events"))
        .header("content-type", "application/x-ndjson")
        .body("{\"kind\":\"core.log\",\"payload\":{\"message\":\"a\"}}\n{\"kind\":\"core.log\",\"payload\":{\"message\":\"b\"}}\n")
        .send()
        .await
        .unwrap();
    let ack: Value = res.json().await.unwrap();
    assert_eq!(ack["accepted"], 2);
    // Read-your-writes: a GET right after the POST sees the events.
    let list: Value = reqwest::get(format!("http://{addr}/v1/events?kind=core.log")).await.unwrap().json().await.unwrap();
    assert_eq!(list["events"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn bad_bodies_hosts_and_origins_are_refused() {
    let addr = start().await;
    let http = reqwest::Client::new();
    let url = format!("http://{addr}/v1/events");

    let bad = http.post(&url).body("not json").send().await.unwrap();
    assert_eq!(bad.status(), 400);

    let foreign = http
        .post(&url)
        .header("origin", "https://evil.example")
        .body(r#"{"kind":"core.value","payload":1}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(foreign.status(), 403);

    let same = http
        .post(&url)
        .header("origin", format!("http://{addr}"))
        .body(r#"{"kind":"core.value","payload":1}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(same.status(), 202);

    let rebound = http.get(&url).header("host", format!("evil.example:{}", addr.port())).send().await.unwrap();
    assert_eq!(rebound.status(), 403);
}

#[tokio::test]
async fn serves_ui_and_info() {
    let addr = start().await;
    let http = reqwest::Client::new();
    let page = http.get(format!("http://{addr}/")).send().await.unwrap().text().await.unwrap();
    assert!(page.contains("<title>Glint</title>"));
    let info: Value = http.get(format!("http://{addr}/v1/info")).send().await.unwrap().json().await.unwrap();
    assert_eq!(info["protocol"][0], 1);
}

#[tokio::test]
async fn native_pause_long_poll_and_resume() {
    let addr = start().await;
    let http = reqwest::Client::new();
    http.post(format!("http://{addr}/v1/events"))
        .json(&json!({"id": "pz", "kind": "ctl.pause", "payload": {"reason": "inspect"}}))
        .send()
        .await
        .unwrap();
    let still = http.get(format!("http://{addr}/v1/ctl/pz?timeout_s=1")).send().await.unwrap();
    assert_eq!(still.status(), 204);

    let waiter = {
        let http = http.clone();
        tokio::spawn(async move { http.get(format!("http://{addr}/v1/ctl/pz?timeout_s=10")).send().await.unwrap() })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    let res = http.post(format!("http://{addr}/v1/ctl/pz/resume")).json(&json!({"value": {"ok": true}, "by": "ui"})).send().await.unwrap();
    assert_eq!(res.status(), 200);
    let answer: Value = waiter.await.unwrap().json().await.unwrap();
    assert_eq!(answer, json!({"action": "continue", "value": {"ok": true}}));

    let again = http.post(format!("http://{addr}/v1/ctl/pz/resume")).send().await.unwrap();
    assert_eq!(again.status(), 409);
    let unknown = http.get(format!("http://{addr}/v1/ctl/nope?timeout_s=1")).send().await.unwrap();
    assert_eq!(unknown.status(), 404);
}

#[tokio::test]
async fn mcp_over_http() {
    let addr = start().await;
    let http = reqwest::Client::new();
    let url = format!("http://{addr}/mcp");
    let init = http
        .post(&url)
        .header("accept", "application/json, text/event-stream")
        .json(&json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "e2e", "version": "0"}}}))
        .send()
        .await
        .unwrap();
    assert_eq!(init.status(), 200);
    assert!(init.headers().contains_key("mcp-session-id"));
    let body: Value = init.json().await.unwrap();
    assert_eq!(body["result"]["serverInfo"]["name"], "glint");

    let note = http.post(&url).json(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"})).send().await.unwrap();
    assert_eq!(note.status(), 202);

    http.post(format!("http://{addr}/v1/events")).json(&json!({"kind": "core.log", "payload": {"message": "seen by agent"}})).send().await.unwrap();
    let res: Value = http
        .post(&url)
        .json(&json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "glint_events", "arguments": {}}}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(res["result"]["content"][0]["text"].as_str().unwrap().contains("seen by agent"));

    let bad = http.post(&url).body("{nope").send().await.unwrap();
    assert_eq!(bad.status(), 400);
    let foreign = http.post(&url).header("origin", "https://evil.example").json(&json!({"jsonrpc": "2.0", "id": 3, "method": "ping"})).send().await.unwrap();
    assert_eq!(foreign.status(), 403);
}

/// An HTTP client that names no project: the first tool call asks for its workspace roots over
/// SSE, and the answer scopes that call and later ones. `/mcp?project=` scopes without asking.
#[tokio::test]
async fn mcp_scopes_http_clients_by_workspace_root() {
    let addr = start().await;
    let http = reqwest::Client::new();
    let url = format!("http://{addr}/mcp");
    for (project, class) in [("alpha", "AlphaError"), ("beta", "BetaError")] {
        http.post(format!("http://{addr}/v1/events"))
            .json(&json!({"kind": "core.exception", "project": project, "payload": {"class": class, "message": "x"}}))
            .send()
            .await
            .unwrap();
    }
    let call = json!({"jsonrpc": "2.0", "id": 5, "method": "tools/call", "params": {"name": "glint_last_error", "arguments": {}}});
    let dir = std::env::temp_dir().join(format!("glint-e2e-roots-{}", std::process::id())).join("beta");
    std::fs::create_dir_all(&dir).unwrap();

    let sid = "glint-e2e-claude-code";
    let mut res = http
        .post(&url)
        .header("accept", "application/json, text/event-stream")
        .header("mcp-session-id", sid)
        .json(&call)
        .send()
        .await
        .unwrap();
    assert!(res.headers()["content-type"].to_str().unwrap().starts_with("text/event-stream"));
    let mut text = String::new();
    let mut answered = false;
    while let Some(chunk) = tokio::time::timeout(Duration::from_secs(5), res.chunk()).await.unwrap().unwrap() {
        text.push_str(&String::from_utf8_lossy(&chunk));
        if !answered && let Some(line) = text.lines().find(|l| l.starts_with("data:") && l.contains("roots/list")) {
            let ask: Value = serde_json::from_str(line.trim_start_matches("data:").trim()).unwrap();
            let answer = json!({"jsonrpc": "2.0", "id": ask["id"], "result": {"roots": [{"uri": format!("file://{}", dir.display())}]}});
            let r = http.post(&url).header("mcp-session-id", sid).json(&answer).send().await.unwrap();
            assert_eq!(r.status(), 202);
            answered = true;
        }
    }
    assert!(answered, "{text}");
    let reply = text.lines().rfind(|l| l.starts_with("data:")).unwrap();
    let reply: Value = serde_json::from_str(reply.trim_start_matches("data:").trim()).unwrap();
    let t = reply["result"]["content"][0]["text"].as_str().unwrap();
    assert!(t.starts_with("Project: beta (the agent's workspace") && t.contains("BetaError") && !t.contains("AlphaError"), "{t}");

    // Later calls of that session are plain JSON, still scoped.
    let res: Value = http.post(&url).header("accept", "application/json, text/event-stream").header("mcp-session-id", sid).json(&call).send().await.unwrap().json().await.unwrap();
    assert!(res["result"]["content"][0]["text"].as_str().unwrap().contains("BetaError"));

    // A per-project URL needs no roots.
    let res: Value = http.post(format!("{url}?project=alpha")).header("mcp-session-id", "other").json(&call).send().await.unwrap().json().await.unwrap();
    let t = res["result"]["content"][0]["text"].as_str().unwrap();
    assert!(t.starts_with("Project: alpha (named by the connection)") && t.contains("AlphaError"), "{t}");
    std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
}

#[tokio::test]
async fn claude_code_hooks_become_agent_events() {
    let addr = start().await;
    let http = reqwest::Client::new();
    let url = format!("http://{addr}/v1/hooks/claude-code");
    let common = json!({"session_id": "S9", "transcript_path": "/t.jsonl", "cwd": "/Users/me/src/shop"});
    let with = |extra: Value| {
        let mut v = common.clone();
        for (k, x) in extra.as_object().unwrap() {
            v[k] = x.clone();
        }
        v
    };
    for body in [
        with(json!({"hook_event_name": "UserPromptSubmit", "prompt": "run the tests"})),
        with(json!({"hook_event_name": "PreToolUse", "tool_name": "Bash", "tool_input": {"command": "cargo test"}, "tool_use_id": "tu1"})),
        with(json!({"hook_event_name": "PostToolUse", "tool_name": "Bash", "tool_input": {"command": "cargo test"}, "tool_use_id": "tu1", "tool_response": {"stdout": "ok", "stderr": "", "interrupted": false}})),
    ] {
        let res = http.post(&url).json(&body).send().await.unwrap();
        assert_eq!(res.status(), 200);
    }
    let got: Value = http.get(format!("http://{addr}/v1/events?run=claude-code:S9")).send().await.unwrap().json().await.unwrap();
    let kinds: Vec<_> = got["events"].as_array().unwrap().iter().map(|e| e["kind"].as_str().unwrap().to_owned()).collect();
    assert_eq!(kinds, ["agent.step", "agent.tool_call", "agent.tool_result", "core.update"]);
    assert_eq!(got["events"][2]["parent"], "cc-S9-tu1");
    assert_eq!(got["events"][0]["project"], "shop");

    assert_eq!(http.post(format!("http://{addr}/v1/hooks/cursor")).json(&common).send().await.unwrap().status(), 404);
    assert_eq!(http.post(&url).body("nope").send().await.unwrap().status(), 400);
}

#[tokio::test]
async fn runs_are_listed_newest_first() {
    let addr = start().await;
    let http = reqwest::Client::new();
    http.post(format!("http://{addr}/v1/events"))
        .json(&json!([
            {"kind": "core.log", "payload": {"message": "a"}, "run": "r1", "project": "shop"},
            {"kind": "core.exception", "payload": {"class": "E", "message": "boom"}, "run": "r2", "project": "shop"},
            {"kind": "core.log", "payload": {"message": "b"}, "run": "r3", "project": "blog"}
        ]))
        .send()
        .await
        .unwrap();
    let all: Value = http.get(format!("http://{addr}/v1/runs")).send().await.unwrap().json().await.unwrap();
    let ids: Vec<_> = all["runs"].as_array().unwrap().iter().map(|r| r["run"].as_str().unwrap().to_owned()).collect();
    assert_eq!(ids, ["r3", "r2", "r1"]);
    let shop: Value = http.get(format!("http://{addr}/v1/runs?project=shop")).send().await.unwrap().json().await.unwrap();
    assert_eq!(shop["runs"][0]["errors"], 1);
    assert_eq!(shop["runs"].as_array().unwrap().len(), 2);
}
