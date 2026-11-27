//! Compact text projections of events: what an agent reads first. One event → one line, a
//! few hundred bytes at most, so a page of 30 events costs little context.

use glint_protocol::{Envelope, Level};
use serde_json::Value;

const SUMMARY_MAX: usize = 160;

/// `<id> 10:15:03.123 core.exception [error] «label» cart.ts:88 — TypeError: …`
pub fn line(e: &Envelope) -> String {
    let mut s = format!("{} {} {}", e.id, e.ts.format("%H:%M:%S%.3f"), e.kind);
    let level = e.level();
    if level >= Level::Warn {
        s.push_str(&format!(" [{}]", level.as_str()));
    }
    if let Some(l) = e.meta.label.as_deref().filter(|l| !l.is_empty()) {
        s.push_str(&format!(" «{}»", clip(l, 40)));
    }
    if let Some(f) = &e.origin.file {
        s.push(' ');
        s.push_str(&short_path(f));
        if let Some(l) = e.origin.line {
            s.push_str(&format!(":{l}"));
        }
    }
    let sum = summary(e);
    if !sum.is_empty() {
        s.push_str(" — ");
        s.push_str(&sum);
    }
    if let Some(note) = e.meta.extra.get("note").and_then(Value::as_str) {
        s.push_str(&format!(" (note: {})", clip(note, 80)));
    }
    s
}

/// One line of what the event says, by kind.
pub fn summary(e: &Envelope) -> String {
    let p = &e.payload;
    let s = |k: &str| p.get(k).and_then(Value::as_str).unwrap_or("");
    let text = match e.kind.as_str() {
        "core.value" => compact(p.get("value").unwrap_or(p)),
        "core.log" => {
            let mut t = s("message").to_owned();
            if let Some(f) = p.get("fields").filter(|f| f.as_object().is_some_and(|o| !o.is_empty())) {
                t.push(' ');
                t.push_str(&compact(f));
            }
            t
        }
        "core.text" | "core.markdown" | "core.mermaid" | "core.html" => s("content").to_owned(),
        "core.json" => compact(p.get("content").unwrap_or(p)),
        "core.exception" => {
            let at = first_user_frame(p).map(|f| format!(" @ {f}")).unwrap_or_default();
            format!("{}: {}{at}", s("class"), s("message"))
        }
        "core.trace" => format!("{} frames", p.get("frames").and_then(Value::as_array).map_or(0, Vec::len)),
        "core.table" => format!("table, {} rows", p.get("rows").and_then(Value::as_array).map_or(0, Vec::len)),
        "core.measure" => format!("{} {} ms", s("name"), p.get("duration_ms").map(compact).unwrap_or_default()),
        "sql.query" => {
            let d = p.get("duration_ms").filter(|v| v.is_number()).map(|v| format!(" ({} ms)", compact(v)));
            format!("{}{}", s("sql"), d.unwrap_or_default())
        }
        "core.update" => format!("update → {} {}", s("target"), compact_without(p, "target")),
        "ctl.pause" => format!("PAUSED {}", s("reason")),
        "llm.call" => llm_summary(p),
        "core.span" => format!(
            "{} {} ms {}",
            s("name"),
            p.get("duration_ms").map(compact).unwrap_or_default(),
            if s("status") == "error" { "ERROR" } else { "" }
        ),
        "agent.tool_call" => format!("{} {}", s("tool"), tool_args(p.get("args"))),
        "agent.tool_result" => {
            let what = if p.get("ok") == Some(&Value::Bool(false)) { "FAILED" } else { "ok" };
            let detail = p.get("error").or_else(|| p.get("result")).map(compact).unwrap_or_default();
            format!("{} {what} {detail}", s("tool"))
        }
        "agent.permission" => format!("{} waiting for approval {}", s("tool"), tool_args(p.get("args"))),
        "agent.step" => format!("{} {}", s("status"), [s("input"), s("output")].concat()),
        "agent.session" => format!("{} {}", s("status"), [s("source"), s("reason")].concat()),
        "ctl.resume" => format!("{} → {}", s("action"), s("target")),
        "core.separator" | "core.clear" => String::new(),
        "core.screen" => format!("screen {}", s("name")),
        _ => compact(p),
    };
    clip(&text.replace(['\n', '\r'], " ⏎ "), SUMMARY_MAX)
}

/// `claude-sonnet-5-5 812→64 tok 1250 ms $0.004 — What is 2+2? ⇒ 4`
fn llm_summary(p: &Value) -> String {
    let u = p.get("usage");
    let n = |k: &str| u.and_then(|u| u.get(k)).map(compact);
    let mut s = p.get("model").and_then(Value::as_str).unwrap_or("llm").to_owned();
    if let (Some(i), Some(o)) = (n("input"), n("output")) {
        s.push_str(&format!(" {i}→{o} tok"));
    }
    if let Some(l) = p.get("latency_ms").filter(|v| v.is_number()) {
        s.push_str(&format!(" {} ms", compact(l)));
    }
    if let Some(c) = p.get("cost_usd").filter(|v| v.is_number()) {
        s.push_str(&format!(" ${}", compact(c)));
    }
    if let Some(e) = p.get("error").and_then(Value::as_str) {
        return format!("{s} ERROR {e}");
    }
    let last_user = p
        .get("messages")
        .and_then(Value::as_array)
        .and_then(|m| m.iter().rev().find(|x| x.get("role").and_then(Value::as_str) == Some("user")))
        .map(message_text)
        .unwrap_or_default();
    let answer = p.get("output").and_then(Value::as_array).and_then(|m| m.first()).map(message_text).unwrap_or_default();
    format!("{s} — {last_user} ⇒ {answer}")
}

/// The text of a chat message in any of the shapes instrumentations use.
pub fn message_text(m: &Value) -> String {
    fn of(c: &Value) -> String {
        match c {
            Value::String(s) => s.clone(),
            Value::Array(parts) => parts
                .iter()
                .map(|p| match p {
                    Value::String(s) => s.clone(),
                    other => other
                        .get("content")
                        .or_else(|| other.get("text"))
                        .map(of)
                        .unwrap_or_else(|| other.get("type").and_then(Value::as_str).map(|t| format!("[{t}]")).unwrap_or_default()),
                })
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(" "),
            Value::Null => String::new(),
            other => other.to_string(),
        }
    }
    m.get("parts").or_else(|| m.get("content")).map(of).unwrap_or_default()
}

/// The argument that says most about a tool call: a command, a path, a pattern.
fn tool_args(args: Option<&Value>) -> String {
    let Some(a) = args else { return String::new() };
    for k in ["command", "file_path", "path", "pattern", "url", "query", "description", "prompt"] {
        if let Some(v) = a.get(k).and_then(Value::as_str) {
            return v.to_owned();
        }
    }
    compact(a)
}

/// `fn (file:line)` of the innermost frame not marked as vendor code.
pub fn first_user_frame(p: &Value) -> Option<String> {
    let frames = p.get("frames")?.as_array()?;
    let f = frames.iter().find(|f| f.get("vendor") != Some(&Value::Bool(true))).or(frames.first())?;
    Some(frame(f))
}

pub fn frame(f: &Value) -> String {
    let file = f.get("file").and_then(Value::as_str).map(short_path).unwrap_or_else(|| "?".into());
    let line = f.get("line").and_then(Value::as_u64).map(|l| format!(":{l}")).unwrap_or_default();
    let func = match (f.get("class").and_then(Value::as_str), f.get("fn").and_then(Value::as_str)) {
        (Some(c), Some(m)) => format!("{c}::{m} "),
        (None, Some(m)) => format!("{m} "),
        _ => String::new(),
    };
    let vendor = if f.get("vendor") == Some(&Value::Bool(true)) { " [vendor]" } else { "" };
    format!("{func}({file}{line}){vendor}")
}

pub fn compact(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn compact_without(v: &Value, key: &str) -> String {
    match v {
        Value::Object(m) => {
            let mut m = m.clone();
            m.remove(key);
            Value::Object(m).to_string()
        }
        other => other.to_string(),
    }
}

/// Last three path segments: enough to recognise a file, short enough for a line.
pub fn short_path(p: &str) -> String {
    let parts: Vec<&str> = p.split(['/', '\\']).filter(|s| !s.is_empty()).collect();
    if parts.len() > 3 { format!("…/{}", parts[parts.len() - 3..].join("/")) } else { p.to_owned() }
}

pub fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use glint_core::Normaliser;
    use glint_protocol::{BodyFormat, parse_body};

    fn ev(json: &str) -> Envelope {
        let inc = parse_body(json.as_bytes(), BodyFormat::Json).unwrap().remove(0).unwrap();
        Normaliser::new().normalise(inc, chrono::Utc::now())
    }

    #[test]
    fn exception_line_points_at_user_code() {
        let e = ev(r#"{"id":"E1","kind":"core.exception","payload":{"class":"TypeError","message":"x is undefined",
            "frames":[{"file":"/app/node_modules/react/index.js","line":1,"vendor":true},{"file":"/srv/app/src/cart.ts","line":88,"fn":"renderTotal"}]},
            "origin":{"file":"/srv/app/src/cart.ts","line":88},"meta":{"label":"checkout"}}"#);
        assert_eq!(
            line(&e).split_once(' ').unwrap().1.split_once(' ').unwrap().1,
            "core.exception [error] «checkout» …/app/src/cart.ts:88 — TypeError: x is undefined @ renderTotal (…/app/src/cart.ts:88)"
        );
    }

    #[test]
    fn summaries_are_single_line_and_clipped() {
        let e = ev(&format!(r#"{{"kind":"core.log","payload":{{"message":"a\nb {}"}}}}"#, "x".repeat(400)));
        let s = summary(&e);
        assert!(!s.contains('\n'));
        assert!(s.chars().count() <= SUMMARY_MAX);
        assert!(s.starts_with("a ⏎ b"));
    }

    #[test]
    fn sql_and_values() {
        assert_eq!(summary(&ev(r#"{"kind":"sql.query","payload":{"sql":"select 1","duration_ms":3.4}}"#)), "select 1 (3.4 ms)");
        assert_eq!(summary(&ev(r#"{"kind":"core.value","payload":{"value":{"a":1}}}"#)), r#"{"a":1}"#);
        assert_eq!(summary(&ev(r#"{"kind":"core.value","payload":{"value":"hi"}}"#)), "hi");
    }
}
