//! OTLP/HTTP bodies (protobuf or JSON) → one plain model, so mapping is written once.

use base64::Engine;
use prost::Message;
use serde_json::{Map, Number, Value};

use super::proto::{self, any_value};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Span {
    pub trace_id: String,
    pub span_id: String,
    pub parent: Option<String>,
    pub name: String,
    pub kind: i64,
    pub start_ns: u64,
    pub end_ns: u64,
    pub attrs: Map<String, Value>,
    pub events: Vec<SpanEvent>,
    /// 0 unset, 1 ok, 2 error.
    pub status_code: i64,
    pub status_message: String,
    pub resource: Map<String, Value>,
    pub scope: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SpanEvent {
    pub name: String,
    pub time_ns: u64,
    pub attrs: Map<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct LogRec {
    pub time_ns: u64,
    pub severity_number: i64,
    pub severity_text: String,
    pub body: Value,
    pub attrs: Map<String, Value>,
    pub trace_id: Option<String>,
    pub span_id: Option<String>,
    pub event_name: String,
    pub resource: Map<String, Value>,
    pub scope: String,
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum OtlpError {
    #[error("not a valid OTLP protobuf message: {0}")]
    Protobuf(String),
    #[error("not valid OTLP JSON: {0}")]
    Json(String),
    #[error("unsupported content type `{0}` (use application/x-protobuf or application/json)")]
    ContentType(String),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Encoding {
    Protobuf,
    Json,
}

impl Encoding {
    pub fn from_content_type(ct: Option<&str>) -> Result<Self, OtlpError> {
        let ct = ct.unwrap_or("application/x-protobuf").split(';').next().unwrap_or("").trim();
        match ct {
            "application/x-protobuf" | "application/protobuf" | "" => Ok(Encoding::Protobuf),
            "application/json" => Ok(Encoding::Json),
            other => Err(OtlpError::ContentType(other.to_owned())),
        }
    }
}

// ------------------------------------------------------------------ protobuf

pub fn traces(body: &[u8], enc: Encoding) -> Result<Vec<Span>, OtlpError> {
    match enc {
        Encoding::Protobuf => {
            let req = proto::ExportTraceServiceRequest::decode(body).map_err(|e| OtlpError::Protobuf(e.to_string()))?;
            let mut out = Vec::new();
            for rs in req.resource_spans {
                let resource = kvs(rs.resource.map(|r| r.attributes).unwrap_or_default());
                for ss in rs.scope_spans {
                    let scope = ss.scope.map(|s| s.name).unwrap_or_default();
                    for s in ss.spans {
                        out.push(Span {
                            trace_id: hex(&s.trace_id),
                            span_id: hex(&s.span_id),
                            parent: (!s.parent_span_id.is_empty()).then(|| hex(&s.parent_span_id)),
                            name: s.name,
                            kind: i64::from(s.kind),
                            start_ns: s.start_time_unix_nano,
                            end_ns: s.end_time_unix_nano,
                            attrs: kvs(s.attributes),
                            events: s
                                .events
                                .into_iter()
                                .map(|e| SpanEvent { name: e.name, time_ns: e.time_unix_nano, attrs: kvs(e.attributes) })
                                .collect(),
                            status_code: s.status.as_ref().map_or(0, |st| i64::from(st.code)),
                            status_message: s.status.map(|st| st.message).unwrap_or_default(),
                            resource: resource.clone(),
                            scope: scope.clone(),
                        });
                    }
                }
            }
            Ok(out)
        }
        Encoding::Json => json_traces(body),
    }
}

pub fn logs(body: &[u8], enc: Encoding) -> Result<Vec<LogRec>, OtlpError> {
    match enc {
        Encoding::Protobuf => {
            let req = proto::ExportLogsServiceRequest::decode(body).map_err(|e| OtlpError::Protobuf(e.to_string()))?;
            let mut out = Vec::new();
            for rl in req.resource_logs {
                let resource = kvs(rl.resource.map(|r| r.attributes).unwrap_or_default());
                for sl in rl.scope_logs {
                    let scope = sl.scope.map(|s| s.name).unwrap_or_default();
                    for l in sl.log_records {
                        out.push(LogRec {
                            time_ns: if l.time_unix_nano > 0 { l.time_unix_nano } else { l.observed_time_unix_nano },
                            severity_number: i64::from(l.severity_number),
                            severity_text: l.severity_text,
                            body: l.body.map(any).unwrap_or(Value::Null),
                            attrs: kvs(l.attributes),
                            trace_id: (!l.trace_id.is_empty()).then(|| hex(&l.trace_id)),
                            span_id: (!l.span_id.is_empty()).then(|| hex(&l.span_id)),
                            event_name: l.event_name,
                            resource: resource.clone(),
                            scope: scope.clone(),
                        });
                    }
                }
            }
            Ok(out)
        }
        Encoding::Json => json_logs(body),
    }
}

fn kvs(list: Vec<proto::KeyValue>) -> Map<String, Value> {
    list.into_iter().map(|kv| (kv.key, kv.value.map(any).unwrap_or(Value::Null))).collect()
}

fn any(v: proto::AnyValue) -> Value {
    match v.value {
        None => Value::Null,
        Some(any_value::Value::StringValue(s)) => Value::String(s),
        Some(any_value::Value::BoolValue(b)) => Value::Bool(b),
        Some(any_value::Value::IntValue(i)) => Value::from(i),
        Some(any_value::Value::DoubleValue(d)) => Number::from_f64(d).map_or_else(|| Value::String(d.to_string()), Value::Number),
        Some(any_value::Value::ArrayValue(a)) => Value::Array(a.values.into_iter().map(any).collect()),
        Some(any_value::Value::KvlistValue(l)) => Value::Object(kvs(l.values)),
        Some(any_value::Value::BytesValue(b)) => serde_json::json!({ "$bin": base64::engine::general_purpose::STANDARD.encode(b) }),
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

// ------------------------------------------------------------------ JSON

/// Field lookup accepting the spec's lowerCamelCase and the snake_case some senders use.
fn field<'a>(v: &'a Value, camel: &str) -> Option<&'a Value> {
    v.get(camel).or_else(|| {
        let mut snake = String::new();
        for c in camel.chars() {
            if c.is_ascii_uppercase() {
                snake.push('_');
                snake.push(c.to_ascii_lowercase());
            } else {
                snake.push(c);
            }
        }
        v.get(&snake)
    })
}

fn arr<'a>(v: &'a Value, k: &str) -> &'a [Value] {
    field(v, k).and_then(Value::as_array).map_or(&[], Vec::as_slice)
}

fn text(v: &Value, k: &str) -> String {
    field(v, k).and_then(Value::as_str).unwrap_or("").to_owned()
}

/// OTLP JSON encodes 64-bit integers as strings.
fn uint(v: &Value, k: &str) -> u64 {
    match field(v, k) {
        Some(Value::String(s)) => s.parse().unwrap_or(0),
        Some(Value::Number(n)) => n.as_u64().unwrap_or(0),
        _ => 0,
    }
}

fn int(v: &Value, k: &str) -> i64 {
    match field(v, k) {
        Some(Value::String(s)) => s.parse().unwrap_or(0),
        Some(Value::Number(n)) => n.as_i64().unwrap_or(0),
        _ => 0,
    }
}

fn json_kvs(list: &[Value]) -> Map<String, Value> {
    list.iter()
        .filter_map(|kv| Some((kv.get("key")?.as_str()?.to_owned(), kv.get("value").map(json_any).unwrap_or(Value::Null))))
        .collect()
}

fn json_any(v: &Value) -> Value {
    if let Some(s) = field(v, "stringValue") {
        return s.clone();
    }
    if let Some(b) = field(v, "boolValue") {
        return b.clone();
    }
    if let Some(i) = field(v, "intValue") {
        return match i {
            Value::String(s) => s.parse::<i64>().map_or_else(|_| i.clone(), Value::from),
            other => other.clone(),
        };
    }
    if let Some(d) = field(v, "doubleValue") {
        return d.clone();
    }
    if let Some(a) = field(v, "arrayValue") {
        return Value::Array(arr(a, "values").iter().map(json_any).collect());
    }
    if let Some(l) = field(v, "kvlistValue") {
        return Value::Object(json_kvs(arr(l, "values")));
    }
    if let Some(b) = field(v, "bytesValue") {
        return serde_json::json!({ "$bin": b });
    }
    Value::Null
}

/// JSON trace and span ids are hex strings already (a few senders use base64; keep as given).
fn id(v: &Value, k: &str) -> String {
    text(v, k).to_ascii_lowercase()
}

fn json_traces(body: &[u8]) -> Result<Vec<Span>, OtlpError> {
    let root: Value = serde_json::from_slice(body).map_err(|e| OtlpError::Json(e.to_string()))?;
    let mut out = Vec::new();
    for rs in arr(&root, "resourceSpans") {
        let resource = field(rs, "resource").map(|r| json_kvs(arr(r, "attributes"))).unwrap_or_default();
        for ss in arr(rs, "scopeSpans") {
            let scope = field(ss, "scope").map(|s| text(s, "name")).unwrap_or_default();
            for s in arr(ss, "spans") {
                let parent = id(s, "parentSpanId");
                out.push(Span {
                    trace_id: id(s, "traceId"),
                    span_id: id(s, "spanId"),
                    parent: (!parent.is_empty()).then_some(parent),
                    name: text(s, "name"),
                    kind: int(s, "kind"),
                    start_ns: uint(s, "startTimeUnixNano"),
                    end_ns: uint(s, "endTimeUnixNano"),
                    attrs: json_kvs(arr(s, "attributes")),
                    events: arr(s, "events")
                        .iter()
                        .map(|e| SpanEvent { name: text(e, "name"), time_ns: uint(e, "timeUnixNano"), attrs: json_kvs(arr(e, "attributes")) })
                        .collect(),
                    status_code: field(s, "status").map_or(0, |st| int(st, "code")),
                    status_message: field(s, "status").map(|st| text(st, "message")).unwrap_or_default(),
                    resource: resource.clone(),
                    scope: scope.clone(),
                });
            }
        }
    }
    Ok(out)
}

fn json_logs(body: &[u8]) -> Result<Vec<LogRec>, OtlpError> {
    let root: Value = serde_json::from_slice(body).map_err(|e| OtlpError::Json(e.to_string()))?;
    let mut out = Vec::new();
    for rl in arr(&root, "resourceLogs") {
        let resource = field(rl, "resource").map(|r| json_kvs(arr(r, "attributes"))).unwrap_or_default();
        for sl in arr(rl, "scopeLogs") {
            let scope = field(sl, "scope").map(|s| text(s, "name")).unwrap_or_default();
            for l in arr(sl, "logRecords") {
                let t = uint(l, "timeUnixNano");
                let trace = id(l, "traceId");
                let span = id(l, "spanId");
                out.push(LogRec {
                    time_ns: if t > 0 { t } else { uint(l, "observedTimeUnixNano") },
                    severity_number: int(l, "severityNumber"),
                    severity_text: text(l, "severityText"),
                    body: field(l, "body").map(json_any).unwrap_or(Value::Null),
                    attrs: json_kvs(arr(l, "attributes")),
                    trace_id: (!trace.is_empty()).then_some(trace),
                    span_id: (!span.is_empty()).then_some(span),
                    event_name: text(l, "eventName"),
                    resource: resource.clone(),
                    scope: scope.clone(),
                });
            }
        }
    }
    Ok(out)
}
