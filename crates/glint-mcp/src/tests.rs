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
    assert_eq!(names.len(), 12);
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

    let (text, _) = call(&s, "glint_events", json!({"kind": "core.exception", "project": "*"})).await;
    assert!(text.starts_with("Project: all (as asked)\n"), "{text}");
    assert!(text.contains("TypeError: total is undefined @ renderTotal"));

    let (text, _) = call(&s, "glint_runs", json!({})).await;
    assert!(text.starts_with("Project: all (this agent's project is unknown"), "{text}");
    let rows: Vec<_> = text.lines().skip(2).collect();
    assert!(rows[0].starts_with("req-2 · blog"));
    assert!(rows[1].starts_with("req-1 · shop") && rows[1].contains(" · 3 · 1 · "));

    let (text, _) = call(&s, "glint_last_error", json!({"project": "shop"})).await;
    assert!(text.starts_with("Project: shop (as asked)\n"), "{text}");
    assert!(text.contains("TypeError: total is undefined"));
    assert!(text.contains("at renderTotal (…/shop/src/cart.ts:88)"));
    assert!(text.contains("[vendor]"));
    assert!(text.contains("What happened before it in this run (2 events)"));
    assert!(text.contains("GET /cart"));
    assert!(!text.contains("other"), "context must stay within the run");

    let id = text.lines().nth(1).unwrap().split_whitespace().nth(2).unwrap().to_owned();
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
    assert!(text.contains("Arrived after the call") && text.contains("fresh") && !text.contains("old"), "{text}");

    let (text, _) = call(&s, "glint_wait_for", json!({"kind": "core.value", "lookback_s": 0, "timeout_s": 1})).await;
    assert!(text.ends_with("No matching event: searched only new events (lookback_s=0), then waited 1 s."), "{text}");
}

#[tokio::test]
async fn wait_for_finds_what_happened_just_before_the_call() {
    let (s, hub) = server().await;
    put(&hub, r#"{"kind":"core.value","payload":{"value":"early"},"meta":{"label":"marker"}}"#).await;
    // The default lookback sees it, and says which window it searched.
    let (text, _) = call(&s, "glint_wait_for", json!({"q": "marker", "timeout_s": 1})).await;
    assert!(text.contains("Found in the 10 s before the call") && text.contains("early"), "{text}");
    // lookback_s=0 waits only for new ones.
    let (text, _) = call(&s, "glint_wait_for", json!({"q": "marker", "lookback_s": 0, "timeout_s": 1})).await;
    assert!(text.contains("No matching event"), "{text}");
    // after=<id> searches everything stored after that event, however old.
    put(&hub, r#"{"id":"CUT","kind":"core.log","payload":{"message":"cut"}}"#).await;
    put(&hub, r#"{"kind":"core.value","payload":{"value":"later"},"meta":{"label":"marker"}}"#).await;
    let (text, _) = call(&s, "glint_wait_for", json!({"q": "marker", "after": "CUT", "lookback_s": 0, "timeout_s": 1})).await;
    assert!(text.contains("Found in stored events after CUT") && text.contains("later") && !text.contains("early"), "{text}");
}

#[tokio::test]
async fn tools_read_the_callers_project_and_say_so() {
    let (s, hub) = server().await;
    put(&hub, r#"[
        {"kind":"core.exception","payload":{"class":"ShopError","message":"in shop"},"run":"a","project":"shop"},
        {"kind":"core.exception","payload":{"class":"BlogError","message":"in blog"},"run":"b","project":"blog"}
    ]"#)
    .await;
    let ask = |caller: Caller| {
        let s = s.clone();
        async move {
            let r = s
                .handle_from(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "glint_last_error", "arguments": {}}}), &caller)
                .await;
            r.reply.unwrap()["result"]["content"][0]["text"].as_str().unwrap().to_owned()
        }
    };
    // Named by the connection (glint mcp's X-Glint-Project, or /mcp?project=).
    let t = ask(Caller { session: None, project: Some("shop".into()) }).await;
    assert!(t.starts_with("Project: shop (named by the connection)") && t.contains("ShopError") && !t.contains("BlogError"), "{t}");
    let t = ask(Caller { session: None, project: Some("blog".into()) }).await;
    assert!(t.contains("BlogError") && !t.contains("ShopError"), "{t}");
    // Unknown, with two projects: no mixing, a list instead.
    let t = ask(Caller::default()).await;
    assert!(t.contains("does not know which project") && t.contains("- shop · ") && t.contains("- blog · "), "{t}");
    assert!(!t.contains("Error:"), "{t}");
    // An explicit argument wins over the connection's project.
    let (t, _) = call(&s, "glint_events", json!({"project": "blog"})).await;
    assert!(t.starts_with("Project: blog (as asked)") && t.contains("in blog") && !t.contains("in shop"), "{t}");
}

#[tokio::test]
async fn workspace_roots_scope_an_http_session() {
    let (s, hub) = server().await;
    put(&hub, r#"[
        {"kind":"core.exception","payload":{"class":"ShopError","message":"in shop"},"run":"a","project":"shop"},
        {"kind":"core.exception","payload":{"class":"BlogError","message":"in blog"},"run":"b","project":"blog"}
    ]"#)
    .await;
    let caller = Caller { session: Some("glint-1-claude-code".into()), project: None };
    let msg = json!({"jsonrpc": "2.0", "id": 7, "method": "tools/call", "params": {"name": "glint_last_error", "arguments": {}}});
    assert!(s.wants_roots(&msg, &caller));
    let (ask, answered) = s.roots_request("glint-1-claude-code");
    assert_eq!(ask["method"], "roots/list");
    assert!(!s.wants_roots(&msg, &caller), "asked once per session");
    // The client answers with its workspace: a folder named blog (no .git), with a space escaped.
    let dir = std::env::temp_dir().join(format!("glint roots {}", std::process::id())).join("blog");
    std::fs::create_dir_all(&dir).unwrap();
    let uri = format!("file://{}", dir.display().to_string().replace(' ', "%20"));
    let r = s.handle_from(json!({"jsonrpc": "2.0", "id": ask["id"], "result": {"roots": [{"uri": uri, "name": "blog"}]}}), &caller).await;
    assert!(r.reply.is_none(), "a response gets no reply");
    answered.await.unwrap();
    let t = s.handle_from(msg.clone(), &caller).await.reply.unwrap()["result"]["content"][0]["text"].as_str().unwrap().to_owned();
    assert!(t.starts_with("Project: blog (the agent's workspace ") && t.contains("BlogError") && !t.contains("ShopError"), "{t}");
    // The client says its roots changed: asked again on the next call.
    s.handle_from(json!({"jsonrpc": "2.0", "method": "notifications/roots/list_changed"}), &caller).await;
    assert!(s.wants_roots(&msg, &caller));
    // A call that names a project, or a connection that does, needs no roots.
    assert!(!s.wants_roots(&json!({"method": "tools/call", "params": {"arguments": {"project": "x"}}}), &caller));
    assert!(!s.wants_roots(&msg, &Caller { session: Some("s2".into()), project: Some("x".into()) }));
    std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
}

#[test]
fn root_uris_become_paths() {
    assert_eq!(root_path("file:///Users/a%20b/shop").as_deref(), Some("/Users/a b/shop"));
    assert_eq!(root_path("file://localhost/srv/x").as_deref(), Some("/srv/x"));
    assert_eq!(root_path("file:///C:/work/app").as_deref(), Some("C:/work/app"));
    assert_eq!(root_path("https://x"), None);
}

#[tokio::test]
async fn expect_passes_on_markers_old_and_new_and_fails_on_a_crash() {
    let (s, hub) = server().await;
    // Sent just before asking (lookback), then the rest while it waits.
    put(&hub, r#"{"id":"A","kind":"core.value","payload":{"value":{"total":129.5}},"project":"shop","meta":{"label":"cart"}}"#).await;
    let h = hub.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        put(&h, r#"{"id":"B","kind":"core.value","payload":{"value":{"paid":true}},"project":"shop","meta":{"label":"order saved"}}"#).await;
    });
    let (text, err) = call(&s, "glint_expect", json!({"project": "shop", "timeout_s": 5, "expect": ["label:cart payload.value.total>0", "label:\"order saved\" payload.value.paid=true"]})).await;
    assert!(!err, "{text}");
    assert!(text.starts_with("Project: shop (as asked)\nPASS") && text.contains("— A") && text.contains("— B"), "{text}");

    let h = hub.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        put(&h, r#"{"kind":"core.exception","payload":{"class":"TypeError","message":"coupon is null","frames":[]},"project":"shop"}"#).await;
    });
    let (text, _) = call(&s, "glint_expect", json!({"project": "shop", "lookback_s": 0, "timeout_s": 5, "expect": ["label:\"never sent\""]})).await;
    assert!(text.contains("\nFAIL") && text.contains("TypeError: coupon is null"), "{text}");

    let (text, err) = call(&s, "glint_expect", json!({"expect": ["nonsense"]})).await;
    assert!(err && text.contains("expected"), "{text}");
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
    let (text, _) = call(&s, "glint_events", json!({"after": cursor, "project": "*"})).await;
    assert!(text.contains("after clear") && !text.contains("boom"));

    let all = hub.query(glint_core::Query::default()).await.unwrap();
    let upd = all.iter().find(|e| e.kind == "core.update").unwrap();
    assert_eq!(upd.payload, json!({"target": "E1", "meta": {"note": "root cause: null total", "color": "red"}}));
    assert_eq!(upd.origin.lang.as_deref(), Some("agent"));
    assert!(all.iter().any(|e| e.kind == "core.mermaid" && e.meta.label.as_deref() == Some("flow")));
}

#[tokio::test]
async fn llm_calls_list_and_totals() {
    let (s, hub) = server().await;
    put(&hub, r#"[
        {"kind":"llm.call","payload":{"model":"claude-sonnet-5-5","usage":{"input":800,"output":60},"latency_ms":1200,"cost_usd":0.003,
          "messages":[{"role":"user","parts":[{"type":"text","content":"What is 2+2?"}]}],"output":[{"role":"assistant","parts":[{"type":"text","content":"4"}]}]}},
        {"kind":"llm.call","payload":{"model":"gpt-5","usage":{"input":20,"output":5},"latency_ms":400,"error":"rate limited","messages":[{"role":"user","content":"hi"}]},"meta":{"level":"error"}}
    ]"#)
    .await;
    let (text, err) = call(&s, "glint_llm_calls", json!({})).await;
    assert!(!err);
    assert!(text.contains("claude-sonnet-5-5 800→60 tok 1200 ms $0.003 — What is 2+2? ⇒ 4"), "{text}");
    assert!(text.contains("gpt-5 20→5 tok 400 ms ERROR rate limited"));
    assert!(text.contains("Totals over 2 calls: 820 input + 65 output tokens, avg latency 800 ms, 1 errors, cost $0.0030"));
    let (text, _) = call(&s, "glint_llm_calls", json!({"model": "sonnet"})).await;
    assert!(text.contains("Totals over 1 calls"));
}

#[tokio::test]
async fn tool_calls_are_recorded_with_the_client_and_the_events_it_saw() {
    let (s, hub) = server().await;
    put(&hub, r#"[{"kind":"core.exception","payload":{"class":"E","message":"boom"},"run":"r1","project":"shop"}]"#).await;
    let init = s
        .handle_from(
            json!({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "clientInfo": {"name": "claude-code", "version": "2"}}}),
            &Caller::default(),
        )
        .await;
    let sid = init.session.expect("a session id for initialize");
    assert!(sid.ends_with("-claude-code"), "{sid}");
    let caller = Caller { session: Some(sid.clone()), project: Some("shop".into()) };
    let r = s
        .handle_from(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "glint_last_error", "arguments": {}}}), &caller)
        .await;
    assert!(r.reply.is_some() && r.session.is_none());
    // Without the session (a client that drops it): still recorded, as an unknown agent.
    s.handle(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "glint_runs", "arguments": {}}})).await;

    let st = hub.agent_stats(1, None, 10).await.unwrap();
    let t = &st.totals;
    assert_eq!(t.calls, 2);
    assert_eq!(t.agents["claude-code"].calls, 1);
    assert_eq!(t.agents["claude-code"].sessions, 1);
    assert_eq!(t.agents["unknown"].calls, 1);
    assert_eq!((t.failures, t.read, t.read_in_time), (1, 1, 1));
    assert_eq!(t.calls_with_failures, 2, "glint_runs names the run's last event, the failure");
    let reads = hub.reads_in_run("r1".into()).await.unwrap();
    assert!(reads.iter().any(|r| r.agent == "claude-code" && r.via == "mcp"));
}
