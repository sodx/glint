use std::sync::Arc;
use std::time::Duration;

use glint_core::{Hub, HubConfig, PauseState, Resolution};
use glint_protocol::{BodyFormat, parse_body};
use glint_store_sqlite::SqliteStore;
use serde_json::{Value, json};

use super::*;

async fn server() -> (McpServer, Hub) {
    let (hub, _w) = Hub::start(Arc::new(SqliteStore::open_in_memory().unwrap()), HubConfig::default());
    (McpServer::new(hub.clone()), hub)
}

async fn put(hub: &Hub, body: &str) {
    let inc = parse_body(body.as_bytes(), BodyFormat::Json).unwrap().into_iter().map(Result::unwrap).collect();
    let events = hub.normalise(inc);
    hub.submit(events).await.unwrap();
}

async fn call(s: &McpServer, name: &str, args: Value) -> (String, bool) {
    let r = s
        .handle(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": name, "arguments": args}}))
        .await
        .unwrap();
    let res = &r["result"];
    (res["content"][0]["text"].as_str().unwrap().to_owned(), res["isError"] == true)
}

#[tokio::test]
async fn handshake_and_listing() {
    let (s, _) = server().await;
    let r = s
        .handle(json!({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {"protocolVersion": "2025-03-26", "capabilities": {}, "clientInfo": {"name": "t", "version": "1"}}}))
        .await
        .unwrap();
    assert_eq!(r["result"]["protocolVersion"], "2025-03-26");
    assert_eq!(r["result"]["serverInfo"]["name"], "glint");
    let r = s.handle(json!({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {"protocolVersion": "1999-01-01"}})).await.unwrap();
    assert_eq!(r["result"]["protocolVersion"], PROTOCOL_VERSIONS[0]);

    assert!(s.handle(json!({"jsonrpc": "2.0", "method": "notifications/initialized"})).await.is_none());
    let tools = s.handle(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})).await.unwrap();
    let names: Vec<_> = tools["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names.len(), 10);
    assert!(names.contains(&"glint_last_error") && names.contains(&"glint_resume"));
    for t in tools["result"]["tools"].as_array().unwrap() {
        assert_eq!(t["inputSchema"]["type"], "object", "{}", t["name"]);
    }

    let err = s.handle(json!({"jsonrpc": "2.0", "id": 3, "method": "nope"})).await.unwrap();
    assert_eq!(err["error"]["code"], -32601);
    let batch = s.handle(json!([{"jsonrpc": "2.0", "id": 4, "method": "ping"}, {"jsonrpc": "2.0", "method": "notifications/x"}])).await.unwrap();
    assert_eq!(batch.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn events_runs_and_last_error() {
    let (s, hub) = server().await;
    put(&hub, r#"[
        {"kind":"core.log","payload":{"message":"GET /cart"},"run":"req-1","project":"shop"},
        {"kind":"sql.query","payload":{"sql":"select * from carts where id = ?","duration_ms":2.1},"run":"req-1","project":"shop"},
        {"kind":"core.exception","payload":{"class":"TypeError","message":"total is undefined",
          "frames":[{"file":"/srv/shop/src/cart.ts","line":88,"fn":"renderTotal"},{"file":"/srv/shop/node_modules/x/i.js","line":1,"vendor":true}]},
         "run":"req-1","project":"shop","origin":{"file":"/srv/shop/src/cart.ts","line":88}},
        {"kind":"core.log","payload":{"message":"other"},"run":"req-2","project":"blog"}
    ]"#)
    .await;

    let (text, err) = call(&s, "glint_events", json!({"project": "shop"})).await;
    assert!(!err);
    assert_eq!(text.lines().filter(|l| l.contains(" core.") || l.contains(" sql.")).count(), 3);
    assert!(text.contains("select * from carts where id = ? (2.1 ms)"));
    assert!(text.contains("newest id:"));

    let (text, _) = call(&s, "glint_events", json!({"kind": "core.exception"})).await;
    assert!(text.contains("TypeError: total is undefined @ renderTotal"));

    let (text, _) = call(&s, "glint_runs", json!({})).await;
    let rows: Vec<_> = text.lines().skip(1).collect();
    assert!(rows[0].starts_with("req-2 · blog"));
    assert!(rows[1].starts_with("req-1 · shop") && rows[1].contains(" · 3 · 1 · "));

    let (text, _) = call(&s, "glint_last_error", json!({})).await;
    assert!(text.contains("TypeError: total is undefined"));
    assert!(text.contains("at renderTotal (…/shop/src/cart.ts:88)"));
    assert!(text.contains("[vendor]"));
    assert!(text.contains("What happened before it in this run (2 events)"));
    assert!(text.contains("GET /cart"));
    assert!(!text.contains("other"), "context must stay within the run");

    let id = text.split_whitespace().nth(2).unwrap().to_owned();
    let (full, _) = call(&s, "glint_event", json!({"id": id, "max_chars": 200})).await;
    assert!(full.contains("truncated at 200 chars"));
    let (_, err) = call(&s, "glint_event", json!({"id": "missing"})).await;
    assert!(err);
}

#[tokio::test]
async fn wait_for_sees_only_new_matching_events() {
    let (s, hub) = server().await;
    put(&hub, r#"{"kind":"core.log","payload":{"message":"old"}}"#).await;
    let h = hub.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        put(&h, r#"[{"kind":"core.log","payload":{"message":"noise"}},{"kind":"core.value","payload":{"value":"fresh"}}]"#).await;
    });
    let (text, err) = call(&s, "glint_wait_for", json!({"kind": "core.value", "timeout_s": 5})).await;
    assert!(!err);
    assert!(text.contains("fresh") && !text.contains("old"));

    let (text, _) = call(&s, "glint_wait_for", json!({"kind": "core.value", "timeout_s": 1})).await;
    assert_eq!(text, "No matching event within 1 s.");
}

#[tokio::test]
async fn pause_resume_through_tools() {
    let (s, hub) = server().await;
    put(&hub, r#"{"id":"P1","kind":"ctl.pause","payload":{"reason":"check the cart"},"run":"r","origin":{"file":"/a.php","line":3}}"#).await;
    let (text, _) = call(&s, "glint_pauses", json!({})).await;
    assert!(text.contains("P1 · ") && text.contains("check the cart") && text.contains("/a.php:3"));

    let (text, err) = call(&s, "glint_resume", json!({"id": "P1", "value": {"discount": 5}})).await;
    assert!(!err, "{text}");
    assert_eq!(hub.control().state("P1"), PauseState::Resolved(Resolution::Continue { value: Some(json!({"discount": 5})) }));
    let (text, err) = call(&s, "glint_resume", json!({"id": "P1"})).await;
    assert!(err && text.contains("already resolved"));
    assert_eq!(call(&s, "glint_pauses", json!({})).await.0, "No program is paused.");
}

#[tokio::test]
async fn annotate_show_clear() {
    let (s, hub) = server().await;
    put(&hub, r#"{"id":"E1","kind":"core.log","payload":{"message":"boom"},"project":"shop"}"#).await;
    let (_, err) = call(&s, "glint_annotate", json!({"id": "E1", "note": "root cause: null total", "color": "red"})).await;
    assert!(!err);
    let (_, err) = call(&s, "glint_annotate", json!({"id": "nope", "note": "x"})).await;
    assert!(err);
    let (text, _) = call(&s, "glint_show", json!({"format": "mermaid", "content": "graph TD; A-->B", "label": "flow"})).await;
    assert!(text.starts_with("Shown as "));
    let (_, err) = call(&s, "glint_show", json!({"format": "json", "content": "{bad"})).await;
    assert!(err);
    let (text, _) = call(&s, "glint_clear", json!({})).await;
    let cursor = text.split("after=").nth(1).unwrap().split_whitespace().next().unwrap().to_owned();
    put(&hub, r#"{"kind":"core.log","payload":{"message":"after clear"}}"#).await;
    let (text, _) = call(&s, "glint_events", json!({"after": cursor})).await;
    assert!(text.contains("after clear") && !text.contains("boom"));

    let all = hub.query(glint_core::Query::default()).await.unwrap();
    let upd = all.iter().find(|e| e.kind == "core.update").unwrap();
    assert_eq!(upd.payload, json!({"target": "E1", "meta": {"note": "root cause: null total", "color": "red"}}));
    assert_eq!(upd.origin.lang.as_deref(), Some("agent"));
    assert!(all.iter().any(|e| e.kind == "core.mermaid" && e.meta.label.as_deref() == Some("flow")));
}
