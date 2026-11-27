//! Bodies below are shaped exactly like `spatie/ray` sends them (see `docs/research/ray.md`).

use super::*;
use serde_json::json;

const META: &str = r#"{"php_version":"8.3.4","php_version_id":80304,"project_name":"shop","ray_package_version":"1.41.0"}"#;

fn origin() -> Value {
    json!({"file": "/srv/shop/app/Http/Controllers/OrderController.php", "line_number": 42, "hostname": "mbp"})
}

fn body(uuid: &str, payloads: Value) -> Vec<u8> {
    let meta: Value = serde_json::from_str(META).unwrap();
    serde_json::to_vec(&json!({ "uuid": uuid, "payloads": payloads, "meta": meta })).unwrap()
}

fn p(ty: &str, content: Value) -> Value {
    json!({ "type": ty, "content": content, "origin": origin() })
}

#[test]
fn string_value_keeps_origin_project_and_client() {
    let a = RayAdapter::new();
    let out = a.decode(&body("u1", json!([p("log", json!({"values": ["hello"], "meta": [{"clipboard_data": "hello"}]}))]))).unwrap();
    assert_eq!(out.len(), 1);
    let e = &out[0];
    assert_eq!(e.id.as_deref(), Some("ray-u1"));
    assert_eq!(e.kind, "core.value");
    assert_eq!(e.payload, json!({"value": "hello"}));
    assert_eq!(e.project.as_deref(), Some("shop"));
    assert_eq!(e.origin.file.as_deref(), Some("/srv/shop/app/Http/Controllers/OrderController.php"));
    assert_eq!(e.origin.line, Some(42));
    assert_eq!(e.origin.host.as_deref(), Some("mbp"));
    assert_eq!(e.origin.lang.as_deref(), Some("php"));
    assert_eq!(e.origin.runtime.as_deref(), Some("8.3.4"));
    assert_eq!(e.origin.sdk.as_deref(), Some("spatie/ray/1.41.0"));
    assert_eq!(e.origin.extra["via"], "ray");
}

#[test]
fn dumped_array_leads_with_plain_text_and_keeps_html() {
    let html = "<script> Sfdump = window.Sfdump || ...</script><pre class=sf-dump id=sf-dump-1>array:1 [<samp>\"a\" => 1</samp>]</pre>";
    let out = RayAdapter::new()
        .decode(&body("u2", json!([p("log", json!({"values": [html], "meta": [{"clipboard_data": "['a' => 1]"}]}))])))
        .unwrap();
    assert_eq!(out[0].kind, "core.text");
    assert_eq!(out[0].payload["content"], "['a' => 1]");
    assert_eq!(out[0].payload["html"], html);
}

#[test]
fn chained_command_in_a_later_request_updates_the_entry() {
    let a = RayAdapter::new();
    a.decode(&body("u3", json!([p("log", json!({"values": ["x"]}))]))).unwrap();
    let out = a.decode(&body("u3", json!([p("color", json!({"color": "yellow"}))]))).unwrap();
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].kind, "core.update");
    assert!(out[0].id.is_none());
    assert_eq!(out[0].payload, json!({"target": "ray-u3", "meta": {"color": "orange"}}));
}

#[test]
fn several_values_get_numbered_ids_and_commands_reach_all() {
    let a = RayAdapter::new();
    let first = a
        .decode(&body("u4", json!([p("log", json!({"values": [1]})), p("log", json!({"values": [2]}))])))
        .unwrap();
    let ids: Vec<_> = first.iter().map(|e| e.id.clone().unwrap()).collect();
    assert_eq!(ids, ["ray-u4", "ray-u4-2"]);
    let upd = a.decode(&body("u4", json!([p("label", json!({"label": "pair"})), p("remove", json!({}))]))).unwrap();
    let targets: Vec<_> = upd.iter().map(|e| (e.payload["target"].clone(), e.payload.get("removed").cloned())).collect();
    assert_eq!(
        targets,
        [
            (json!("ray-u4"), None),
            (json!("ray-u4-2"), None),
            (json!("ray-u4"), Some(json!(true))),
            (json!("ray-u4-2"), Some(json!(true))),
        ]
    );
}

#[test]
fn command_before_data_aims_at_first_entry() {
    let out = RayAdapter::new().decode(&body("u5", json!([p("hide", json!({}))]))).unwrap();
    assert_eq!(out[0].payload, json!({"target": "ray-u5", "hidden": true}));
}

#[test]
fn exception_with_trailing_color() {
    let out = RayAdapter::new()
        .decode(&body(
            "u6",
            json!([
                p("exception", json!({
                    "class": "RuntimeException", "message": "boom",
                    "frames": [
                        {"file_name": "/srv/shop/app/Jobs/Sync.php", "line_number": 10, "class": "App\\Jobs\\Sync", "method": "handle", "vendor_frame": false},
                        {"file_name": "/srv/shop/vendor/laravel/framework/src/Container.php", "line_number": 36, "class": null, "method": "call", "vendor_frame": true}
                    ],
                    "meta": []
                })),
                p("color", json!({"color": "red"}))
            ]),
        ))
        .unwrap();
    assert_eq!(out[0].kind, "core.exception");
    assert_eq!(out[0].payload["class"], "RuntimeException");
    assert_eq!(
        out[0].payload["frames"][0],
        json!({"file": "/srv/shop/app/Jobs/Sync.php", "line": 10, "class": "App\\Jobs\\Sync", "fn": "handle", "vendor": false})
    );
    assert_eq!(out[0].payload["frames"][1], json!({"file": "/srv/shop/vendor/laravel/framework/src/Container.php", "line": 36, "fn": "call", "vendor": true}));
    assert_eq!(out[1].payload["target"], "ray-u6");
}

#[test]
fn laravel_query_and_unknown_types() {
    let out = RayAdapter::new()
        .decode(&body(
            "u7",
            json!([
                p("executed_query", json!({"sql": "select * from users where id = ?", "bindings": [7], "connection_name": "mysql", "time": 1.92})),
                p("eloquent_model", json!({"class_name": "App\\Models\\User", "attributes": {"id": 7}}))
            ]),
        ))
        .unwrap();
    assert_eq!(out[0].kind, "sql.query");
    assert_eq!(out[0].payload, json!({"sql": "select * from users where id = ?", "bindings": [7], "conn": "mysql", "duration_ms": 1.92}));
    assert_eq!(out[1].kind, "x-ray.eloquent_model");
    assert_eq!(out[1].payload["class_name"], "App\\Models\\User");
}

#[test]
fn custom_variants() {
    let out = RayAdapter::new()
        .decode(&body(
            "u8",
            json!([
                p("custom", json!({"content": "a&nbsp;b&lt;c&gt;<br />\nd", "label": "Text"})),
                p("custom", json!({"content": false, "label": "Boolean"})),
                p("custom", json!({"content": "<em>hi</em>", "label": "HTML"})),
                p("custom", json!({"content": "Called 3 times.", "label": "Count"}))
            ]),
        ))
        .unwrap();
    assert_eq!((out[0].kind.as_str(), out[0].payload["content"].as_str()), ("core.text", Some("a b<c>\nd")));
    assert_eq!(out[1].payload, json!({"value": false}));
    assert_eq!(out[2].kind, "core.html");
    assert_eq!(out[2].meta.label, None);
    assert_eq!(out[3].kind, "core.text");
    assert_eq!(out[3].meta.label.as_deref(), Some("Count"));
}

#[test]
fn screens_measure_pause_and_ignored() {
    let out = RayAdapter::new()
        .decode(&body(
            "u9",
            json!([
                p("new_screen", json!({"name": ""})),
                p("new_screen", json!({"name": "checkout"})),
                p("measure", json!({"name": "default", "is_new_timer": false, "total_time": 12.5, "max_memory_usage_during_total_time": 2097152, "time_since_last_call": 3, "max_memory_usage_since_last_call": 0})),
                p("create_lock", json!({"name": "abc"})),
                p("confetti", json!({})),
                p("separator", json!({}))
            ]),
        ))
        .unwrap();
    let kinds: Vec<_> = out.iter().map(|e| e.kind.as_str()).collect();
    assert_eq!(kinds, ["core.clear", "core.screen", "core.measure", "ctl.pause", "core.separator"]);
    assert_eq!(out[2].payload["duration_ms"], 12.5);
    assert_eq!(out[2].payload["memory_bytes"], 2_097_152);
    assert_eq!(out[3].id.as_deref(), Some("ray-lock-abc"));
    assert_eq!(out[3].payload, json!({"reason": "ray()->pause()", "lock": "abc"}));
}

#[test]
fn agent_payloads() {
    let out = RayAdapter::new()
        .decode(&body(
            "u10",
            json!([
                p("ai_custom_markdown", json!({"markdown": "# Plan", "label": "agent"})),
                p("ai_custom_mermaid", json!({"mermaid": "graph TD; A-->B"}))
            ]),
        ))
        .unwrap();
    assert_eq!((out[0].kind.as_str(), &out[0].payload["content"]), ("core.markdown", &json!("# Plan")));
    assert_eq!(out[0].meta.label.as_deref(), Some("agent"));
    assert_eq!(out[1].kind, "core.mermaid");
}

#[test]
fn node_ray_meta_and_function_name() {
    let b = serde_json::to_vec(&json!({
        "uuid": "n1",
        "payloads": [{"type": "log", "content": {"values": [1]}, "origin": {"function_name": "main", "file": "/app/index.js", "line_number": 3, "hostname": "box"}}],
        "meta": {"node_ray_package_version": "2.1.0", "project_name": ""}
    }))
    .unwrap();
    let e = &RayAdapter::new().decode(&b).unwrap()[0];
    assert_eq!(e.origin.lang.as_deref(), Some("js"));
    assert_eq!(e.origin.sdk.as_deref(), Some("node-ray/2.1.0"));
    assert_eq!(e.origin.function.as_deref(), Some("main"));
    assert_eq!(e.project, None);
}

#[test]
fn malformed_bodies() {
    let a = RayAdapter::new();
    assert!(matches!(a.decode(b"nope"), Err(RayError::InvalidJson(_))));
    assert_eq!(a.decode(br#"{"payloads":[]}"#), Err(RayError::Shape("missing `uuid`")));
    assert_eq!(a.decode(br#"{"uuid":"x"}"#), Err(RayError::Shape("missing `payloads` array")));
}

#[test]
fn every_output_kind_is_valid() {
    let types = [
        "log", "custom", "json_string", "table", "carbon", "exception", "trace", "caller", "measure",
        "application_log", "executed_query", "separator", "new_screen", "clear_all", "notify", "create_lock",
        "ai_custom_markdown", "ai_custom_mermaid", "ai_custom_html", "event", "job_event", "mailable", "view", "response",
    ];
    let payloads: Vec<Value> = types.iter().map(|t| p(t, json!({}))).collect();
    for e in RayAdapter::new().decode(&body("v", Value::Array(payloads))).unwrap() {
        assert!(glint_protocol::is_valid_kind(&e.kind), "{}", e.kind);
    }
}

#[test]
fn memory_is_bounded() {
    let a = RayAdapter::new();
    for i in 0..(REMEMBERED_UUIDS + 10) {
        a.decode(&body(&format!("m{i}"), json!([p("log", json!({"values": [i]}))]))).unwrap();
    }
    let seen = a.seen.lock().unwrap();
    assert_eq!(seen.data_count.len(), REMEMBERED_UUIDS);
    assert!(!seen.data_count.contains_key("m0"));
}
