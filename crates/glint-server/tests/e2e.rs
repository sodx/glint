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

/// Glint's own clients (the window, the CLI) send `X-Glint: 1`.
fn client() -> reqwest::Client {
    let mut h = reqwest::header::HeaderMap::new();
    h.insert("x-glint", "1".parse().unwrap());
    reqwest::Client::builder().default_headers(h).build().unwrap()
}

#[tokio::test]
async fn posted_events_stream_live_and_are_queryable() {
    let addr = start().await;
    let mut ws_req = tokio_tungstenite::tungstenite::client::IntoClientRequest::into_client_request(format!("ws://{addr}/v1/stream?kind=core.*")).unwrap();
    ws_req.headers_mut().insert("x-glint", "1".parse().unwrap());
    let (mut ws, _) = tokio_tungstenite::connect_async(ws_req)
        .await
        .unwrap();
    let http = client();

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
    let res = client()
        .post(format!("http://{addr}/v1/events"))
        .header("content-type", "application/x-ndjson")
        .body("{\"kind\":\"core.log\",\"payload\":{\"message\":\"a\"}}\n{\"kind\":\"core.log\",\"payload\":{\"message\":\"b\"}}\n")
        .send()
        .await
        .unwrap();
    let ack: Value = res.json().await.unwrap();
    assert_eq!(ack["accepted"], 2);
    // Read-your-writes: a GET right after the POST sees the events.
    let list: Value = client().get(format!("http://{addr}/v1/events?kind=core.log")).send().await.unwrap().json().await.unwrap();
    assert_eq!(list["events"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn bad_bodies_hosts_and_origins_are_refused() {
    let addr = start().await;
    let http = client();
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
    let http = client();
    let page = http.get(format!("http://{addr}/")).send().await.unwrap().text().await.unwrap();
    assert!(page.contains("<title>Glint</title>"));
    let info: Value = http.get(format!("http://{addr}/v1/info")).send().await.unwrap().json().await.unwrap();
    assert_eq!(info["protocol"][0], 1);
}

#[tokio::test]
async fn native_pause_long_poll_and_resume() {
    let addr = start().await;
    let http = client();
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
    let http = client();
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

#[tokio::test]
async fn claude_code_hooks_become_agent_events() {
    let addr = start().await;
    let http = client();
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
    let http = client();
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

#[tokio::test]
async fn only_glints_own_clients_read_or_change_anything() {
    let addr = start().await;
    let bare = reqwest::Client::new();
    // Programs send events and wait for their pause with no header.
    let sent = bare.post(format!("http://{addr}/v1/events")).json(&json!({"kind": "core.value", "payload": 1})).send().await.unwrap();
    assert_eq!(sent.status(), 202);
    assert_eq!(bare.get(format!("http://{addr}/")).send().await.unwrap().status(), 200);
    // Reading and changing need the header.
    for (method, path) in [("GET", "/v1/events"), ("GET", "/v1/info"), ("GET", "/v1/setup"), ("POST", "/v1/data/clear"), ("GET", "/v1/ctl")] {
        let req = bare.request(method.parse().unwrap(), format!("http://{addr}{path}"));
        let res = req.send().await.unwrap();
        assert_eq!(res.status(), 403, "{method} {path}");
        assert!(res.text().await.unwrap().contains("X-Glint"), "{method} {path}");
    }
    assert_eq!(client().get(format!("http://{addr}/v1/events")).send().await.unwrap().status(), 200);
    // A page's `<img src=".../v1/setup">` (no Origin, Sec-Fetch-Site: cross-site) is refused,
    // also when it is sent to a route that would otherwise take the header.
    for site in ["cross-site", "same-site"] {
        let img = bare.get(format!("http://{addr}/v1/setup")).header("sec-fetch-site", site).header("sec-fetch-mode", "no-cors").send().await.unwrap();
        assert_eq!(img.status(), 403, "{site}");
        assert!(img.text().await.unwrap().contains("another site"));
    }
    // A no-cors POST from another origin carries that Origin and is refused.
    let form = bare.post(format!("http://{addr}/v1/setup")).header("origin", "https://evil.example").header("sec-fetch-site", "cross-site").send().await.unwrap();
    assert_eq!(form.status(), 403);
    let mcp = bare.post(format!("http://{addr}/mcp")).header("origin", "http://localhost:5173").json(&json!({"jsonrpc": "2.0", "id": 1, "method": "ping"})).send().await.unwrap();
    assert_eq!(mcp.status(), 403);
    // Coding agents reach /mcp without the header.
    let ping = bare.post(format!("http://{addr}/mcp")).json(&json!({"jsonrpc": "2.0", "id": 1, "method": "ping"})).send().await.unwrap();
    assert_eq!(ping.status(), 200);
}
