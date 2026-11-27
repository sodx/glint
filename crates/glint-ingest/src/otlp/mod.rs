//! OpenTelemetry ingest (OTLP/HTTP, protobuf or JSON). Mapping: `docs/mappings/otlp.md`.
//!
//! The point is zero-SDK coverage: any language with an OpenTelemetry SDK shows up, and any LLM
//! library instrumented with the GenAI semantic conventions (OpenLLMetry, OpenInference-style
//! exporters, vendor instrumentors) produces `llm.call` events without Glint-specific code.

mod decode;
mod map;
pub mod proto;

pub use decode::{Encoding, LogRec, OtlpError, Span, SpanEvent};

use glint_protocol::IncomingEnvelope;

/// `POST /v1/traces`.
pub fn decode_traces(body: &[u8], content_type: Option<&str>) -> Result<Vec<IncomingEnvelope>, OtlpError> {
    let enc = Encoding::from_content_type(content_type)?;
    Ok(decode::traces(body, enc)?.iter().flat_map(map::span).collect())
}

/// `POST /v1/logs`.
pub fn decode_logs(body: &[u8], content_type: Option<&str>) -> Result<Vec<IncomingEnvelope>, OtlpError> {
    let enc = Encoding::from_content_type(content_type)?;
    Ok(decode::logs(body, enc)?.iter().map(map::log).collect())
}

#[cfg(test)]
mod tests;
