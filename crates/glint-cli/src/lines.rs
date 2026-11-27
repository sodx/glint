//! Turning lines of program output into envelopes. Pure functions, no I/O.

use glint_protocol::Level;
use serde_json::{Map, Value, json};

/// What one line of input says.
#[derive(Debug, Clone, PartialEq)]
pub enum Line {
    /// A complete Glint envelope; forwarded as is.
    Envelope(Value),
    /// A log record: plain text or a structured JSON log line.
    Log {
        message: String,
        level: Option<Level>,
        fields: Option<Map<String, Value>>,
    },
}

pub fn classify(raw: &str) -> Line {
    let clean = strip_ansi(raw);
    let trimmed = clean.trim();
    if trimmed.starts_with('{')
        && let Ok(Value::Object(obj)) = serde_json::from_str::<Value>(trimmed)
    {
        if obj.contains_key("kind") && obj.contains_key("payload") {
            return Line::Envelope(Value::Object(obj));
        }
        return structured(obj);
    }
    Line::Log {
        level: sniff_level(&clean).or_else(|| looks_like_exception(&clean).then_some(Level::Error)),
        message: clean.trim_end().to_owned(),
        fields: None,
    }
}

/// `TypeError: …`, `java.lang.IllegalStateException: …`, `RuntimeException: …`, a Python
/// traceback, a Rust panic.
fn looks_like_exception(text: &str) -> bool {
    let first = text.lines().next().unwrap_or("").trim_start();
    if first.starts_with("Traceback (most recent call last)") || first.contains("panicked at ") {
        return true;
    }
    let Some((head, _)) = first.split_once(':') else {
        return false;
    };
    let name = head.rsplit(['.', '\\', ' ']).next().unwrap_or(head);
    !head.contains(' ')
        && name.len() > 5
        && name.starts_with(|c: char| c.is_ascii_uppercase())
        && (name.ends_with("Error") || name.ends_with("Exception"))
}

/// A JSON log line from pino, bunyan, zap, slog, structlog, logrus, monolog-json, …
fn structured(mut obj: Map<String, Value>) -> Line {
    let message = ["msg", "message", "text", "event"]
        .iter()
        .find_map(|k| match obj.get(*k) {
            Some(Value::String(s)) => {
                let s = s.clone();
                obj.remove(*k);
                Some(s)
            }
            _ => None,
        });
    let level = ["level", "severity", "lvl", "level_name"]
        .iter()
        .find_map(|k| obj.get(*k).and_then(level_of).map(|l| (*k, l)))
        .map(|(k, l)| {
            obj.remove(k);
            l
        });
    match message {
        Some(message) => Line::Log { message, level, fields: Some(obj) },
        None => Line::Log {
            message: Value::Object(obj.clone()).to_string(),
            level,
            fields: Some(obj),
        },
    }
}

fn level_of(v: &Value) -> Option<Level> {
    match v {
        Value::String(s) => word_level(&s.to_ascii_lowercase()),
        // pino / bunyan numeric levels.
        Value::Number(n) => match n.as_u64()? {
            0..=10 => Some(Level::Trace),
            11..=20 => Some(Level::Debug),
            21..=30 => Some(Level::Info),
            31..=40 => Some(Level::Warn),
            41..=50 => Some(Level::Error),
            _ => Some(Level::Fatal),
        },
        _ => None,
    }
}

fn word_level(w: &str) -> Option<Level> {
    Some(match w {
        "trace" | "verbose" => Level::Trace,
        "debug" | "dbg" => Level::Debug,
        "info" | "notice" | "information" => Level::Info,
        "warn" | "warning" => Level::Warn,
        "error" | "err" | "exception" => Level::Error,
        "fatal" | "critical" | "crit" | "panic" | "emergency" | "alert" => Level::Fatal,
        _ => return None,
    })
}

/// Looks for a level word among the first dozen words of a plain line: `ERROR`, `[warn]`,
/// `level=info`, `production.ERROR:` (Laravel). Only whole words count, so "errors" or
/// "information-rich" do not.
pub fn sniff_level(line: &str) -> Option<Level> {
    let head: String = line.chars().take(120).collect::<String>().to_ascii_lowercase();
    head.split(|c: char| !c.is_ascii_alphabetic())
        .filter(|w| !w.is_empty())
        .take(12)
        .find_map(word_level)
}

/// Removes ANSI escape sequences (colors, cursor moves) that dev servers print.
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match chars.peek() {
            // CSI: ESC [ params final-byte(0x40..=0x7e)
            Some('[') => {
                chars.next();
                for c in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&c) {
                        break;
                    }
                }
            }
            // OSC: ESC ] ... BEL or ESC \
            Some(']') => {
                chars.next();
                while let Some(c) = chars.next() {
                    if c == '\u{7}' {
                        break;
                    }
                    if c == '\u{1b}' && chars.peek() == Some(&'\\') {
                        chars.next();
                        break;
                    }
                }
            }
            _ => {
                chars.next();
            }
        }
    }
    out
}

/// Whether `line` continues the previous record: indented lines and stack-trace markers.
pub fn is_continuation(raw: &str) -> bool {
    let line = strip_ansi(raw);
    if line.trim().is_empty() {
        return false;
    }
    let t = line.trim_start();
    line.starts_with([' ', '\t'])
        || t.starts_with("at ")
        || t.starts_with("Caused by")
        || t.starts_with("...")
        || t.starts_with('#') && t[1..].starts_with(|c: char| c.is_ascii_digit()) // PHP traces: #0 /path
        || t.starts_with("Stack trace:")
}

/// The envelope for a log line.
pub fn log_envelope(line: Line, base: &Base) -> Value {
    match line {
        Line::Envelope(mut v) => {
            if v.get("run").is_none() {
                v["run"] = json!(base.run);
            }
            v
        }
        Line::Log { message, level, fields } => {
            let mut payload = json!({ "message": message });
            if let Some(f) = fields.filter(|f| !f.is_empty()) {
                payload["fields"] = Value::Object(f);
            }
            let mut env = base.envelope("core.log", payload);
            if let Some(l) = level {
                env["meta"]["level"] = json!(l.as_str());
            }
            env
        }
    }
}

/// Fields every envelope from one CLI invocation shares.
#[derive(Debug, Clone)]
pub struct Base {
    pub run: String,
    pub project: Option<String>,
    pub label: Option<String>,
    pub color: Option<String>,
    pub tags: Vec<String>,
    pub origin: Value,
}

impl Base {
    pub fn envelope(&self, kind: &str, payload: Value) -> Value {
        let mut meta = Map::new();
        if let Some(l) = &self.label {
            meta.insert("label".into(), json!(l));
        }
        if let Some(c) = &self.color {
            meta.insert("color".into(), json!(c));
        }
        if !self.tags.is_empty() {
            meta.insert("tags".into(), json!(self.tags));
        }
        let mut env = json!({
            "run": self.run,
            "kind": kind,
            "payload": payload,
            "origin": self.origin,
            "meta": meta,
        });
        if let Some(p) = &self.project {
            env["project"] = json!(p);
        }
        env
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn log(message: &str, level: Option<Level>) -> Line {
        Line::Log { message: message.into(), level, fields: None }
    }

    #[test]
    fn plain_lines_get_sniffed_levels() {
        assert_eq!(classify("ERROR: db down"), log("ERROR: db down", Some(Level::Error)));
        assert_eq!(classify("[2026-11-27 10:00:00] production.WARNING: slow"), log("[2026-11-27 10:00:00] production.WARNING: slow", Some(Level::Warn)));
        assert_eq!(classify("time=1 level=info msg=ok"), log("time=1 level=info msg=ok", Some(Level::Info)));
        assert_eq!(classify("Compiled successfully in 12ms"), log("Compiled successfully in 12ms", None));
    }

    #[test]
    fn exceptions_are_errors() {
        for line in [
            "TypeError: Cannot read properties of undefined",
            "java.lang.IllegalStateException: bad",
            "App\\Exceptions\\PaymentException: declined",
            "Traceback (most recent call last):",
            "thread 'main' panicked at src/main.rs:3:5:",
        ] {
            assert!(matches!(classify(line), Line::Log { level: Some(Level::Error), .. }), "{line}");
        }
        for line in ["Note: restart needed", "Error handling improved: yes ok", "Errors: 0"] {
            assert!(!looks_like_exception(line), "{line}");
        }
    }

    #[test]
    fn ansi_is_stripped_from_the_message() {
        assert_eq!(strip_ansi("\u{1b}[31mred\u{1b}[0m \u{1b}]8;;http://x\u{7}link\u{1b}]8;;\u{7}"), "red link");
        assert_eq!(classify("\u{1b}[33mwarn\u{1b}[39m  something"), log("warn  something", Some(Level::Warn)));
    }

    #[test]
    fn json_logs_become_structured() {
        let Line::Log { message, level, fields } = classify(r#"{"level":50,"time":1,"msg":"boom","reqId":"a1"}"#) else {
            panic!()
        };
        assert_eq!((message.as_str(), level), ("boom", Some(Level::Error)));
        let f = fields.unwrap();
        assert_eq!(f["reqId"], "a1");
        assert!(!f.contains_key("msg") && !f.contains_key("level"));

        let Line::Log { message, level, .. } = classify(r#"{"severity":"WARNING","other":1}"#) else { panic!() };
        assert_eq!(level, Some(Level::Warn));
        assert!(message.contains("other"));
    }

    #[test]
    fn envelopes_pass_through() {
        assert!(matches!(classify(r#"{"kind":"core.value","payload":1}"#), Line::Envelope(_)));
    }

    #[test]
    fn continuation_lines() {
        assert!(is_continuation("    at Object.<anonymous> (/app/x.js:1:1)"));
        assert!(is_continuation("\tat com.acme.Main.run(Main.java:10)"));
        assert!(is_continuation("#0 /srv/app/index.php(12): foo()"));
        assert!(is_continuation("Caused by: java.io.IOException"));
        assert!(!is_continuation("Server started"));
        assert!(!is_continuation(""));
        assert!(!is_continuation("#hashtag"));
    }

    #[test]
    fn envelope_carries_base_fields() {
        let base = Base {
            run: "r".into(),
            project: Some("p".into()),
            label: Some("web".into()),
            color: None,
            tags: vec!["dev".into()],
            origin: json!({"sdk": "glint-cli/0"}),
        };
        let e = log_envelope(log("x", Some(Level::Warn)), &base);
        assert_eq!(e["run"], "r");
        assert_eq!(e["project"], "p");
        assert_eq!(e["meta"], json!({"label": "web", "tags": ["dev"], "level": "warn"}));
        assert_eq!(e["payload"], json!({"message": "x"}));
    }
}
