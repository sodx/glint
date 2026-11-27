//! Glint core: turns incoming envelopes into complete ones, persists them through a
//! [`Store`] and fans them out on a bus. Knows no transport, no storage engine and no
//! language.

pub mod agents;
pub mod control;
mod hub;
mod normalise;
pub mod config;
pub mod expect;
pub mod project;
mod query;
pub mod redact;

pub use control::{PauseInfo, PauseState, Resolution};
pub use hub::{Hub, HubConfig, ResumeError, SubmitError};
pub use normalise::Normaliser;
pub use query::Query;
pub use redact::{RedactConfig, Redactor};

use glint_protocol::Envelope;

/// Persistence for envelopes. Implementations are synchronous; the hub calls them from a
/// blocking thread.
pub trait Store: Send + Sync + 'static {
    /// Persists a batch atomically, in order.
    fn insert(&self, events: &[Envelope]) -> Result<(), StoreError>;
    /// Events matching `q`, oldest first, at most `q.limit` of the newest matches.
    fn query(&self, q: &Query) -> Result<Vec<Envelope>, StoreError>;
    /// One event by id.
    fn get(&self, id: &str) -> Result<Option<Envelope>, StoreError>;
    /// The most recently active runs, newest first.
    fn runs(&self, project: Option<&str>, limit: usize) -> Result<Vec<RunSummary>, StoreError>;

    /// Records what agents did with Glint (see [`agents`]). Stores that don't keep it ignore it.
    fn record_activity(&self, _activity: &[agents::Activity]) -> Result<(), StoreError> {
        Ok(())
    }
    /// Agent activity since `since`, oldest first, with `error_events` filled in and, where the
    /// agent named no project, the project of the first event it read.
    fn activity_since(&self, _since: chrono::DateTime<chrono::Utc>) -> Result<Vec<agents::Activity>, StoreError> {
        Ok(Vec::new())
    }
    /// Program failures since `since`, oldest first (not an agent's own failed tool calls).
    fn failures_since(&self, _since: chrono::DateTime<chrono::Utc>) -> Result<Vec<agents::Failure>, StoreError> {
        Ok(Vec::new())
    }
    /// Which events of `run` agents saw, first time per event, agent and way.
    fn reads_in_run(&self, _run: &str) -> Result<Vec<agents::EventRead>, StoreError> {
        Ok(Vec::new())
    }
}

/// One run (a process, request, test or agent session) at a glance.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct RunSummary {
    pub run: String,
    pub project: String,
    pub first: String,
    pub last: String,
    pub events: u64,
    pub errors: u64,
    /// Id of the newest event in the run, a cursor for queries.
    pub last_id: String,
    /// The calling file of the run's first event: what tells runs of one language apart.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// The run's first log line (e.g. `GET /checkout/8`), when it starts with one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("storage backend: {0}")]
    Backend(String),
    #[error("stored event is unreadable: {0}")]
    Corrupt(String),
}
