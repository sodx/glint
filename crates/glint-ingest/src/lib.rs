//! Ingest adapters. Each one turns foreign bytes into [`IncomingEnvelope`]s and nothing
//! else: no storage, no transport. M0 ships the native protocol only.

pub mod native;

pub use glint_protocol::IncomingEnvelope;
