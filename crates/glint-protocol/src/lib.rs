//! Glint wire protocol v1.
//!
//! The normative description lives in `docs/PROTOCOL.md`; the machine-readable one in
//! `spec/envelope.schema.json`. This crate holds the Rust shape of both and the rules for
//! turning request bodies into envelopes. It knows nothing about transports, storage or
//! any particular language.

mod kind;
mod parse;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub use kind::{default_level, is_valid_kind};
pub use parse::{BodyError, BodyFormat, EnvelopeError, parse_body};

/// Current protocol major version.
pub const PROTOCOL_VERSION: u32 = 1;
/// Largest single envelope accepted, in bytes of its JSON encoding.
pub const MAX_ENVELOPE_BYTES: usize = 1024 * 1024;
/// Largest request body accepted, in bytes.
pub const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

/// An envelope as a client sent it: only `kind` and `payload` are required.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct IncomingEnvelope {
    #[serde(default)]
    pub v: Option<u32>,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub ts: Option<DateTime<Utc>>,
    #[serde(default)]
    pub run: Option<String>,
    #[serde(default)]
    pub parent: Option<String>,
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub origin: Origin,
    pub kind: String,
    pub payload: Value,
    #[serde(default)]
    pub meta: Meta,
}

/// A complete envelope, after the server filled in every default. This is what the store
/// keeps, the stream sends and the UI and MCP read.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct Envelope {
    pub v: u32,
    pub id: String,
    pub ts: DateTime<Utc>,
    pub run: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    pub project: String,
    #[serde(default)]
    pub origin: Origin,
    pub kind: String,
    pub payload: Value,
    #[serde(default)]
    pub meta: Meta,
}

impl Envelope {
    /// The level this event is shown and filtered at.
    pub fn level(&self) -> Level {
        self.meta.level.unwrap_or_else(|| default_level(&self.kind))
    }
}

/// Where an event came from. Every field is optional; unknown keys are kept.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
pub struct Origin {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lang: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub framework: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sdk: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub col: Option<u32>,
    #[serde(rename = "fn", default, skip_serializing_if = "Option::is_none")]
    pub function: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Display and filtering hints. Unknown keys are kept and ignored.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
pub struct Meta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<Level>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screen: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hidden: Option<bool>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Severity, ordered from least to most severe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
    Fatal,
}

impl Level {
    pub const ALL: [Level; 6] = [
        Level::Trace,
        Level::Debug,
        Level::Info,
        Level::Warn,
        Level::Error,
        Level::Fatal,
    ];

    /// Numeric rank for storage and `>=` filtering.
    pub fn rank(self) -> u8 {
        self as u8
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Level::Trace => "trace",
            Level::Debug => "debug",
            Level::Info => "info",
            Level::Warn => "warn",
            Level::Error => "error",
            Level::Fatal => "fatal",
        }
    }

    pub fn parse(s: &str) -> Option<Level> {
        Level::ALL.into_iter().find(|l| l.as_str() == s)
    }
}
