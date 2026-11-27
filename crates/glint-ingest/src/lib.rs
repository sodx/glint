//! Ingest adapters. Each one turns foreign bytes into [`IncomingEnvelope`]s and nothing
//! else: no storage, no transport.

pub mod agent;
pub mod native;
pub mod otlp;
pub mod ray;

pub use glint_protocol::IncomingEnvelope;
