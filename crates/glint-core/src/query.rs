use glint_protocol::{Envelope, Level};
use serde::Deserialize;

/// A filter over events, shared by the read API, the live stream and (later) MCP.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Query {
    pub project: Option<String>,
    pub run: Option<String>,
    /// Exact kind, or a prefix ending in `*` (`sql.*`).
    pub kind: Option<String>,
    /// Minimum level, inclusive.
    pub level: Option<Level>,
    /// Full-text search over string values.
    pub q: Option<String>,
    /// Only events stored after the event with this id.
    pub after: Option<String>,
    /// Only events stored before the event with this id.
    pub before: Option<String>,
    pub limit: Option<usize>,
}

impl Query {
    pub const DEFAULT_LIMIT: usize = 200;
    pub const MAX_LIMIT: usize = 5000;

    pub fn limit(&self) -> usize {
        self.limit.unwrap_or(Self::DEFAULT_LIMIT).clamp(1, Self::MAX_LIMIT)
    }

    /// In-memory version of the filter, used by the live stream. `after`/`before` are ignored, and
    /// `q` is a case-insensitive substring match rather than full-text search.
    pub fn matches(&self, e: &Envelope) -> bool {
        if self.project.as_ref().is_some_and(|p| *p != e.project) {
            return false;
        }
        if self.run.as_ref().is_some_and(|r| *r != e.run) {
            return false;
        }
        if let Some(k) = &self.kind
            && !kind_matches(k, &e.kind)
        {
            return false;
        }
        if self.level.is_some_and(|l| e.level() < l) {
            return false;
        }
        if let Some(q) = self.q.as_deref().filter(|q| !q.is_empty()) {
            let hay = serde_json::to_string(&e.payload).unwrap_or_default().to_lowercase();
            let label = e.meta.label.as_deref().unwrap_or_default().to_lowercase();
            let q = q.to_lowercase();
            if !hay.contains(&q) && !label.contains(&q) && !e.kind.contains(&q) {
                return false;
            }
        }
        true
    }
}

/// `pattern` is an exact kind or a prefix ending in `*`.
pub fn kind_matches(pattern: &str, kind: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(prefix) => kind.starts_with(prefix),
        None => pattern == kind,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Normaliser;
    use glint_protocol::{BodyFormat, parse_body};

    fn ev(json: &str) -> Envelope {
        let inc = parse_body(json.as_bytes(), BodyFormat::Json).unwrap().remove(0).unwrap();
        Normaliser::new().normalise(inc, chrono::Utc::now())
    }

    #[test]
    fn filters_by_kind_prefix_level_and_text() {
        let e = ev(r#"{"kind":"sql.query","payload":{"sql":"SELECT * FROM Orders"},"meta":{"level":"warn"},"project":"shop"}"#);
        let q = |s: &str| serde_json::from_str::<Query>(s).unwrap();
        assert!(q(r#"{"kind":"sql.*"}"#).matches(&e));
        assert!(!q(r#"{"kind":"core.*"}"#).matches(&e));
        assert!(q(r#"{"level":"warn"}"#).matches(&e));
        assert!(!q(r#"{"level":"error"}"#).matches(&e));
        assert!(q(r#"{"q":"orders"}"#).matches(&e));
        assert!(!q(r#"{"q":"users"}"#).matches(&e));
        assert!(!q(r#"{"project":"other"}"#).matches(&e));
    }

    #[test]
    fn limit_is_clamped() {
        assert_eq!(Query::default().limit(), 200);
        assert_eq!(Query { limit: Some(0), ..Default::default() }.limit(), 1);
        assert_eq!(Query { limit: Some(1_000_000), ..Default::default() }.limit(), 5000);
    }
}
