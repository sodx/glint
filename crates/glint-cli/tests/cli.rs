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

/// Runs `glint` with a sandbox HOME (hook state, config) and extra environment.
fn glint_home(url: &str, home: &std::path::Path, env: &[(&str, &str)], args: &[&str], stdin: &str) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_glint"));
    cmd.args(args).env("GLINT_URL", url).env("HOME", home).env_remove("GLINT_CONFIG").env_remove("GLINT_AGENT_CONTEXT").env_remove("GLINT_STOP_GUARD");
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut child = cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
    child.wait_with_output().unwrap()
}

fn sandbox_home(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("glint-cli-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn post(addr: SocketAddr, body: Value) {
    ureq::post(&format!("http://{addr}/v1/events")).send_json(body).unwrap();
}

#[test]
fn session_start_still_explains_glint_when_it_is_down() {
    let home = sandbox_home("down");
    let out = glint_home("http://127.0.0.1:9", &home, &[], &["hook"], r#"{"hook_event_name":"SessionStart","session_id":"s","cwd":"/a/shop"}"#);
    assert!(out.status.success());
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    let text = v["hookSpecificOutput"]["additionalContext"].as_str().unwrap();
    assert!(text.contains("project `shop`") && text.contains("glint_last_error"), "{text}");
    assert!(text.contains("Glint is not running right now") && text.contains("glintd"), "{text}");
    // Other events stay silent.
    let out = glint_home("http://127.0.0.1:9", &home, &[], &["hook"], r#"{"hook_event_name":"UserPromptSubmit","session_id":"s","cwd":"/a/shop","prompt":"x"}"#);
    assert!(out.stdout.is_empty());
    std::fs::remove_dir_all(&home).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn stop_guard_starts_counting_without_a_mark_and_counts_what_it_does_not_show() {
    let addr = start().await;
    let url = format!("http://{addr}");
    let home = sandbox_home("guard");
    blocking(move || {
        let stop = r#"{"hook_event_name":"Stop","session_id":"g1","cwd":"/a/shop"}"#;
        post(addr, serde_json::json!({"kind": "core.log", "project": "shop", "payload": {"message": "before"}}));
        // No mark (SessionStart never ran): nothing to block on this turn, but counting starts.
        let out = glint_home(&url, &home, &[], &["hook"], stop);
        assert!(out.stdout.is_empty());
        assert!(home.join(".glint/hooks/g1.last").exists(), "the guard set its mark");

        let fails: Vec<Value> = (0..7)
            .map(|i| serde_json::json!({"kind": "core.exception", "project": "shop", "run": format!("r{i}"), "payload": {"class": "E", "message": format!("boom {i}")}}))
            .collect();
        post(addr, Value::Array(fails));
        let out = glint_home(&url, &home, &[], &["hook"], stop);
        let v: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(v["decision"], "block");
        let reason = v["reason"].as_str().unwrap();
        assert!(reason.contains("recorded 7 failures"), "{reason}");
        assert!(reason.contains("boom 6") && reason.contains("boom 2") && !reason.contains("boom 1"), "{reason}");
        assert!(reason.contains("(2 more not shown; glint_events project=shop level=error after="), "{reason}");
        // Reported once.
        let out = glint_home(&url, &home, &[], &["hook"], stop);
        assert!(out.stdout.is_empty());
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn stop_guard_hides_an_edited_failure_only_after_a_passing_rerun() {
    let addr = start().await;
    let url = format!("http://{addr}");
    let home = sandbox_home("edited");
    blocking(move || {
        let stop = r#"{"hook_event_name":"Stop","session_id":"g2","cwd":"/a/shop"}"#;
        let file = home.join("app.py");
        let file_s = file.display().to_string();
        post(addr, serde_json::json!({"kind": "core.log", "project": "shop", "payload": {"message": "start"}}));
        glint_home(&url, &home, &[], &["hook"], r#"{"hook_event_name":"SessionStart","session_id":"g2","cwd":"/a/shop"}"#);

        // Failed 10 s ago, then the agent edited the file: possibly fixed, still listed.
        let past = (chrono::Utc::now() - chrono::Duration::seconds(10)).to_rfc3339();
        post(addr, serde_json::json!({"kind": "core.exception", "project": "shop", "run": "f1", "ts": past, "origin": {"file": file_s}, "payload": {"class": "KeyError", "message": "email"}}));
        std::fs::write(&file, "fixed").unwrap();
        let out = glint_home(&url, &home, &[], &["hook"], stop);
        let v: Value = serde_json::from_slice(&out.stdout).unwrap();
        let reason = v["reason"].as_str().unwrap();
        assert!(reason.contains("KeyError: email") && reason.contains("possibly fixed"), "{reason}");

        // Same again, but a later run of the same file passed: not worth stopping for.
        let past = (chrono::Utc::now() - chrono::Duration::seconds(10)).to_rfc3339();
        post(addr, serde_json::json!({"kind": "core.exception", "project": "shop", "run": "f2", "ts": past, "origin": {"file": file_s}, "payload": {"class": "KeyError", "message": "again"}}));
        std::fs::write(&file, "fixed again").unwrap();
        post(addr, serde_json::json!({"kind": "core.log", "project": "shop", "run": "ok1", "origin": {"file": file_s}, "payload": {"message": "sent"}}));
        let out = glint_home(&url, &home, &[], &["hook"], stop);
        assert!(out.stdout.is_empty(), "{}", String::from_utf8_lossy(&out.stdout));
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn prompts_mark_the_turn_even_with_agent_context_off() {
    let addr = start().await;
    let url = format!("http://{addr}");
    let home = sandbox_home("ctxoff");
    blocking(move || {
        post(addr, serde_json::json!({"kind": "core.log", "project": "shop", "payload": {"message": "x"}}));
        let out = glint_home(&url, &home, &[("GLINT_AGENT_CONTEXT", "0")], &["hook"], r#"{"hook_event_name":"UserPromptSubmit","session_id":"g3","cwd":"/a/shop","prompt":"hi"}"#);
        assert!(out.stdout.is_empty());
        assert!(home.join(".glint/hooks/g3.last").exists());
        post(addr, serde_json::json!({"kind": "core.exception", "project": "shop", "payload": {"class": "E", "message": "in turn"}}));
        let out = glint_home(&url, &home, &[("GLINT_AGENT_CONTEXT", "0")], &["hook"], r#"{"hook_event_name":"Stop","session_id":"g3","cwd":"/a/shop"}"#);
        let v: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert!(v["reason"].as_str().unwrap().contains("in turn"));
    })
    .await;
}
