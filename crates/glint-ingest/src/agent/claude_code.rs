//! Claude Code hooks (`glint hook` posts the hook's stdin JSON here). Mapping table:
//! `docs/mappings/claude-code.md`.
//!
//! Every hook carries `session_id`, `cwd` and `hook_event_name`; tool hooks add `tool_name`,
//! `tool_input`, `tool_use_id` and (after) `tool_response` / `error`.

use glint_protocol::{IncomingEnvelope, Level, Meta, Origin, is_valid_kind};
use serde_json::{Map, Value, json};

use super::{clip_str, clip_value};

const SDK: &str = concat!("glint-hooks/", env!("CARGO_PKG_VERSION"));

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum HookError {
    #[error("hook body is not JSON: {0}")]
    InvalidJson(String),
    #[error("hook body has no `hook_event_name`")]
    NoEvent,
}

pub fn decode(body: &[u8]) -> Result<Vec<IncomingEnvelope>, HookError> {
    let h: Value = serde_json::from_slice(body).map_err(|e| HookError::InvalidJson(e.to_string()))?;
    let event = h.get("hook_event_name").and_then(Value::as_str).ok_or(HookError::NoEvent)?;
    let s = |k: &str| h.get(k).and_then(Value::as_str).filter(|v| !v.is_empty());
    let session = s("session_id").unwrap_or("unknown");
    let tool = s("tool_name").unwrap_or("");
    let call_id = s("tool_use_id").map(|u| format!("cc-{session}-{u}"));

    let base = |kind: &str, payload: Value| IncomingEnvelope {
        v: None,
        id: None,
        ts: None,
        run: Some(format!("claude-code:{session}")),
        parent: None,
        project: None, // the normaliser names it from origin.cwd (git-aware)
        origin: Origin {
            lang: Some("agent".into()),
            framework: Some("claude-code".into()),
            sdk: Some(SDK.into()),
            cwd: s("cwd").map(str::to_owned),
            ..Origin::default()
        },
        kind: kind.into(),
        payload,
        meta: Meta::default(),
    };
    let with_meta = |mut e: IncomingEnvelope, label: Option<String>, level: Option<Level>, color: Option<&str>| {
        e.meta = Meta { label, level, color: color.map(str::to_owned), ..Meta::default() };
        e
    };
    let agent = || {
        let mut m = Map::new();
        for k in ["agent_id", "agent_type"] {
            if let Some(v) = s(k) {
                m.insert(k.into(), json!(v));
            }
        }
        m
    };
    let payload_with = |mut p: Map<String, Value>| {
        p.insert("session".into(), json!(session));
        p.extend(agent());
        Value::Object(p)
    };

    let out = match event {
        "SessionStart" => vec![with_meta(
            base("agent.session", payload_with(obj(&[("status", json!("started")), ("source", opt(&h, "source")), ("model", opt(&h, "model"))]))),
            Some("session start".into()),
            Some(Level::Info),
            None,
        )],
        "SessionEnd" => vec![with_meta(
            base("agent.session", payload_with(obj(&[("status", json!("ended")), ("reason", opt(&h, "reason"))]))),
            Some("session end".into()),
            Some(Level::Info),
            None,
        )],
        "UserPromptSubmit" => vec![with_meta(
            base("agent.step", payload_with(obj(&[("status", json!("prompt")), ("input", text(&h, "prompt"))]))),
            Some("prompt".into()),
            Some(Level::Info),
            Some("blue"),
        )],
        "Stop" | "SubagentStop" => vec![with_meta(
            base(
                "agent.step",
                payload_with(obj(&[("status", json!("stopped")), ("output", text(&h, "last_assistant_message"))])),
            ),
            Some(if event == "Stop" { "turn done".into() } else { format!("{} done", s("agent_type").unwrap_or("subagent")) }),
            Some(Level::Info),
            None,
        )],
        "SubagentStart" => vec![with_meta(
            base("agent.step", payload_with(obj(&[("status", json!("started"))]))),
            Some(format!("{} started", s("agent_type").unwrap_or("subagent"))),
            None,
            None,
        )],
        "PreToolUse" => {
            let mut e = base(
                "agent.tool_call",
                payload_with(obj(&[
                    ("tool", json!(tool)),
                    ("args", h.get("tool_input").map(clip_value).unwrap_or(Value::Null)),
                    ("tool_use_id", opt(&h, "tool_use_id")),
                    ("status", json!("running")),
                ])),
            );
            e.id = call_id.clone();
            vec![with_meta(e, Some(tool.to_owned()), None, None)]
        }
        "PostToolUse" | "PostToolUseFailure" => {
            let failed = event == "PostToolUseFailure" || h.get("error").is_some_and(|e| !e.is_null());
            let mut p = obj(&[
                ("tool", json!(tool)),
                ("tool_use_id", opt(&h, "tool_use_id")),
                ("ok", json!(!failed)),
            ]);
            if let Some(r) = h.get("tool_response") {
                p.insert("result".into(), clip_value(r));
            }
            if let Some(err) = h.get("error").filter(|e| !e.is_null()) {
                p.insert("error".into(), clip_value(err));
            }
            let mut result = base("agent.tool_result", payload_with(p));
            result.parent = call_id.clone();
            let mut out = vec![with_meta(
                result,
                Some(tool.to_owned()),
                failed.then_some(Level::Error),
                None,
            )];
            // Colour the call itself so the feed reads at a glance.
            if let Some(target) = &call_id {
                out.push(base(
                    "core.update",
                    json!({ "target": target, "meta": { "color": if failed { "red" } else { "green" } } }),
                ));
            }
            out
        }
        "PermissionRequest" => vec![with_meta(
            base(
                "agent.permission",
                payload_with(obj(&[
                    ("tool", json!(tool)),
                    ("args", h.get("tool_input").map(clip_value).unwrap_or(Value::Null)),
                    ("mode", opt(&h, "permission_mode")),
                ])),
            ),
            Some(format!("{tool}: waiting for approval")),
            Some(Level::Warn),
            Some("orange"),
        )],
        "Notification" => vec![with_meta(
            base("core.log", json!({ "message": s("message").unwrap_or("notification"), "fields": { "session": session } })),
            Some("notification".into()),
            Some(Level::Info),
            None,
        )],
        "PreCompact" | "PostCompact" => vec![with_meta(
            base("core.log", json!({ "message": format!("{event} ({})", s("trigger").unwrap_or("auto")), "fields": { "session": session } })),
            Some("compact".into()),
            None,
            None,
        )],
        other => {
            // Future and less common hooks: keep everything but the noisy paths.
            let mut p = h.as_object().cloned().unwrap_or_default();
            for k in ["transcript_path", "agent_transcript_path", "scratchpad_dir", "hook_event_name", "cwd"] {
                p.remove(k);
            }
            vec![base(&vendor_kind(other), clip_value(&Value::Object(p)))]
        }
    };
    Ok(out)
}

fn obj(pairs: &[(&str, Value)]) -> Map<String, Value> {
    pairs.iter().filter(|(_, v)| !v.is_null()).map(|(k, v)| ((*k).to_owned(), v.clone())).collect()
}

fn opt(h: &Value, k: &str) -> Value {
    h.get(k).cloned().unwrap_or(Value::Null)
}

fn text(h: &Value, k: &str) -> Value {
    h.get(k).and_then(Value::as_str).map(|t| json!(clip_str(t, super::STRING_MAX))).unwrap_or(Value::Null)
}

/// `UserPromptExpansion` → `x-claude-code.user_prompt_expansion`.
fn vendor_kind(event: &str) -> String {
    let mut snake = String::new();
    for (i, c) in event.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 {
                snake.push('_');
            }
            snake.push(c.to_ascii_lowercase());
        } else if c.is_ascii_alphanumeric() {
            snake.push(c);
        } else {
            snake.push('_');
        }
    }
    let kind = format!("x-claude-code.{snake}");
    if is_valid_kind(&kind) { kind } else { "x-claude-code.unknown".into() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(v: Value) -> Vec<IncomingEnvelope> {
        decode(&serde_json::to_vec(&v).unwrap()).unwrap()
    }

    const COMMON: &str = r#"{"session_id":"S1","transcript_path":"/x.jsonl","cwd":"/Users/me/src/shop","permission_mode":"default"}"#;

    fn hook(event: &str, extra: Value) -> Value {
        let mut v: Value = serde_json::from_str(COMMON).unwrap();
        v["hook_event_name"] = json!(event);
        for (k, x) in extra.as_object().unwrap() {
            v[k] = x.clone();
        }
        v
    }

    #[test]
    fn tool_call_and_result_pair_up() {
        let call = one(hook("PreToolUse", json!({"tool_name": "Bash", "tool_input": {"command": "cargo test"}, "tool_use_id": "toolu_1"})));
        assert_eq!(call.len(), 1);
        let c = &call[0];
        assert_eq!((c.kind.as_str(), c.id.as_deref()), ("agent.tool_call", Some("cc-S1-toolu_1")));
        assert_eq!(c.payload["args"], json!({"command": "cargo test"}));
        assert_eq!(c.run.as_deref(), Some("claude-code:S1"));
        assert_eq!(c.origin.cwd.as_deref().map(|c| c.ends_with("shop")), Some(true));
        assert_eq!(c.origin.framework.as_deref(), Some("claude-code"));
        assert_eq!(c.meta.label.as_deref(), Some("Bash"));

        let res = one(hook("PostToolUse", json!({"tool_name": "Bash", "tool_input": {"command": "cargo test"}, "tool_use_id": "toolu_1", "tool_response": {"stdout": "ok", "exit_code": 0}})));
        assert_eq!(res[0].kind, "agent.tool_result");
        assert_eq!(res[0].parent.as_deref(), Some("cc-S1-toolu_1"));
        assert_eq!(res[0].payload["ok"], true);
        assert_eq!(res[0].payload["result"]["stdout"], "ok");
        assert_eq!(res[1].kind, "core.update");
        assert_eq!(res[1].payload, json!({"target": "cc-S1-toolu_1", "meta": {"color": "green"}}));
    }

    #[test]
    fn failures_are_errors_and_red() {
        let res = one(hook("PostToolUseFailure", json!({"tool_name": "Edit", "tool_use_id": "t2", "error": "old_string not found"})));
        assert_eq!(res[0].meta.level, Some(Level::Error));
        assert_eq!(res[0].payload["error"], "old_string not found");
        assert_eq!(res[1].payload["meta"]["color"], "red");
    }

    #[test]
    fn long_tool_output_is_clipped() {
        let big = "x".repeat(50_000);
        let res = one(hook("PostToolUse", json!({"tool_name": "Read", "tool_use_id": "t3", "tool_response": {"content": big}})));
        let s = res[0].payload["result"]["content"].as_str().unwrap();
        assert!(s.len() < 4100 && s.ends_with("more bytes]"));
    }

    #[test]
    fn session_prompt_stop_and_subagents() {
        let kinds = |v: Vec<IncomingEnvelope>| v.into_iter().map(|e| (e.kind, e.meta.label.unwrap_or_default())).collect::<Vec<_>>();
        assert_eq!(kinds(one(hook("SessionStart", json!({"source": "startup"})))), [("agent.session".into(), "session start".into())]);
        let p = one(hook("UserPromptSubmit", json!({"prompt": "fix the cart total"})));
        assert_eq!(p[0].payload["input"], "fix the cart total");
        let s = one(hook("SubagentStop", json!({"agent_id": "a1", "agent_type": "Explore", "last_assistant_message": "found it"})));
        assert_eq!(s[0].meta.label.as_deref(), Some("Explore done"));
        assert_eq!(s[0].payload["agent_type"], "Explore");
        assert_eq!(s[0].payload["output"], "found it");
    }

    #[test]
    fn real_fixture_shapes_decode() {
        // Shapes captured from Claude Code 2.1.281 (paths and content anonymised).
        let perm = r#"{"session_id":"SESSION","transcript_path":"t","cwd":"C:\\Users\\u\\src\\payments-service","scratchpad_dir":"s",
            "hook_event_name":"PermissionRequest","prompt_id":"p","permission_mode":"default","effort":{"level":"xhigh"},"tool_name":"Edit",
            "tool_input":{"file_path":"C:\\x.txt","old_string":"a","new_string":"b","replace_all":false},"permission_suggestions":[]}"#;
        let e = decode(perm.as_bytes()).unwrap();
        assert_eq!(e[0].kind, "agent.permission");
        assert!(e[0].origin.cwd.as_deref().is_some_and(|c| c.ends_with("payments-service")));
        assert_eq!(e[0].meta.level, Some(Level::Warn));

        let exp = r#"{"session_id":"S","cwd":"/a","hook_event_name":"UserPromptExpansion","prompt_id":"p","expansion":"x"}"#;
        let e = decode(exp.as_bytes()).unwrap();
        assert_eq!(e[0].kind, "x-claude-code.user_prompt_expansion");
        assert_eq!(e[0].payload["expansion"], "x");
        assert!(e[0].payload.get("cwd").is_none());
    }

    #[test]
    fn bad_bodies() {
        assert!(matches!(decode(b"nope"), Err(HookError::InvalidJson(_))));
        assert_eq!(decode(b"{}"), Err(HookError::NoEvent));
    }
}
