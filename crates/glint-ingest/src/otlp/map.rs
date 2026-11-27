//! Spans and log records → envelopes.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use glint_protocol::{IncomingEnvelope, Level, Meta, Origin};
use serde_json::{Map, Value, json};

use super::decode::{LogRec, Span};
use crate::agent::clip_value;

const SPAN_KINDS: [&str; 6] = ["unspecified", "internal", "server", "client", "producer", "consumer"];

pub fn span(s: &Span) -> Vec<IncomingEnvelope> {
    let mut out = Vec::new();
    let a = &s.attrs;
    let is_gen_ai = a.keys().any(|k| {
        matches!(k.as_str(), "gen_ai.operation.name" | "gen_ai.system" | "gen_ai.provider.name" | "gen_ai.request.model")
    });
    let (kind, payload, label, level) = if is_gen_ai {
        gen_ai(s)
    } else if let Some(sql) = str_attr(a, &["db.query.text", "db.statement"]) {
        let mut p = json!({ "sql": sql, "duration_ms": duration_ms(s) });
        if let Some(sys) = str_attr(a, &["db.system.name", "db.system"]) {
            p["conn"] = json!(sys);
        }
        ("sql.query".to_owned(), p, None, error_level(s))
    } else {
        let mut p = json!({
            "name": s.name,
            "kind": SPAN_KINDS.get(usize::try_from(s.kind).unwrap_or(0)).copied().unwrap_or("unspecified"),
            "duration_ms": duration_ms(s),
            "status": match s.status_code { 1 => "ok", 2 => "error", _ => "unset" },
        });
        if !s.status_message.is_empty() {
            p["status_message"] = json!(s.status_message);
        }
        let attrs: Map<String, Value> = a.iter().filter(|(k, _)| !k.starts_with("code.")).map(|(k, v)| (k.clone(), v.clone())).collect();
        if !attrs.is_empty() {
            p["attributes"] = clip_value(&Value::Object(attrs));
        }
        let events: Vec<Value> = s.events.iter().filter(|e| e.name != "exception").map(|e| json!({ "name": e.name, "attributes": e.attrs })).collect();
        if !events.is_empty() {
            p["events"] = clip_value(&Value::Array(events));
        }
        ("core.span".to_owned(), p, Some(s.name.clone()), error_level(s))
    };
    let mut e = envelope(s, &kind, payload);
    e.id = Some(format!("otel-{}", s.span_id));
    e.meta = Meta { label, level, ..Meta::default() };
    out.push(e);

    // Exceptions recorded on the span (`span.recordException`) are first-class events too.
    for ev in s.events.iter().filter(|e| e.name == "exception") {
        let stack = str_attr(&ev.attrs, &["exception.stacktrace"]);
        let p = json!({
            "class": str_attr(&ev.attrs, &["exception.type"]).unwrap_or("Exception"),
            "message": str_attr(&ev.attrs, &["exception.message"]).unwrap_or(""),
            "frames": stack.map(frames_from_text).unwrap_or_default(),
            "stacktrace": stack,
        });
        let mut x = envelope(s, "core.exception", p);
        x.ts = Some(ts(ev.time_ns));
        x.parent = Some(format!("otel-{}", s.span_id));
        out.push(x);
    }
    out
}

pub fn log(l: &LogRec) -> IncomingEnvelope {
    let message = match &l.body {
        Value::String(s) => s.clone(),
        Value::Null => l.event_name.clone(),
        other => other.to_string(),
    };
    // code.* attributes become the origin, not fields.
    let mut fields: Map<String, Value> = l.attrs.iter().filter(|(k, _)| !k.starts_with("code.")).map(|(k, v)| (k.clone(), v.clone())).collect();
    if !l.event_name.is_empty() {
        fields.insert("event_name".into(), json!(l.event_name));
    }
    let mut payload = json!({ "message": message });
    if !fields.is_empty() {
        payload["fields"] = clip_value(&Value::Object(fields));
    }
    let level = match l.severity_number {
        1..=4 => Some(Level::Trace),
        5..=8 => Some(Level::Debug),
        9..=12 => Some(Level::Info),
        13..=16 => Some(Level::Warn),
        17..=20 => Some(Level::Error),
        21..=24 => Some(Level::Fatal),
        _ => Level::parse(&l.severity_text.to_ascii_lowercase()),
    };
    IncomingEnvelope {
        v: None,
        id: None,
        ts: (l.time_ns > 0).then(|| ts(l.time_ns)),
        run: l.trace_id.as_ref().map(|t| format!("otel-{t}")),
        parent: l.span_id.as_ref().map(|s| format!("otel-{s}")),
        project: str_attr(&l.resource, &["service.name"]).map(str::to_owned),
        origin: origin(&l.resource, &l.attrs, &l.scope),
        kind: "core.log".into(),
        payload,
        meta: Meta {
            level,
            label: (!l.event_name.is_empty()).then(|| l.event_name.clone()),
            ..Meta::default()
        },
    }
}

/// GenAI semantic conventions → `llm.call` (or agent kinds for tool and agent operations).
fn gen_ai(s: &Span) -> (String, Value, Option<String>, Option<Level>) {
    let a = &s.attrs;
    let op = str_attr(a, &["gen_ai.operation.name"]).unwrap_or("chat");
    match op {
        "execute_tool" => {
            let tool = str_attr(a, &["gen_ai.tool.name"]).unwrap_or("tool");
            let mut p = json!({ "tool": tool, "duration_ms": duration_ms(s), "status": if s.status_code == 2 { "error" } else { "done" } });
            put(&mut p, "args", a.get("gen_ai.tool.call.arguments").map(parse_json));
            put(&mut p, "result", a.get("gen_ai.tool.call.result").map(parse_json));
            put(&mut p, "tool_use_id", a.get("gen_ai.tool.call.id").cloned());
            ("agent.tool_call".into(), p, Some(tool.to_owned()), error_level(s))
        }
        "invoke_agent" | "create_agent" => {
            let name = str_attr(a, &["gen_ai.agent.name"]).unwrap_or("agent");
            let mut p = json!({ "status": op, "agent": name, "duration_ms": duration_ms(s) });
            put(&mut p, "model", a.get("gen_ai.request.model").cloned());
            ("agent.step".into(), p, Some(name.to_owned()), error_level(s))
        }
        _ => {
            let model = str_attr(a, &["gen_ai.request.model", "gen_ai.response.model"]);
            let (input, output) = messages(s);
            let mut usage = Map::new();
            for (key, names) in [
                ("input", &["gen_ai.usage.input_tokens", "gen_ai.usage.prompt_tokens", "llm.usage.prompt_tokens"][..]),
                ("output", &["gen_ai.usage.output_tokens", "gen_ai.usage.completion_tokens", "llm.usage.completion_tokens"][..]),
                (
                    "cache_read",
                    &["gen_ai.usage.cache_read.input_tokens", "gen_ai.usage.cache_read_input_tokens", "gen_ai.usage.cached_tokens"][..],
                ),
                (
                    "cache_write",
                    &["gen_ai.usage.cache_creation.input_tokens", "gen_ai.usage.cache_creation_input_tokens"][..],
                ),
                ("total", &["llm.usage.total_tokens", "gen_ai.usage.total_tokens"][..]),
            ] {
                if let Some(v) = names.iter().find_map(|n| a.get(*n)).and_then(as_number) {
                    usage.insert(key.into(), v);
                }
            }
            let mut p = json!({
                "operation": op,
                "provider": str_attr(a, &["gen_ai.provider.name", "gen_ai.system"]),
                "model": model,
                "messages": input,
                "output": output,
                "usage": usage,
                "latency_ms": duration_ms(s),
            });
            put(&mut p, "response_model", a.get("gen_ai.response.model").cloned());
            put(&mut p, "response_id", a.get("gen_ai.response.id").cloned());
            put(&mut p, "stop_reason", a.get("gen_ai.response.finish_reasons").cloned());
            put(&mut p, "system", a.get("gen_ai.system_instructions").map(parse_json));
            put(&mut p, "tools", a.get("gen_ai.tool.definitions").map(parse_json));
            put(&mut p, "cost_usd", ["gen_ai.usage.cost", "llm.usage.cost", "gen_ai.cost"].iter().find_map(|n| a.get(*n)).and_then(as_number));
            let params: Map<String, Value> = a
                .iter()
                .filter_map(|(k, v)| k.strip_prefix("gen_ai.request.").filter(|k| *k != "model").map(|k| (k.to_owned(), v.clone())))
                .collect();
            if !params.is_empty() {
                p["params"] = Value::Object(params);
            }
            if s.status_code == 2 {
                p["error"] = json!(if s.status_message.is_empty() { "error" } else { &s.status_message });
            }
            ("llm.call".into(), clip_value(&p), model.map(str::to_owned), error_level(s))
        }
    }
}

/// Input and output messages, from whichever convention the instrumentation used.
fn messages(s: &Span) -> (Value, Value) {
    let a = &s.attrs;
    // 1. Current semconv: JSON in gen_ai.input.messages / gen_ai.output.messages.
    if a.contains_key("gen_ai.input.messages") || a.contains_key("gen_ai.output.messages") {
        return (
            a.get("gen_ai.input.messages").map(parse_json).unwrap_or(json!([])),
            a.get("gen_ai.output.messages").map(parse_json).unwrap_or(json!([])),
        );
    }
    // 2. Span events: gen_ai.user.message, gen_ai.system.message, …, gen_ai.choice.
    let mut input = Vec::new();
    let mut output = Vec::new();
    for e in &s.events {
        if let Some(role) = e.name.strip_prefix("gen_ai.").and_then(|r| r.strip_suffix(".message")) {
            let content = e.attrs.get("content").map(parse_json).or_else(|| e.attrs.get("gen_ai.event.content").map(parse_json));
            input.push(json!({ "role": role, "content": content.unwrap_or(Value::Null) }));
        } else if e.name == "gen_ai.choice" {
            let msg = e.attrs.get("message").map(parse_json).unwrap_or_else(|| json!({ "content": e.attrs.get("content").cloned() }));
            let mut m = json!({ "role": "assistant" });
            if let Value::Object(o) = msg {
                for (k, v) in o {
                    m[k] = v;
                }
            }
            put(&mut m, "finish_reason", e.attrs.get("finish_reason").cloned());
            output.push(m);
        }
    }
    if !input.is_empty() || !output.is_empty() {
        return (Value::Array(input), Value::Array(output));
    }
    // 3. OpenLLMetry: gen_ai.prompt.<i>.role / .content, gen_ai.completion.<i>.role / .content.
    (indexed(a, "gen_ai.prompt."), indexed(a, "gen_ai.completion."))
}

fn indexed(a: &Map<String, Value>, prefix: &str) -> Value {
    let mut by_index: BTreeMap<u32, Map<String, Value>> = BTreeMap::new();
    for (k, v) in a {
        let Some(rest) = k.strip_prefix(prefix) else { continue };
        let Some((i, field)) = rest.split_once('.') else { continue };
        let Ok(i) = i.parse::<u32>() else { continue };
        by_index.entry(i).or_default().insert(field.to_owned(), v.clone());
    }
    Value::Array(by_index.into_values().map(Value::Object).collect())
}

fn envelope(s: &Span, kind: &str, payload: Value) -> IncomingEnvelope {
    IncomingEnvelope {
        v: None,
        id: None,
        ts: (s.start_ns > 0).then(|| ts(s.start_ns)),
        run: Some(format!("otel-{}", s.trace_id)),
        parent: s.parent.as_ref().map(|p| format!("otel-{p}")),
        project: str_attr(&s.resource, &["service.name"]).map(str::to_owned),
        origin: origin(&s.resource, &s.attrs, &s.scope),
        kind: kind.into(),
        payload,
        meta: Meta::default(),
    }
}

fn origin(resource: &Map<String, Value>, attrs: &Map<String, Value>, scope: &str) -> Origin {
    let r = |keys: &[&str]| str_attr(resource, keys).map(str::to_owned);
    let mut extra = Map::new();
    extra.insert("via".into(), json!("otlp"));
    if !scope.is_empty() {
        extra.insert("scope".into(), json!(scope));
    }
    let runtime = match (r(&["process.runtime.name"]), r(&["process.runtime.version"])) {
        (Some(n), Some(v)) => Some(format!("{n} {v}")),
        (n, v) => n.or(v),
    };
    Origin {
        lang: r(&["telemetry.sdk.language"]),
        runtime,
        sdk: r(&["telemetry.sdk.version"]).map(|v| format!("{}/{v}", r(&["telemetry.sdk.name"]).unwrap_or_else(|| "opentelemetry".into()))),
        file: str_attr(attrs, &["code.file.path", "code.filepath"]).map(str::to_owned),
        line: attrs
            .get("code.line.number")
            .or_else(|| attrs.get("code.lineno"))
            .and_then(Value::as_u64)
            .and_then(|l| u32::try_from(l).ok()),
        function: str_attr(attrs, &["code.function.name", "code.function"]).map(str::to_owned),
        host: r(&["host.name"]),
        pid: resource.get("process.pid").and_then(Value::as_u64).and_then(|p| u32::try_from(p).ok()),
        extra,
        ..Origin::default()
    }
}

/// Frames from a textual stack trace, innermost first. Understands Python tracebacks and
/// `at fn (file:line:col)` lines (JS, Java, C#-ish); anything else yields no frames.
pub(crate) fn frames_from_text(text: &str) -> Vec<Value> {
    let mut python = Vec::new();
    let mut at = Vec::new();
    for line in text.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("File \"")
            && let Some((file, rest)) = rest.split_once("\", line ")
        {
            let (num, func) = rest.split_once(", in ").unwrap_or((rest, ""));
            if let Ok(n) = num.trim().parse::<u64>() {
                python.push(frame(file, n, func.trim()));
            }
        } else if let Some(rest) = t.strip_prefix("at ") {
            let (func, loc) = match rest.rsplit_once(" (") {
                Some((f, l)) => (f, l.trim_end_matches(')')),
                None => ("", rest),
            };
            let mut parts = loc.rsplitn(3, ':');
            let (_col, line_no, file) = (parts.next(), parts.next(), parts.next());
            if let (Some(file), Some(n)) = (file, line_no.and_then(|n| n.parse::<u64>().ok())) {
                at.push(frame(file, n, func));
            }
        }
    }
    python.reverse(); // Python prints the innermost frame last.
    if python.is_empty() { at } else { python }
}

fn frame(file: &str, line: u64, func: &str) -> Value {
    let vendor = ["site-packages", "dist-packages", "node_modules", "/lib/python", "node:"].iter().any(|v| file.contains(v));
    let mut f = json!({ "file": file, "line": line, "vendor": vendor });
    if !func.is_empty() {
        f["fn"] = json!(func);
    }
    f
}

fn str_attr<'a>(m: &'a Map<String, Value>, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|k| m.get(*k).and_then(Value::as_str)).filter(|s| !s.is_empty())
}

fn as_number(v: &Value) -> Option<Value> {
    match v {
        Value::Number(_) => Some(v.clone()),
        Value::String(s) => s.parse::<f64>().ok().and_then(serde_json::Number::from_f64).map(Value::Number),
        _ => None,
    }
}

/// Attribute values holding JSON as a string (common for messages) become structured.
fn parse_json(v: &Value) -> Value {
    match v {
        Value::String(s) if s.trim_start().starts_with(['[', '{']) => serde_json::from_str(s).unwrap_or_else(|_| v.clone()),
        other => other.clone(),
    }
}

fn put(p: &mut Value, k: &str, v: Option<Value>) {
    if let Some(v) = v.filter(|v| !v.is_null()) {
        p[k] = v;
    }
}

fn duration_ms(s: &Span) -> Value {
    if s.end_ns >= s.start_ns && s.start_ns > 0 {
        json!(((s.end_ns - s.start_ns) as f64 / 1e6 * 1000.0).round() / 1000.0)
    } else {
        Value::Null
    }
}

fn error_level(s: &Span) -> Option<Level> {
    (s.status_code == 2).then_some(Level::Error)
}

fn ts(ns: u64) -> DateTime<Utc> {
    let secs = i64::try_from(ns / 1_000_000_000).unwrap_or(0);
    let nanos = u32::try_from(ns % 1_000_000_000).unwrap_or(0);
    DateTime::from_timestamp(secs, nanos).unwrap_or_default()
}
