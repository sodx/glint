//! `glint send`: one event per value.

use std::io::Read;

use glint_protocol::{BodyFormat, Level, is_valid_kind, parse_body};
use serde_json::{Value, json};

use crate::client::Client;
use crate::lines::Base;

pub struct SendArgs {
    pub values: Vec<String>,
    pub kind: String,
    pub level: Option<Level>,
    pub raw: bool,
    pub envelope: bool,
    pub quiet: bool,
}

/// Builds the envelopes for `glint send`. `stdin` is read only when no value is given or a
/// value is `-`.
pub fn build(args: &SendArgs, base: &Base, stdin: &mut dyn Read) -> Result<Vec<Value>, String> {
    if !is_valid_kind(&args.kind) {
        return Err(format!("invalid kind `{}` (expected e.g. core.value, sql.query, x-acme.thing)", args.kind));
    }
    let mut inputs = Vec::new();
    if args.values.is_empty() {
        inputs.push(read_all(stdin)?);
    } else {
        for v in &args.values {
            inputs.push(if v == "-" { read_all(stdin)? } else { v.clone() });
        }
    }

    if args.envelope {
        let mut out = Vec::new();
        for input in inputs {
            out.extend(envelopes(&input, base)?);
        }
        return Ok(out);
    }

    inputs
        .into_iter()
        .map(|input| {
            let value = if args.raw {
                Value::String(input)
            } else {
                serde_json::from_str(input.trim()).unwrap_or(Value::String(input))
            };
            let payload = payload_for(&args.kind, value)?;
            let mut env = base.envelope(&args.kind, payload);
            if let Some(l) = args.level {
                env["meta"]["level"] = json!(l.as_str());
            }
            Ok(env)
        })
        .collect()
}

/// The payload shape each common kind expects (`docs/PROTOCOL.md` §4).
fn payload_for(kind: &str, value: Value) -> Result<Value, String> {
    let as_text = |v: Value| match v {
        Value::String(s) => s,
        other => other.to_string(),
    };
    Ok(match kind {
        "core.value" => json!({ "value": value }),
        "core.log" => json!({ "message": as_text(value) }),
        "core.text" | "core.html" | "core.markdown" | "core.mermaid" => json!({ "content": as_text(value) }),
        "core.json" => json!({ "content": value }),
        _ => match value {
            Value::String(s) => {
                return Err(format!(
                    "kind `{kind}` needs a JSON payload, got text: {}",
                    s.chars().take(60).collect::<String>()
                ));
            }
            v => v,
        },
    })
}

/// Full envelopes from JSON (object or array) or NDJSON; the CLI's run and origin fill gaps.
fn envelopes(input: &str, base: &Base) -> Result<Vec<Value>, String> {
    let format = if serde_json::from_str::<Value>(input.trim()).is_ok() { BodyFormat::Json } else { BodyFormat::Ndjson };
    let parsed = parse_body(input.as_bytes(), format).map_err(|e| e.to_string())?;
    parsed
        .into_iter()
        .enumerate()
        .map(|(i, r)| {
            let e = r.map_err(|e| format!("envelope {i}: {e}"))?;
            let mut v = serde_json::to_value(e).map_err(|e| e.to_string())?;
            if v["run"].is_null() {
                v["run"] = json!(base.run);
            }
            if v["origin"].as_object().is_none_or(|o| o.is_empty()) {
                v["origin"] = base.origin.clone();
            }
            Ok(v)
        })
        .collect()
}

fn read_all(stdin: &mut dyn Read) -> Result<String, String> {
    let mut s = String::new();
    stdin.read_to_string(&mut s).map_err(|e| format!("reading stdin: {e}"))?;
    Ok(s.strip_suffix('\n').unwrap_or(&s).to_owned())
}

pub fn run(args: &SendArgs, base: &Base, client: &Client) -> Result<(), String> {
    let envs = build(args, base, &mut std::io::stdin())?;
    let ack = client.post(&envs).map_err(|e| match e {
        crate::client::ClientError::Unreachable(m) => {
            format!("cannot reach Glint at {} ({m}). Is glintd running?", client.base())
        }
        other => other.to_string(),
    })?;
    for (i, err) in &ack.rejected {
        eprintln!("glint: event {i} rejected: {err}");
    }
    if !args.quiet {
        for id in &ack.ids {
            println!("{id}");
        }
    }
    if ack.ids.is_empty() && !ack.rejected.is_empty() {
        return Err("nothing was accepted".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Base {
        Base { run: "r".into(), project: None, label: None, color: None, tags: vec![], origin: json!({"sdk": "glint-cli/test"}) }
    }

    fn args(values: &[&str], kind: &str) -> SendArgs {
        SendArgs {
            values: values.iter().map(|s| s.to_string()).collect(),
            kind: kind.into(),
            level: None,
            raw: false,
            envelope: false,
            quiet: false,
        }
    }

    #[test]
    fn values_are_json_when_they_parse() {
        let e = build(&args(&["42", "{\"a\":1}", "hello", "true"], "core.value"), &base(), &mut std::io::empty()).unwrap();
        let vals: Vec<_> = e.iter().map(|e| e["payload"]["value"].clone()).collect();
        assert_eq!(vals, [json!(42), json!({"a": 1}), json!("hello"), json!(true)]);
    }

    #[test]
    fn raw_keeps_text() {
        let mut a = args(&["42"], "core.value");
        a.raw = true;
        assert_eq!(build(&a, &base(), &mut std::io::empty()).unwrap()[0]["payload"]["value"], "42");
    }

    #[test]
    fn stdin_when_no_values_or_dash() {
        let e = build(&args(&[], "core.log"), &base(), &mut "line one\n".as_bytes()).unwrap();
        assert_eq!(e[0]["payload"], json!({"message": "line one"}));
        let e = build(&args(&["a", "-"], "core.text"), &base(), &mut "from stdin".as_bytes()).unwrap();
        assert_eq!(e[1]["payload"], json!({"content": "from stdin"}));
    }

    #[test]
    fn other_kinds_take_the_json_as_payload() {
        let e = build(&args(&[r#"{"sql":"select 1"}"#], "sql.query"), &base(), &mut std::io::empty()).unwrap();
        assert_eq!(e[0]["payload"], json!({"sql": "select 1"}));
        assert!(build(&args(&["not json"], "sql.query"), &base(), &mut std::io::empty()).is_err());
        assert!(build(&args(&["1"], "bogus"), &base(), &mut std::io::empty()).is_err());
    }

    #[test]
    fn envelope_mode_accepts_json_and_ndjson() {
        let mut a = args(&[], "core.value");
        a.envelope = true;
        let e = build(&a, &base(), &mut r#"[{"kind":"core.value","payload":1},{"kind":"core.log","payload":{"message":"m"},"run":"mine"}]"#.as_bytes()).unwrap();
        assert_eq!((e[0]["run"].as_str(), e[1]["run"].as_str()), (Some("r"), Some("mine")));
        let e = build(&a, &base(), &mut "{\"kind\":\"core.value\",\"payload\":1}\n{\"kind\":\"core.value\",\"payload\":2}\n".as_bytes()).unwrap();
        assert_eq!(e.len(), 2);
        assert!(build(&a, &base(), &mut r#"{"payload":1}"#.as_bytes()).is_err());
    }
}
