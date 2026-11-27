//! The native protocol (`docs/PROTOCOL.md` §2): a JSON envelope, a JSON array of them, or
//! NDJSON.

use glint_protocol::{BodyError, BodyFormat, IncomingEnvelope, parse_body};
use serde::Serialize;

/// The usable envelopes of a body, plus why the others were rejected.
#[derive(Debug, Default)]
pub struct Decoded {
    pub envelopes: Vec<IncomingEnvelope>,
    pub rejected: Vec<Rejected>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Rejected {
    /// Position in the body (array index or line among non-blank lines).
    pub index: usize,
    pub error: String,
}

pub fn decode(body: &[u8], content_type: Option<&str>) -> Result<Decoded, BodyError> {
    let mut out = Decoded::default();
    for (index, item) in parse_body(body, BodyFormat::from_content_type(content_type))?
        .into_iter()
        .enumerate()
    {
        match item {
            Ok(e) => out.envelopes.push(e),
            Err(err) => out.rejected.push(Rejected { index, error: err.to_string() }),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_good_from_bad() {
        let d = decode(br#"[{"kind":"core.value","payload":1},{"kind":"bad","payload":1}]"#, None).unwrap();
        assert_eq!(d.envelopes.len(), 1);
        assert_eq!(d.rejected, vec![Rejected { index: 1, error: "invalid kind `bad`".into() }]);
    }
}
