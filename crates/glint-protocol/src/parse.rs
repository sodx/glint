use serde_json::Value;

use crate::{IncomingEnvelope, MAX_BODY_BYTES, MAX_ENVELOPE_BYTES, PROTOCOL_VERSION, is_valid_kind};

/// How a request body is framed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyFormat {
    /// One envelope object, or an array of them.
    Json,
    /// One envelope per line.
    Ndjson,
}

impl BodyFormat {
    /// Picks the format from a `Content-Type` header value; anything else is JSON.
    pub fn from_content_type(ct: Option<&str>) -> BodyFormat {
        match ct {
            Some(ct) if ct.trim_start().starts_with("application/x-ndjson") => BodyFormat::Ndjson,
            _ => BodyFormat::Json,
        }
    }
}

/// The body as a whole could not be read. Nothing in it is accepted.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum BodyError {
    #[error("body is {0} bytes; the limit is {MAX_BODY_BYTES}")]
    TooLarge(usize),
    #[error("body is not valid JSON: {0}")]
    InvalidJson(String),
    #[error("body must be an envelope object or an array of envelopes")]
    NotEnvelopes,
}

/// One envelope in an otherwise readable body was rejected. The rest are unaffected.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum EnvelopeError {
    #[error("envelope is {0} bytes; the limit is {MAX_ENVELOPE_BYTES}")]
    TooLarge(usize),
    #[error("not valid JSON: {0}")]
    InvalidJson(String),
    #[error("envelope must be a JSON object")]
    NotAnObject,
    #[error("missing required field `{0}`")]
    Missing(&'static str),
    #[error("invalid kind `{0}`")]
    InvalidKind(String),
    #[error("unsupported protocol version {0}; this server speaks v{PROTOCOL_VERSION}")]
    UnsupportedVersion(u64),
    #[error("{0}")]
    Shape(String),
}

/// Splits a request body into envelopes. A body-level error rejects everything; otherwise
/// each envelope succeeds or fails on its own, in body order.
pub fn parse_body(
    body: &[u8],
    format: BodyFormat,
) -> Result<Vec<Result<IncomingEnvelope, EnvelopeError>>, BodyError> {
    if body.len() > MAX_BODY_BYTES {
        return Err(BodyError::TooLarge(body.len()));
    }
    match format {
        BodyFormat::Ndjson => Ok(body
            .split(|b| *b == b'\n')
            .filter(|line| !line.iter().all(u8::is_ascii_whitespace))
            .map(|line| {
                if line.len() > MAX_ENVELOPE_BYTES {
                    return Err(EnvelopeError::TooLarge(line.len()));
                }
                let value: Value = serde_json::from_slice(line)
                    .map_err(|e| EnvelopeError::InvalidJson(e.to_string()))?;
                parse_value(value, line.len())
            })
            .collect()),
        BodyFormat::Json => {
            let value: Value =
                serde_json::from_slice(body).map_err(|e| BodyError::InvalidJson(e.to_string()))?;
            match value {
                Value::Array(items) => Ok(items
                    .into_iter()
                    .map(|item| {
                        let size = encoded_len(&item);
                        parse_value(item, size)
                    })
                    .collect()),
                obj @ Value::Object(_) => Ok(vec![parse_value(obj, body.len())]),
                _ => Err(BodyError::NotEnvelopes),
            }
        }
    }
}

fn encoded_len(v: &Value) -> usize {
    serde_json::to_vec(v).map(|b| b.len()).unwrap_or(usize::MAX)
}

fn parse_value(value: Value, size: usize) -> Result<IncomingEnvelope, EnvelopeError> {
    if size > MAX_ENVELOPE_BYTES {
        return Err(EnvelopeError::TooLarge(size));
    }
    let Value::Object(obj) = &value else {
        return Err(EnvelopeError::NotAnObject);
    };
    let kind = match obj.get("kind") {
        None => return Err(EnvelopeError::Missing("kind")),
        Some(Value::String(k)) => k.clone(),
        Some(_) => return Err(EnvelopeError::InvalidKind("<not a string>".into())),
    };
    if !is_valid_kind(&kind) {
        return Err(EnvelopeError::InvalidKind(kind));
    }
    if !obj.contains_key("payload") {
        return Err(EnvelopeError::Missing("payload"));
    }
    match obj.get("v") {
        None | Some(Value::Null) => {}
        Some(Value::Number(n)) if n.as_u64() == Some(u64::from(PROTOCOL_VERSION)) => {}
        Some(Value::Number(n)) => {
            return Err(EnvelopeError::UnsupportedVersion(n.as_u64().unwrap_or(0)));
        }
        Some(_) => return Err(EnvelopeError::Shape("`v` must be an integer".into())),
    }
    serde_json::from_value(value).map_err(|e| EnvelopeError::Shape(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Level;

    fn ok(body: &str, f: BodyFormat) -> Vec<Result<IncomingEnvelope, EnvelopeError>> {
        parse_body(body.as_bytes(), f).expect("body parses")
    }

    #[test]
    fn minimal_object_needs_only_kind_and_payload() {
        let r = ok(r#"{"kind":"core.value","payload":42}"#, BodyFormat::Json);
        let e = r[0].as_ref().unwrap();
        assert_eq!(e.kind, "core.value");
        assert_eq!(e.payload, 42);
        assert!(e.id.is_none() && e.run.is_none());
    }

    #[test]
    fn array_rejects_bad_items_individually() {
        let r = ok(
            r#"[{"kind":"core.log","payload":{"message":"a"}},{"payload":1},{"kind":"nope","payload":1},{"kind":"core.value"}]"#,
            BodyFormat::Json,
        );
        assert!(r[0].is_ok());
        assert_eq!(r[1], Err(EnvelopeError::Missing("kind")));
        assert_eq!(r[2], Err(EnvelopeError::InvalidKind("nope".into())));
        assert_eq!(r[3], Err(EnvelopeError::Missing("payload")));
    }

    #[test]
    fn null_payload_is_present() {
        assert!(ok(r#"{"kind":"core.value","payload":null}"#, BodyFormat::Json)[0].is_ok());
    }

    #[test]
    fn ndjson_skips_blank_lines_and_isolates_bad_lines() {
        let r = ok(
            "{\"kind\":\"core.value\",\"payload\":1}\n\n not json\n{\"kind\":\"core.value\",\"payload\":2}\n",
            BodyFormat::Ndjson,
        );
        assert_eq!(r.len(), 3);
        assert!(r[0].is_ok() && r[2].is_ok());
        assert!(matches!(r[1], Err(EnvelopeError::InvalidJson(_))));
    }

    #[test]
    fn unknown_meta_and_origin_keys_survive_round_trip() {
        let r = ok(
            r#"{"kind":"x-acme.thing","payload":{},"meta":{"level":"warn","glow":true},"origin":{"fn":"main","line":3,"weird":"yes"}}"#,
            BodyFormat::Json,
        );
        let e = r[0].as_ref().unwrap();
        assert_eq!(e.meta.level, Some(Level::Warn));
        assert_eq!(e.meta.extra["glow"], true);
        assert_eq!(e.origin.function.as_deref(), Some("main"));
        let back = serde_json::to_value(e).unwrap();
        assert_eq!(back["origin"]["weird"], "yes");
        assert_eq!(back["origin"]["fn"], "main");
    }

    #[test]
    fn wrong_version_and_bad_level_are_rejected() {
        let r = ok(
            r#"[{"v":2,"kind":"core.value","payload":1},{"kind":"core.value","payload":1,"meta":{"level":"loud"}}]"#,
            BodyFormat::Json,
        );
        assert_eq!(r[0], Err(EnvelopeError::UnsupportedVersion(2)));
        assert!(matches!(r[1], Err(EnvelopeError::Shape(_))));
    }

    #[test]
    fn body_level_errors() {
        assert!(matches!(parse_body(b"nope", BodyFormat::Json), Err(BodyError::InvalidJson(_))));
        assert_eq!(parse_body(b"42", BodyFormat::Json), Err(BodyError::NotEnvelopes));
    }

    #[test]
    fn content_type_selects_format() {
        assert_eq!(BodyFormat::from_content_type(Some("application/x-ndjson")), BodyFormat::Ndjson);
        assert_eq!(BodyFormat::from_content_type(Some("application/json")), BodyFormat::Json);
        assert_eq!(BodyFormat::from_content_type(None), BodyFormat::Json);
    }
}
