//! Control plane: programs that wait for a human or an agent (`ctl.pause`).
//!
//! A pause starts when a `ctl.pause` event is stored and ends when someone resolves it
//! (UI button, MCP tool, API). Waiters long-poll; resolved pauses are remembered for a
//! while so a poll that arrives late still gets the answer. State is in memory: after a
//! daemon restart, pollers get "unknown" and continue, which is the safe default.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use glint_protocol::Envelope;
use serde::Serialize;
use serde_json::Value;
use tokio::sync::watch;

/// How long an unanswered pause is kept before it is forgotten.
const PENDING_TTL: Duration = Duration::from_secs(60 * 60);
/// How long a resolved pause keeps answering late polls.
const RESOLVED_TTL: Duration = Duration::from_secs(5 * 60);

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "action", rename_all = "lowercase")]
pub enum Resolution {
    /// Carry on, optionally with a value the program asked for.
    Continue {
        #[serde(skip_serializing_if = "Option::is_none")]
        value: Option<Value>,
    },
    /// Ask the program to stop.
    Stop,
}

/// What a paused program told us about itself.
#[derive(Debug, Clone, Serialize)]
pub struct PauseInfo {
    pub id: String,
    pub since: DateTime<Utc>,
    pub run: String,
    pub project: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
}

impl PauseInfo {
    pub fn from_event(e: &Envelope) -> Self {
        Self {
            id: e.id.clone(),
            since: e.ts,
            run: e.run.clone(),
            project: e.project.clone(),
            reason: e.payload.get("reason").and_then(Value::as_str).map(str::to_owned),
            file: e.origin.file.clone(),
            line: e.origin.line,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum PauseState {
    Pending,
    Resolved(Resolution),
    /// Never registered, or forgotten.
    Unknown,
}

struct Entry {
    info: PauseInfo,
    tx: watch::Sender<Option<Resolution>>,
    touched: Instant,
}

#[derive(Default)]
pub struct Control {
    pauses: Mutex<HashMap<String, Entry>>,
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ResolveError {
    #[error("no pause with id `{0}`")]
    Unknown(String),
    #[error("pause `{0}` was already resolved")]
    AlreadyResolved(String),
}

impl Control {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&self, info: PauseInfo) {
        let mut map = self.lock();
        gc(&mut map);
        map.entry(info.id.clone()).or_insert_with(|| Entry {
            info,
            tx: watch::channel(None).0,
            touched: Instant::now(),
        });
    }

    /// Pauses still waiting for an answer, oldest first.
    pub fn pending(&self) -> Vec<PauseInfo> {
        let mut map = self.lock();
        gc(&mut map);
        let mut out: Vec<PauseInfo> = map
            .values()
            .filter(|e| e.tx.borrow().is_none())
            .map(|e| e.info.clone())
            .collect();
        out.sort_by_key(|p| p.since);
        out
    }

    pub fn state(&self, id: &str) -> PauseState {
        match self.lock().get(id) {
            None => PauseState::Unknown,
            Some(e) => match &*e.tx.borrow() {
                None => PauseState::Pending,
                Some(r) => PauseState::Resolved(r.clone()),
            },
        }
    }

    pub fn resolve(&self, id: &str, r: Resolution) -> Result<PauseInfo, ResolveError> {
        let mut map = self.lock();
        let e = map.get_mut(id).ok_or_else(|| ResolveError::Unknown(id.to_owned()))?;
        if e.tx.borrow().is_some() {
            return Err(ResolveError::AlreadyResolved(id.to_owned()));
        }
        e.touched = Instant::now();
        e.tx.send_replace(Some(r));
        Ok(e.info.clone())
    }

    /// Waits up to `timeout` for `id` to be resolved.
    pub async fn wait(&self, id: &str, timeout: Duration) -> PauseState {
        let mut rx = match self.lock().get(id) {
            None => return PauseState::Unknown,
            Some(e) => e.tx.subscribe(),
        };
        let got = tokio::time::timeout(timeout, rx.wait_for(Option::is_some)).await;
        match got {
            Ok(Ok(r)) => PauseState::Resolved(r.clone().unwrap_or(Resolution::Continue { value: None })),
            _ => self.state(id),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Entry>> {
        self.pauses.lock().unwrap_or_else(|p| p.into_inner())
    }
}

fn gc(map: &mut HashMap<String, Entry>) {
    map.retain(|_, e| {
        let ttl = if e.tx.borrow().is_some() { RESOLVED_TTL } else { PENDING_TTL };
        e.touched.elapsed() < ttl
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(id: &str) -> PauseInfo {
        PauseInfo {
            id: id.into(),
            since: Utc::now(),
            run: "r".into(),
            project: "p".into(),
            reason: None,
            file: None,
            line: None,
        }
    }

    #[tokio::test]
    async fn pause_lifecycle() {
        let c = Control::new();
        assert_eq!(c.state("p1"), PauseState::Unknown);
        c.register(info("p1"));
        assert_eq!(c.state("p1"), PauseState::Pending);
        assert_eq!(c.pending().len(), 1);
        assert_eq!(c.wait("p1", Duration::from_millis(10)).await, PauseState::Pending);

        let r = Resolution::Continue { value: Some(serde_json::json!(5)) };
        c.resolve("p1", r.clone()).unwrap();
        assert_eq!(c.state("p1"), PauseState::Resolved(r.clone()));
        assert!(c.pending().is_empty());
        assert_eq!(c.resolve("p1", Resolution::Stop).unwrap_err(), ResolveError::AlreadyResolved("p1".into()));
        assert_eq!(c.resolve("nope", Resolution::Stop).unwrap_err(), ResolveError::Unknown("nope".into()));
        assert_eq!(c.wait("p1", Duration::from_secs(1)).await, PauseState::Resolved(r));
    }

    #[tokio::test]
    async fn a_waiter_wakes_when_resolved() {
        let c = std::sync::Arc::new(Control::new());
        c.register(info("p2"));
        let c2 = c.clone();
        let waiter = tokio::spawn(async move { c2.wait("p2", Duration::from_secs(5)).await });
        tokio::time::sleep(Duration::from_millis(20)).await;
        c.resolve("p2", Resolution::Stop).unwrap();
        assert_eq!(waiter.await.unwrap(), PauseState::Resolved(Resolution::Stop));
    }

    #[test]
    fn resolution_serialises_for_clients() {
        let v = serde_json::to_value(Resolution::Continue { value: Some(serde_json::json!("x")) }).unwrap();
        assert_eq!(v, serde_json::json!({"action": "continue", "value": "x"}));
        assert_eq!(serde_json::to_value(Resolution::Stop).unwrap(), serde_json::json!({"action": "stop"}));
    }
}
