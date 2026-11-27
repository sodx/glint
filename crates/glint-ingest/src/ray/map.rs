//! One Ray payload → one Glint kind. The table in `docs/mappings/ray.md` mirrors this file.

use glint_protocol::{Level, Meta, is_valid_kind};
use serde_json::{Map, Value, json};

// Short-lived (one per Ray payload, consumed at once), so the size gap between variants is moot.
#[allow(clippy::large_enum_variant)]
pub(super) enum Mapped {
    /// A feed item.
    Event { kind: String, payload: Value, meta: Meta },
    /// A change to the entries of this uuid: `meta`, `hidden`, `removed` keys of `core.update`.
    Update(Map<String, Value>),
    /// Meaningful only to Ray's own window (confetti, show_app, …).
    Ignore,
}

fn event(kind: &str, payload: Value) -> Mapped {
    Mapped::Event {
        kind: kind.to_owned(),
        payload,
        meta: Meta::default(),
    }
}

fn event_with(kind: &str, payload: Value, label: Option<&str>, level: Option<Level>) -> Mapped {
    Mapped::Event {
        kind: kind.to_owned(),
        payload,
        meta: Meta {
            label: label.filter(|l| !l.is_empty()).map(str::to_owned),
            level,
            ..Meta::default()
        },
    }
}

fn update(key: &str, value: Value) -> Mapped {
    let mut m = Map::new();
    if key == "hidden" || key == "removed" {
        m.insert(key.into(), value);
    } else {
        m.insert("meta".into(), json!({ key: value }));
    }
    Mapped::Update(m)
}

fn str_of<'a>(c: &'a Value, k: &str) -> Option<&'a str> {
    c.get(k).and_then(Value::as_str)
}

pub(super) fn map_payload(ty: &str, c: Value) -> Mapped {
    match ty {
        "log" => map_log(&c),
        "custom" => map_custom(&c),
        "json_string" => {
            let raw = str_of(&c, "value").unwrap_or("");
            match serde_json::from_str::<Value>(raw) {
                Ok(v) => event("core.json", json!({ "content": v })),
                Err(_) => event("core.text", json!({ "content": raw })),
            }
        }
        "table" => {
            let rows: Vec<Value> = c
                .get("values")
                .and_then(Value::as_object)
                .map(|m| m.iter().map(|(k, v)| json!([k, cell(v)])).collect())
                .unwrap_or_default();
            event_with(
                "core.table",
                json!({ "columns": ["key", "value"], "rows": rows }),
                // `ray()->table()` labels every table "Table"; only custom labels say something.
                str_of(&c, "label").filter(|l| *l != "Table"),
                None,
            )
        }
        "carbon" => event("core.value", json!({ "value": c, "type": "Carbon" })),
        "exception" => event(
            "core.exception",
            json!({
                "class": str_of(&c, "class").unwrap_or("Exception"),
                "message": str_of(&c, "message").unwrap_or(""),
                "frames": frames(c.get("frames")),
            }),
        ),
        "trace" => event("core.trace", json!({ "frames": frames(c.get("frames")) })),
        "caller" => event_with(
            "core.trace",
            json!({ "frames": frames(c.get("frame").map(|f| Value::Array(vec![f.clone()])).as_ref()) }),
            Some("caller"),
            None,
        ),
        "measure" => {
            let new_timer = c.get("is_new_timer").and_then(Value::as_bool).unwrap_or(false);
            event_with(
                "core.measure",
                json!({
                    "name": str_of(&c, "name").unwrap_or("measure"),
                    "duration_ms": c.get("total_time").cloned().unwrap_or(Value::Null),
                    "memory_bytes": c.get("max_memory_usage_during_total_time").cloned().unwrap_or(Value::Null),
                    "since_last_ms": c.get("time_since_last_call").cloned().unwrap_or(Value::Null),
                    "new_timer": new_timer,
                }),
                new_timer.then_some("timer started"),
                None,
            )
        }
        "application_log" => {
            let message = match c.get("value") {
                Some(Value::String(s)) => s.clone(),
                Some(v) => v.to_string(),
                None => String::new(),
            };
            let mut p = json!({ "message": message });
            if let Some(ctx) = c.get("context").filter(|v| !v.is_null()) {
                p["fields"] = ctx.clone();
            }
            event("core.log", p)
        }
        "executed_query" => event(
            "sql.query",
            json!({
                "sql": str_of(&c, "sql").unwrap_or(""),
                "bindings": c.get("bindings").cloned().unwrap_or_else(|| json!([])),
                "conn": c.get("connection_name").cloned().unwrap_or(Value::Null),
                "duration_ms": c.get("time").cloned().unwrap_or(Value::Null),
            }),
        ),
        "separator" => event("core.separator", json!({})),
        "new_screen" => match str_of(&c, "name").filter(|n| !n.is_empty()) {
            Some(name) => event("core.screen", json!({ "name": name })),
            None => event("core.clear", json!({})),
        },
        "clear_all" => event("core.clear", json!({})),
        "notify" => event_with(
            "core.log",
            json!({ "message": c.get("value").map(text_of).unwrap_or_default() }),
            Some("notify"),
            Some(Level::Info),
        ),
        "create_lock" => event_with(
            "ctl.pause",
            json!({ "reason": "ray()->pause()", "lock": str_of(&c, "name").unwrap_or("") }),
            None,
            None,
        ),
        "ai_custom_markdown" => event_with(
            "core.markdown",
            json!({ "content": str_of(&c, "markdown").unwrap_or("") }),
            str_of(&c, "label"),
            None,
        ),
        "ai_custom_mermaid" => event_with(
            "core.mermaid",
            json!({ "content": str_of(&c, "mermaid").unwrap_or("") }),
            str_of(&c, "label"),
            None,
        ),
        "ai_custom_html" => {
            let mut p = json!({ "content": str_of(&c, "html").unwrap_or("") });
            if let Some(of) = c.get("iterationOf").filter(|v| !v.is_null()) {
                p["iteration_of"] = of.clone();
            }
            event_with("core.html", p, str_of(&c, "label"), None)
        }

        // Commands on the entry with the same uuid.
        "color" => update("color", json!(color(str_of(&c, "color").unwrap_or("gray")))),
        "label" => update("label", json!(str_of(&c, "label").unwrap_or(""))),
        "size" => update("size", json!(str_of(&c, "size").unwrap_or("md"))),
        "hide" => update("hidden", json!(true)),
        "remove" => update("removed", json!(true)),
        "expand" | "show_app" | "hide_app" | "confetti" | "screen_color" => Mapped::Ignore,

        other => event(&vendor_kind(other), c),
    }
}

/// `ray($x)`: scalars arrive as-is; anything else as Symfony HtmlDumper HTML plus a plain-text
/// dump in `meta[0].clipboard_data`. The text is what search and agents can use, so it leads.
fn map_log(c: &Value) -> Mapped {
    let values: Vec<Value> = c.get("values").and_then(Value::as_array).cloned().unwrap_or_default();
    let is_dump = |v: &Value| v.as_str().is_some_and(|s| s.contains("sf-dump"));
    if !values.iter().any(is_dump) {
        let value = match values.len() {
            1 => values.into_iter().next().unwrap_or(Value::Null),
            _ => Value::Array(values),
        };
        return event("core.value", json!({ "value": value }));
    }
    let html: String = values
        .iter()
        .map(|v| match v {
            Value::String(s) => s.clone(),
            other => escape_html(&other.to_string()),
        })
        .collect::<Vec<_>>()
        .join("\n");
    let clip = c
        .pointer("/meta/0/clipboard_data")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    match clip {
        Some(text) => event("core.text", json!({ "content": text, "html": html, "format": "ray-dump" })),
        None => event("core.html", json!({ "content": html })),
    }
}

/// `custom` is reused by many PHP helpers; the label tells them apart.
fn map_custom(c: &Value) -> Mapped {
    let label = str_of(c, "label").unwrap_or("");
    let content = c.get("content").cloned().unwrap_or(Value::Null);
    let shown_label = match label {
        "" | "Text" | "HTML" | "Boolean" | "Null" => None,
        l => Some(l),
    };
    match (label, &content) {
        ("Boolean" | "Null", v) => event("core.value", json!({ "value": v })),
        ("Text" | "XML", Value::String(s)) => event_with("core.text", json!({ "content": unescape_text(s) }), shown_label, None),
        ("HTML" | "Image" | "", Value::String(s)) => event_with("core.html", json!({ "content": s }), shown_label, None),
        (_, Value::String(s)) if looks_like_html(s) => event_with("core.html", json!({ "content": s }), shown_label, None),
        (_, Value::String(s)) => event_with("core.text", json!({ "content": s }), shown_label, None),
        (_, v) => event_with("core.value", json!({ "value": v }), shown_label, None),
    }
}

fn frames(v: Option<&Value>) -> Value {
    let Some(Value::Array(items)) = v else {
        return json!([]);
    };
    Value::Array(
        items
            .iter()
            .map(|f| {
                let mut o = Map::new();
                let mut put = |k: &str, v: Option<&Value>| {
                    if let Some(v) = v.filter(|v| !v.is_null()) {
                        o.insert(k.into(), v.clone());
                    }
                };
                put("file", f.get("file_name"));
                put("line", f.get("line_number"));
                put("class", f.get("class"));
                put("fn", f.get("method"));
                put("vendor", f.get("vendor_frame"));
                Value::Object(o)
            })
            .collect(),
    )
}

fn color(c: &str) -> &str {
    match c {
        "yellow" => "orange",
        "grey" => "gray",
        other => other,
    }
}

/// `x-ray.<type>` for payload types Glint has no mapping for (Laravel's `event`,
/// `mailable`, `eloquent_model`, … and anything future).
fn vendor_kind(ty: &str) -> String {
    let clean: String = ty
        .chars()
        .map(|c| c.to_ascii_lowercase())
        .map(|c| if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '.' { c } else { '_' })
        .collect();
    let kind = format!("x-ray.{}", if clean.is_empty() { "unknown" } else { &clean });
    if is_valid_kind(&kind) { kind } else { "x-ray.unknown".to_owned() }
}

fn text_of(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Table cells may be HtmlDumper output; keep their text.
fn cell(v: &Value) -> Value {
    match v {
        Value::String(s) if looks_like_html(s) => Value::String(strip_html(s)),
        other => other.clone(),
    }
}

fn looks_like_html(s: &str) -> bool {
    let t = s.trim_start();
    t.starts_with('<') && t.contains('>')
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// Undoes PHP `TextPayload`: `htmlspecialchars`, spaces as `&nbsp;`, `nl2br`.
fn unescape_text(s: &str) -> String {
    let s = s.replace("<br />\n", "\n").replace("<br>\n", "\n").replace("<br />", "\n").replace("<br>", "\n");
    unescape_entities(&s)
}

fn unescape_entities(s: &str) -> String {
    s.replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#039;", "'")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

/// Text content of an HTML fragment, without `<script>` and `<style>` bodies.
pub(super) fn strip_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find('<') {
        out.push_str(&rest[..start]);
        let tail = &rest[start..];
        let lower = tail.get(..8).unwrap_or(tail).to_ascii_lowercase();
        let skip_to = if lower.starts_with("<script") {
            find_ci(tail, "</script>").map(|i| i + "</script>".len())
        } else if lower.starts_with("<style") {
            find_ci(tail, "</style>").map(|i| i + "</style>".len())
        } else {
            tail.find('>').map(|i| i + 1)
        };
        match skip_to {
            Some(n) => rest = &tail[n..],
            None => {
                rest = "";
            }
        }
    }
    out.push_str(rest);
    unescape_entities(out.trim())
}

fn find_ci(hay: &str, needle: &str) -> Option<usize> {
    hay.to_ascii_lowercase().find(needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_html_drops_scripts_styles_and_tags() {
        assert_eq!(
            strip_html("<script>x()</script><style>.a{}</style><pre class=sf-dump>array:1 [<b>a</b> =&gt; 1]</pre>"),
            "array:1 [a => 1]"
        );
        assert_eq!(strip_html("no tags &amp; fine"), "no tags & fine");
        assert_eq!(strip_html("<b>bold</b> a <i"), "bold a");
    }

    #[test]
    fn text_payload_round_trips() {
        assert_eq!(unescape_text("a&nbsp;&nbsp;&lt;b&gt;<br />\nnext"), "a  <b>\nnext");
    }

    #[test]
    fn vendor_kinds_are_valid() {
        assert_eq!(vendor_kind("eloquent_model"), "x-ray.eloquent_model");
        assert_eq!(vendor_kind("Weird Type!"), "x-ray.weird_type_");
        assert_eq!(vendor_kind(""), "x-ray.unknown");
    }
}
