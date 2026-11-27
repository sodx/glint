//! Coding-agent hooks → `agent.*` events, so Glint shows what the agent did next to what the
//! program did. One module per agent; Claude Code first.

pub mod claude_code;

use serde_json::{Map, Value};

/// Longest string kept from tool inputs and results; agents move whole files through tools.
pub const STRING_MAX: usize = 4000;
const ITEMS_MAX: usize = 100;
const DEPTH_MAX: usize = 8;

/// Shrinks a JSON value for storage: long strings cut, long arrays and objects capped, deep
/// nesting replaced by the protocol's `$truncated` marker.
pub fn clip_value(v: &Value) -> Value {
    clip(v, 0)
}

fn clip(v: &Value, depth: usize) -> Value {
    if depth >= DEPTH_MAX {
        return serde_json::json!({ "$truncated": true });
    }
    match v {
        Value::String(s) => Value::String(clip_str(s, STRING_MAX)),
        Value::Array(a) => {
            let mut out: Vec<Value> = a.iter().take(ITEMS_MAX).map(|x| clip(x, depth + 1)).collect();
            if a.len() > ITEMS_MAX {
                out.push(serde_json::json!({ "$truncated": true, "more": a.len() - ITEMS_MAX }));
            }
            Value::Array(out)
        }
        Value::Object(m) => {
            let mut out = Map::new();
            for (k, x) in m.iter().take(ITEMS_MAX) {
                out.insert(k.clone(), clip(x, depth + 1));
            }
            if m.len() > ITEMS_MAX {
                out.insert("$truncated".into(), Value::Bool(true));
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

pub fn clip_str(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_owned();
    }
    let mut cut = max;
    while !s.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}… [{} more bytes]", &s[..cut], s.len() - cut)
}
