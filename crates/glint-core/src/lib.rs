//! Glint core: turns incoming envelopes into complete ones, persists them through a
//! [`Store`] and fans them out on a bus. Knows no transport, no storage engine and no
//! language.

mod hub;
mod normalise;
mod query;

pub use hub::{Hub, HubConfig, SubmitError};
pub use normalise::Normaliser;
pub use query::Query;

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
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("storage backend: {0}")]
    Backend(String),
    #[error("stored event is unreadable: {0}")]
    Corrupt(String),
}
