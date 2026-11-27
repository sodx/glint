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
    tokio::spawn(glint_server::serve(listener, hub, std::future::pending()));
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
