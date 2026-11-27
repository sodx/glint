//! Runs the real `glint` binary against a real server on a random port.

use std::io::Write;
use std::net::SocketAddr;
use std::process::{Command, Output, Stdio};
use std::sync::Arc;

use glint_core::{Hub, HubConfig};
use glint_store_sqlite::SqliteStore;
use serde_json::Value;

async fn start() -> SocketAddr {
    let store = SqliteStore::open_in_memory().unwrap();
    let (hub, _w) = Hub::start(Arc::new(store), HubConfig::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(glint_server::serve(listener, hub, glint_server::Options::default(), std::future::pending()));
    addr
}

fn glint(url: &str, args: &[&str], stdin: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_glint"))
        .args(args)
        .env("GLINT_URL", url)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
    child.wait_with_output().unwrap()
}

fn events(addr: SocketAddr, query: &str) -> Vec<Value> {
    let v: Value = ureq::get(&format!("http://{addr}/v1/events?{query}")).call().unwrap().into_json().unwrap();
    v["events"].as_array().unwrap().clone()
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    tokio::task::spawn_blocking(f).await.unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn send_values_and_kinds() {
    let addr = start().await;
    let url = format!("http://{addr}");
    blocking(move || {
        let out = glint(&url, &["send", "42", "{\"a\":1}", "hello", "--label", "cli", "--color", "green", "-p", "demo"], "");
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let ids: Vec<_> = String::from_utf8(out.stdout).unwrap().lines().map(str::to_owned).collect();
        assert_eq!(ids.len(), 3);

        let got = events(addr, "kind=core.value");
        let vals: Vec<_> = got.iter().map(|e| e["payload"]["value"].clone()).collect();
        assert_eq!(vals, [serde_json::json!(42), serde_json::json!({"a": 1}), serde_json::json!("hello")]);
        assert_eq!(got[0]["meta"]["label"], "cli");
        assert_eq!(got[0]["meta"]["color"], "green");
        assert_eq!(got[0]["project"], "demo");
        assert_eq!(got[0]["origin"]["via"], "send");
        assert_eq!(got[0]["run"], got[2]["run"]);

        let out = glint(&url, &["send", "--kind", "sql.query", "-q"], "{\"sql\":\"select 1\",\"duration_ms\":2}\n");
        assert!(out.status.success());
        assert!(out.stdout.is_empty());
        assert_eq!(events(addr, "kind=sql.query")[0]["payload"]["sql"], "select 1");

        let out = glint(&url, &["send", "--envelope"], "{\"kind\":\"core.log\",\"payload\":{\"message\":\"env\"},\"meta\":{\"level\":\"warn\"}}");
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        assert_eq!(events(addr, "level=warn")[0]["payload"]["message"], "env");

        let out = glint(&url, &["send", "--kind", "nope", "1"], "");
        assert!(!out.status.success());
        assert!(String::from_utf8_lossy(&out.stderr).contains("invalid kind"));
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn pipe_echoes_folds_and_classifies() {
    let addr = start().await;
    let url = format!("http://{addr}");
    let input = concat!(
        "\u{1b}[32mServer listening on :3000\u{1b}[0m\n",
        "WARN slow request /orders 812ms\n",
        "TypeError: Cannot read properties of undefined\n",
        "    at renderTotal (/app/src/cart.ts:88:12)\n",
        "    at render (/app/src/app.ts:10:3)\n",
        "{\"level\":50,\"msg\":\"payment failed\",\"orderId\":7}\n",
        "{\"kind\":\"core.value\",\"payload\":{\"value\":1}}\n",
        "\n",
        "done\n",
    );
    blocking(move || {
        let out = glint(&url, &["pipe", "--label", "web"], input);
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        // Output is passed through byte for byte, colors included.
        assert_eq!(String::from_utf8(out.stdout).unwrap(), input);

        let got = events(addr, "");
        let summary: Vec<(String, String, String)> = got
            .iter()
            .map(|e| {
                (
                    e["kind"].as_str().unwrap().to_owned(),
                    e["meta"]["level"].as_str().unwrap_or("-").to_owned(),
                    e["payload"]["message"].as_str().map(|m| m.lines().next().unwrap().to_owned()).unwrap_or_default(),
                )
            })
            .collect();
        let s = |a: &str, b: &str, c: &str| (a.to_owned(), b.to_owned(), c.to_owned());
        assert_eq!(
            summary,
            [
                s("core.log", "-", "Server listening on :3000"),
                s("core.log", "warn", "WARN slow request /orders 812ms"),
                s("core.log", "error", "TypeError: Cannot read properties of undefined"),
                s("core.log", "error", "payment failed"),
                s("core.value", "-", ""),
                s("core.log", "-", "done"),
            ]
        );
        assert_eq!(got[2]["payload"]["message"].as_str().unwrap().lines().count(), 3, "stack trace folded");
        assert_eq!(got[3]["payload"]["fields"]["orderId"], 7);
        assert_eq!(got[0]["meta"]["label"], "web");
        assert!(got.iter().all(|e| e["run"] == got[0]["run"]));
    })
    .await;
}

#[test]
fn pipe_keeps_flowing_when_glint_is_down() {
    // Nothing listens on port 9 (discard) on a dev machine.
    let out = glint("http://127.0.0.1:9", &["pipe"], "one\ntwo\n");
    assert!(out.status.success());
    assert_eq!(String::from_utf8(out.stdout).unwrap(), "one\ntwo\n");
    let err = String::from_utf8(out.stderr).unwrap();
    assert!(err.contains("cannot send"), "{err}");
    assert!(err.contains("2 lines were not sent"), "{err}");
}

#[test]
fn send_fails_loudly_when_glint_is_down() {
    let out = glint("http://127.0.0.1:9", &["send", "1"], "");
    assert!(!out.status.success());
    assert!(String::from_utf8(out.stderr).unwrap().contains("Is glintd running?"));
}

#[test]
fn hook_is_silent_and_succeeds_when_glint_is_down() {
    let started = std::time::Instant::now();
    let out = glint("http://127.0.0.1:9", &["hook"], r#"{"hook_event_name":"PreToolUse","session_id":"s"}"#);
    assert!(out.status.success());
    assert!(out.stdout.is_empty() && out.stderr.is_empty());
    assert!(started.elapsed() < std::time::Duration::from_secs(3));
}

#[tokio::test(flavor = "multi_thread")]
async fn hook_forwards_to_glint() {
    let addr = start().await;
    let url = format!("http://{addr}");
    blocking(move || {
        let out = glint(&url, &["hook"], r#"{"hook_event_name":"UserPromptSubmit","session_id":"s1","cwd":"/a/shop","prompt":"hi"}"#);
        assert!(out.status.success() && out.stdout.is_empty());
        let got = events(addr, "kind=agent.step");
        assert_eq!(got[0]["payload"]["input"], "hi");
    })
    .await;
}
