use prost::Message;
use serde_json::{Value, json};

use super::proto::{self, any_value};
use super::*;
use glint_protocol::Level;

fn kv(k: &str, v: Value) -> Value {
    let any = match v {
        Value::String(s) => json!({ "stringValue": s }),
        Value::Bool(b) => json!({ "boolValue": b }),
        Value::Number(n) if n.is_i64() => json!({ "intValue": n.to_string() }),
        Value::Number(n) => json!({ "doubleValue": n }),
        Value::Array(a) => json!({ "arrayValue": { "values": a.into_iter().map(|x| json!({ "stringValue": x })).collect::<Vec<_>>() } }),
        other => panic!("unsupported {other}"),
    };
    json!({ "key": k, "value": any })
}

fn traces_json(attrs: Vec<Value>, events: Value, status: Value) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "resourceSpans": [{
            "resource": { "attributes": [
                kv("service.name", json!("checkout")),
                kv("telemetry.sdk.language", json!("python")),
                kv("telemetry.sdk.name", json!("opentelemetry")),
                kv("telemetry.sdk.version", json!("1.30.0"))
            ]},
            "scopeSpans": [{
                "scope": { "name": "opentelemetry.instrumentation.anthropic" },
                "spans": [{
                    "traceId": "5B8EFFF798038103D269B633813FC60C",
                    "spanId": "EEE19B7EC3C1B174",
                    "parentSpanId": "EEE19B7EC3C1B173",
                    "name": "chat claude",
                    "kind": 3,
                    "startTimeUnixNano": "1795770000000000000",
                    "endTimeUnixNano": "1795770001250000000",
                    "attributes": attrs,
                    "events": events,
                    "status": status
                }]
            }]
        }]
    }))
    .unwrap()
}

#[test]
fn current_semconv_chat_span_becomes_llm_call() {
    let body = traces_json(
        vec![
            kv("gen_ai.operation.name", json!("chat")),
            kv("gen_ai.provider.name", json!("anthropic")),
            kv("gen_ai.request.model", json!("claude-sonnet-5-5")),
            kv("gen_ai.request.max_tokens", json!(1024)),
            kv("gen_ai.request.temperature", json!(0.2)),
            kv("gen_ai.usage.input_tokens", json!(812)),
            kv("gen_ai.usage.output_tokens", json!(64)),
            kv("gen_ai.usage.cache_read.input_tokens", json!(700)),
            kv("gen_ai.response.finish_reasons", json!(["end_turn"])),
            kv("gen_ai.input.messages", json!(r#"[{"role":"user","parts":[{"type":"text","content":"What is 2+2?"}]}]"#)),
            kv("gen_ai.output.messages", json!(r#"[{"role":"assistant","parts":[{"type":"text","content":"4"}]}]"#)),
        ],
        json!([]),
        json!({}),
    );
    let out = decode_traces(&body, Some("application/json")).unwrap();
    assert_eq!(out.len(), 1);
    let e = &out[0];
    assert_eq!(e.kind, "llm.call");
    assert_eq!(e.id.as_deref(), Some("otel-eee19b7ec3c1b174"));
    assert_eq!(e.parent.as_deref(), Some("otel-eee19b7ec3c1b173"));
    assert_eq!(e.run.as_deref(), Some("otel-5b8efff798038103d269b633813fc60c"));
    assert_eq!(e.project.as_deref(), Some("checkout"));
    assert_eq!(e.origin.lang.as_deref(), Some("python"));
    assert_eq!(e.origin.sdk.as_deref(), Some("opentelemetry/1.30.0"));
    assert_eq!(e.meta.label.as_deref(), Some("claude-sonnet-5-5"));
    let p = &e.payload;
    assert_eq!(p["provider"], "anthropic");
    assert_eq!(p["usage"], json!({"input": 812, "output": 64, "cache_read": 700}));
    assert_eq!(p["latency_ms"], 1250.0);
    assert_eq!(p["stop_reason"], json!(["end_turn"]));
    assert_eq!(p["messages"][0]["parts"][0]["content"], "What is 2+2?");
    assert_eq!(p["output"][0]["parts"][0]["content"], "4");
    assert_eq!(p["params"], json!({"max_tokens": 1024, "temperature": 0.2}));
    assert_eq!(e.ts.unwrap().timestamp(), 1_795_770_000);
}

#[test]
fn openllmetry_indexed_prompts() {
    let body = traces_json(
        vec![
            kv("gen_ai.system", json!("openai")),
            kv("gen_ai.request.model", json!("gpt-5")),
            kv("gen_ai.prompt.0.role", json!("system")),
            kv("gen_ai.prompt.0.content", json!("Be brief.")),
            kv("gen_ai.prompt.1.role", json!("user")),
            kv("gen_ai.prompt.1.content", json!("Hi")),
            kv("gen_ai.completion.0.role", json!("assistant")),
            kv("gen_ai.completion.0.content", json!("Hello!")),
            kv("gen_ai.completion.0.finish_reason", json!("stop")),
            kv("gen_ai.usage.prompt_tokens", json!(12)),
            kv("gen_ai.usage.completion_tokens", json!(3)),
            kv("llm.usage.total_tokens", json!(15)),
        ],
        json!([]),
        json!({}),
    );
    let p = &decode_traces(&body, Some("application/json")).unwrap()[0].payload;
    assert_eq!(p["provider"], "openai");
    assert_eq!(p["messages"], json!([{"role": "system", "content": "Be brief."}, {"role": "user", "content": "Hi"}]));
    assert_eq!(p["output"][0]["finish_reason"], "stop");
    assert_eq!(p["usage"], json!({"input": 12, "output": 3, "total": 15}));
}

#[test]
fn event_based_messages() {
    let body = traces_json(
        vec![kv("gen_ai.system", json!("anthropic")), kv("gen_ai.request.model", json!("claude-haiku-5-5"))],
        json!([
            { "name": "gen_ai.system.message", "timeUnixNano": "1", "attributes": [kv("content", json!("You are terse."))] },
            { "name": "gen_ai.user.message", "timeUnixNano": "2", "attributes": [kv("content", json!("Ping"))] },
            { "name": "gen_ai.choice", "timeUnixNano": "3", "attributes": [kv("message", json!(r#"{"content":"Pong"}"#)), kv("finish_reason", json!("end_turn"))] }
        ]),
        json!({}),
    );
    let p = &decode_traces(&body, Some("application/json")).unwrap()[0].payload;
    assert_eq!(p["messages"], json!([{"role": "system", "content": "You are terse."}, {"role": "user", "content": "Ping"}]));
    assert_eq!(p["output"], json!([{"role": "assistant", "content": "Pong", "finish_reason": "end_turn"}]));
}

#[test]
fn tool_spans_db_spans_errors_and_exceptions() {
    let tool = traces_json(
        vec![
            kv("gen_ai.operation.name", json!("execute_tool")),
            kv("gen_ai.tool.name", json!("get_weather")),
            kv("gen_ai.tool.call.arguments", json!(r#"{"city":"Lisbon"}"#)),
        ],
        json!([]),
        json!({}),
    );
    let e = &decode_traces(&tool, Some("application/json")).unwrap()[0];
    assert_eq!((e.kind.as_str(), &e.payload["args"]["city"]), ("agent.tool_call", &json!("Lisbon")));

    let db = traces_json(vec![kv("db.system.name", json!("postgresql")), kv("db.query.text", json!("select 1"))], json!([]), json!({}));
    let e = &decode_traces(&db, Some("application/json")).unwrap()[0];
    assert_eq!(e.kind, "sql.query");
    assert_eq!(e.payload["conn"], "postgresql");

    let failed = traces_json(
        vec![kv("http.route", json!("/pay"))],
        json!([{ "name": "exception", "timeUnixNano": "1795770000500000000", "attributes": [
            kv("exception.type", json!("ValueError")), kv("exception.message", json!("bad amount")), kv("exception.stacktrace", json!("Traceback…"))
        ]}]),
        json!({ "code": 2, "message": "boom" }),
    );
    let out = decode_traces(&failed, Some("application/json")).unwrap();
    assert_eq!(out[0].kind, "core.span");
    assert_eq!(out[0].meta.level, Some(Level::Error));
    assert_eq!(out[0].payload["status"], "error");
    assert_eq!(out[0].payload["kind"], "client");
    assert_eq!(out[1].kind, "core.exception");
    assert_eq!(out[1].payload["class"], "ValueError");
    assert_eq!(out[1].parent.as_deref(), Some("otel-eee19b7ec3c1b174"));
}

fn sv(s: &str) -> Option<proto::AnyValue> {
    Some(proto::AnyValue { value: Some(any_value::Value::StringValue(s.into())) })
}

fn pkv(k: &str, v: Option<proto::AnyValue>) -> proto::KeyValue {
    proto::KeyValue { key: k.into(), value: v }
}

#[test]
fn protobuf_traces_decode_like_json() {
    let req = proto::ExportTraceServiceRequest {
        resource_spans: vec![proto::ResourceSpans {
            resource: Some(proto::Resource { attributes: vec![pkv("service.name", sv("api"))] }),
            scope_spans: vec![proto::ScopeSpans {
                scope: Some(proto::InstrumentationScope { name: "s".into(), version: "1".into() }),
                spans: vec![proto::Span {
                    trace_id: vec![0xab; 16],
                    span_id: vec![0x01; 8],
                    parent_span_id: vec![],
                    name: "chat".into(),
                    kind: 3,
                    start_time_unix_nano: 1_000_000_000,
                    end_time_unix_nano: 1_500_000_000,
                    attributes: vec![
                        pkv("gen_ai.system", sv("openai")),
                        pkv("gen_ai.request.model", sv("gpt-5")),
                        pkv("gen_ai.usage.input_tokens", Some(proto::AnyValue { value: Some(any_value::Value::IntValue(40)) })),
                        pkv("gen_ai.request.temperature", Some(proto::AnyValue { value: Some(any_value::Value::DoubleValue(0.5)) })),
                    ],
                    events: vec![],
                    status: None,
                }],
            }],
        }],
    };
    let bytes = req.encode_to_vec();
    let e = &decode_traces(&bytes, Some("application/x-protobuf")).unwrap()[0];
    assert_eq!(e.kind, "llm.call");
    assert_eq!(e.run.as_deref(), Some(&*format!("otel-{}", "ab".repeat(16))));
    assert_eq!(e.parent, None);
    assert_eq!(e.payload["usage"]["input"], 40);
    assert_eq!(e.payload["params"]["temperature"], 0.5);
    assert_eq!(e.payload["latency_ms"], 500.0);
    assert_eq!(e.project.as_deref(), Some("api"));
}

#[test]
fn logs_json_and_protobuf() {
    let body = serde_json::to_vec(&json!({
        "resourceLogs": [{
            "resource": { "attributes": [kv("service.name", json!("worker"))] },
            "scopeLogs": [{ "scope": { "name": "app" }, "logRecords": [
                { "timeUnixNano": "1795770000000000000", "severityNumber": 17, "severityText": "ERROR",
                  "body": { "stringValue": "job failed" }, "attributes": [kv("job", json!("sync"))],
                  "traceId": "5b8efff798038103d269b633813fc60c", "spanId": "eee19b7ec3c1b174" },
                { "severityNumber": 9, "body": { "kvlistValue": { "values": [kv("a", json!(1))] } } }
            ]}]
        }]
    }))
    .unwrap();
    let out = decode_logs(&body, Some("application/json")).unwrap();
    assert_eq!(out[0].kind, "core.log");
    assert_eq!(out[0].meta.level, Some(Level::Error));
    assert_eq!(out[0].payload, json!({"message": "job failed", "fields": {"job": "sync"}}));
    assert_eq!(out[0].parent.as_deref(), Some("otel-eee19b7ec3c1b174"));
    assert_eq!(out[0].run.as_deref(), Some("otel-5b8efff798038103d269b633813fc60c"));
    assert_eq!(out[1].payload["message"], r#"{"a":1}"#);
    assert_eq!(out[1].meta.level, Some(Level::Info));

    let req = proto::ExportLogsServiceRequest {
        resource_logs: vec![proto::ResourceLogs {
            resource: None,
            scope_logs: vec![proto::ScopeLogs {
                scope: None,
                log_records: vec![proto::LogRecord { severity_number: 13, body: sv("slow"), ..Default::default() }],
            }],
        }],
    };
    let out = decode_logs(&req.encode_to_vec(), None).unwrap();
    assert_eq!((out[0].payload["message"].as_str(), out[0].meta.level), (Some("slow"), Some(Level::Warn)));
}

#[test]
fn bad_inputs() {
    assert!(matches!(decode_traces(b"x", Some("text/plain")), Err(OtlpError::ContentType(_))));
    assert!(matches!(decode_traces(b"{", Some("application/json")), Err(OtlpError::Json(_))));
    assert!(matches!(decode_traces(&[0xff, 0xff, 0xff], Some("application/x-protobuf")), Err(OtlpError::Protobuf(_))));
    assert!(decode_traces(b"{}", Some("application/json")).unwrap().is_empty());
}

#[test]
fn stack_text_becomes_frames() {
    let py = "Traceback (most recent call last):\n  File \"/app/checkout.py\", line 42, in charge\n    pay()\n  File \"/venv/lib/python3.14/site-packages/stripe/api.py\", line 9, in pay\n    raise ValueError\nValueError: card declined";
    let f = map::frames_from_text(py);
    assert_eq!(f[0], json!({"file": "/venv/lib/python3.14/site-packages/stripe/api.py", "line": 9, "fn": "pay", "vendor": true}));
    assert_eq!(f[1], json!({"file": "/app/checkout.py", "line": 42, "fn": "charge", "vendor": false}));
    let js = "TypeError: x\n    at renderTotal (/app/src/cart.ts:88:12)\n    at /app/node_modules/react/index.js:1:2";
    let f = map::frames_from_text(js);
    assert_eq!(f[0], json!({"file": "/app/src/cart.ts", "line": 88, "fn": "renderTotal", "vendor": false}));
    assert_eq!(f[1]["vendor"], true);
    assert!(map::frames_from_text("no trace here").is_empty());
}
