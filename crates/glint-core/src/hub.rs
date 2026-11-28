use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::Utc;
use glint_protocol::{Envelope, IncomingEnvelope, Meta, Origin};
use serde_json::{Value, json};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::{Instant, timeout_at};

use crate::control::{Control, PauseInfo, Resolution, ResolveError};
use crate::{Normaliser, Query, RunSummary, Store, StoreError};

#[derive(Debug, Clone)]
pub struct HubConfig {
    /// Most events written in one transaction.
    pub batch_max: usize,
    /// Longest an event waits for its batch to fill.
    pub batch_window: Duration,
    /// Submissions queued for the writer before `submit` waits.
    pub queue: usize,
    /// Events a slow live subscriber may fall behind before it skips ahead.
    pub bus: usize,
}

impl Default for HubConfig {
    fn default() -> Self {
        Self {
            batch_max: 1000,
            batch_window: Duration::from_millis(5),
            queue: 1024,
            bus: 4096,
        }
    }
}

/// The hub every adapter writes into and every reader reads from. Events are persisted
/// first and published second, so anything seen on the bus can also be queried.
#[derive(Clone)]
pub struct Hub {
    inner: Arc<Inner>,
}

struct Inner {
    store: Arc<dyn Store>,
    tx: mpsc::Sender<Job>,
    bus: broadcast::Sender<Arc<Envelope>>,
    normaliser: Mutex<Normaliser>,
    control: Control,
}

/// One `submit` call: its events and who to tell once they are stored.
struct Job {
    events: Vec<Envelope>,
    done: oneshot::Sender<Result<(), String>>,
}

#[derive(Debug, thiserror::Error)]
pub enum ResumeError {
    #[error(transparent)]
    Resolve(#[from] ResolveError),
    #[error(transparent)]
    Submit(#[from] SubmitError),
}

#[derive(Debug, thiserror::Error)]
pub enum SubmitError {
    #[error("the event writer has stopped")]
    Closed,
    #[error("events were not stored: {0}")]
    NotStored(String),
}

impl Hub {
    /// Starts the writer task. It ends once every clone of the returned hub is dropped and
    /// the queue is drained.
    pub fn start(store: Arc<dyn Store>, cfg: HubConfig) -> (Hub, JoinHandle<()>) {
        let (tx, rx) = mpsc::channel(cfg.queue);
        let (bus, _) = broadcast::channel(cfg.bus);
        let writer = tokio::spawn(write_loop(store.clone(), rx, bus.clone(), cfg));
        let hub = Hub {
            inner: Arc::new(Inner {
                store,
                tx,
                bus,
                normaliser: Mutex::new(Normaliser::new()),
                control: Control::new(),
            }),
        };
        (hub, writer)
    }

    /// Replaces the secrets redaction rules for events normalised from now on.
    pub fn set_redactor(&self, r: Arc<crate::Redactor>) {
        self.inner.normaliser.lock().unwrap_or_else(|p| p.into_inner()).set_redactor(r);
    }

    /// Completes envelopes with server defaults, in order.
    pub fn normalise(&self, incoming: Vec<IncomingEnvelope>) -> Vec<Envelope> {
        let now = Utc::now();
        let mut n = self.inner.normaliser.lock().unwrap_or_else(|p| p.into_inner());
        incoming.into_iter().map(|e| n.normalise(e, now)).collect()
    }

    /// Stores and then publishes `events`. Returns once they are durable, so a read
    /// issued afterwards sees them. Concurrent submissions share one transaction.
    pub async fn submit(&self, events: Vec<Envelope>) -> Result<(), SubmitError> {
        if events.is_empty() {
            return Ok(());
        }
        let pauses: Vec<PauseInfo> = events
            .iter()
            .filter(|e| e.kind == "ctl.pause")
            .map(PauseInfo::from_event)
            .collect();
        let (done, stored) = oneshot::channel();
        self.inner
            .tx
            .send(Job { events, done })
            .await
            .map_err(|_| SubmitError::Closed)?;
        stored
            .await
            .map_err(|_| SubmitError::Closed)?
            .map_err(SubmitError::NotStored)?;
        // Registered only once stored, so a pause is never waitable without its event.
        for p in pauses {
            self.inner.control.register(p);
        }
        Ok(())
    }

    /// The control plane: paused programs.
    pub fn control(&self) -> &Control {
        &self.inner.control
    }

    /// Answers a pause and records the answer as a `ctl.resume` event aimed at it. `by` says
    /// who answered (`ui`, `mcp`, `api`).
    pub async fn resume(&self, id: &str, r: Resolution, by: &str) -> Result<PauseInfo, ResumeError> {
        let info = self.inner.control.resolve(id, r.clone())?;
        let mut payload = serde_json::to_value(&r).unwrap_or(Value::Null);
        payload["target"] = json!(id);
        payload["by"] = json!(by);
        let event = IncomingEnvelope {
            v: None,
            id: None,
            ts: None,
            run: Some(info.run.clone()),
            parent: None,
            project: Some(info.project.clone()),
            origin: Origin { lang: Some("glint".into()), sdk: Some(format!("glintd/{}", env!("CARGO_PKG_VERSION"))), ..Origin::default() },
            kind: "ctl.resume".into(),
            payload,
            meta: Meta::default(),
        };
        let events = self.normalise(vec![event]);
        self.submit(events).await?;
        Ok(info)
    }

    pub async fn runs(&self, project: Option<String>, limit: usize) -> Result<Vec<RunSummary>, StoreError> {
        let store = self.inner.store.clone();
        tokio::task::spawn_blocking(move || store.runs(project.as_deref(), limit))
            .await
            .map_err(|e| StoreError::Backend(e.to_string()))?
    }

    pub async fn projects(&self) -> Result<Vec<crate::ProjectSummary>, StoreError> {
        let store = self.inner.store.clone();
        tokio::task::spawn_blocking(move || store.projects())
            .await
            .map_err(|e| StoreError::Backend(e.to_string()))?
    }

    /// Records agent activity. Never fails the caller: losing a statistic is fine.
    pub async fn record_activity(&self, activity: Vec<crate::agents::Activity>) {
        if activity.is_empty() {
            return;
        }
        let store = self.inner.store.clone();
        let list: Vec<_> = activity.into_iter().map(crate::agents::Activity::bounded).collect();
        match tokio::task::spawn_blocking(move || store.record_activity(&list)).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::warn!(%e, "agent activity was not recorded"),
            Err(e) => tracing::warn!(%e, "agent activity was not recorded"),
        }
    }

    /// Agent statistics for the last `days` days (see [`crate::agents::stats`]).
    pub async fn agent_stats(&self, days: u32, project: Option<String>, within_min: u64) -> Result<crate::agents::AgentStats, StoreError> {
        let store = self.inner.store.clone();
        let since = Utc::now() - chrono::Duration::days(i64::from(days.max(1)));
        tokio::task::spawn_blocking(move || {
            let act = store.activity_since(since)?;
            let fails = store.failures_since(since)?;
            Ok(crate::agents::stats(&act, &fails, Some(since), within_min, project.as_deref()))
        })
        .await
        .map_err(|e| StoreError::Backend(e.to_string()))?
    }

    /// Which events of `run` agents saw.
    pub async fn reads_in_run(&self, run: String) -> Result<Vec<crate::agents::EventRead>, StoreError> {
        let store = self.inner.store.clone();
        tokio::task::spawn_blocking(move || store.reads_in_run(&run))
            .await
            .map_err(|e| StoreError::Backend(e.to_string()))?
    }

    /// Live events, after they are stored.
    pub fn subscribe(&self) -> broadcast::Receiver<Arc<Envelope>> {
        self.inner.bus.subscribe()
    }

    pub async fn query(&self, q: Query) -> Result<Vec<Envelope>, StoreError> {
        let store = self.inner.store.clone();
        tokio::task::spawn_blocking(move || store.query(&q))
            .await
            .map_err(|e| StoreError::Backend(e.to_string()))?
    }

    pub async fn get(&self, id: String) -> Result<Option<Envelope>, StoreError> {
        let store = self.inner.store.clone();
        tokio::task::spawn_blocking(move || store.get(&id))
            .await
            .map_err(|e| StoreError::Backend(e.to_string()))?
    }
}

async fn write_loop(
    store: Arc<dyn Store>,
    mut rx: mpsc::Receiver<Job>,
    bus: broadcast::Sender<Arc<Envelope>>,
    cfg: HubConfig,
) {
    while let Some(first) = rx.recv().await {
        let mut count = first.events.len();
        let mut jobs = vec![first];
        let deadline = Instant::now() + cfg.batch_window;
        while count < cfg.batch_max {
            match timeout_at(deadline, rx.recv()).await {
                Ok(Some(job)) => {
                    count += job.events.len();
                    jobs.push(job);
                }
                Ok(None) | Err(_) => break,
            }
        }
        let (events, waiters): (Vec<Vec<Envelope>>, Vec<_>) = jobs.into_iter().map(|j| (j.events, j.done)).unzip();
        let batch: Vec<Envelope> = events.into_iter().flatten().collect();
        let s = store.clone();
        let result = tokio::task::spawn_blocking(move || s.insert(&batch).map(|()| batch))
            .await
            .unwrap_or_else(|e| Err(StoreError::Backend(format!("store writer panicked: {e}"))));
        match result {
            Ok(batch) => {
                for w in waiters {
                    let _ = w.send(Ok(()));
                }
                for e in batch {
                    // No subscribers is fine: nobody is watching live.
                    let _ = bus.send(Arc::new(e));
                }
            }
            Err(err) => {
                tracing::error!(%err, "a batch of events could not be stored");
                let msg = err.to_string();
                for w in waiters {
                    let _ = w.send(Err(msg.clone()));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glint_protocol::{BodyFormat, parse_body};

    #[derive(Default)]
    struct MemStore(Mutex<Vec<Envelope>>);

    impl Store for MemStore {
        fn insert(&self, events: &[Envelope]) -> Result<(), StoreError> {
            self.0.lock().unwrap().extend_from_slice(events);
            Ok(())
        }
        fn query(&self, q: &Query) -> Result<Vec<Envelope>, StoreError> {
            Ok(self.0.lock().unwrap().iter().filter(|e| q.matches(e)).cloned().collect())
        }
        fn get(&self, id: &str) -> Result<Option<Envelope>, StoreError> {
            Ok(self.0.lock().unwrap().iter().find(|e| e.id == id).cloned())
        }
        fn runs(&self, _: Option<&str>, _: usize) -> Result<Vec<RunSummary>, StoreError> {
            Ok(vec![])
        }
    }

    #[tokio::test]
    async fn pauses_register_on_store_and_resume_records_an_event() {
        let (hub, _w) = Hub::start(Arc::new(MemStore::default()), HubConfig::default());
        let inc = parse_body(br#"{"id":"p1","kind":"ctl.pause","payload":{"reason":"check"},"run":"r1"}"#, BodyFormat::Json)
            .unwrap()
            .into_iter()
            .map(Result::unwrap)
            .collect();
        let events = hub.normalise(inc);
        hub.submit(events).await.unwrap();
        assert_eq!(hub.control().pending()[0].reason.as_deref(), Some("check"));
        hub.resume("p1", Resolution::Continue { value: Some(json!(3)) }, "test").await.unwrap();
        assert!(hub.control().pending().is_empty());
        let all = hub.query(Query::default()).await.unwrap();
        let resume = all.iter().find(|e| e.kind == "ctl.resume").unwrap();
        assert_eq!(resume.payload, json!({"action": "continue", "value": 3, "target": "p1", "by": "test"}));
        assert_eq!(resume.run, "r1");
    }

    #[tokio::test]
    async fn events_are_stored_before_they_are_published() {
        let store = Arc::new(MemStore::default());
        let (hub, _w) = Hub::start(store.clone(), HubConfig::default());
        let mut sub = hub.subscribe();
        let inc = parse_body(br#"[{"kind":"core.value","payload":1},{"kind":"core.value","payload":2}]"#, BodyFormat::Json)
            .unwrap()
            .into_iter()
            .map(Result::unwrap)
            .collect();
        let events = hub.normalise(inc);
        hub.submit(events).await.unwrap();
        // Read-your-writes: stored before submit returns.
        assert_eq!(hub.query(Query::default()).await.unwrap().len(), 2);
        let first = sub.recv().await.unwrap();
        assert_eq!(first.payload, 1);
        assert!(hub.get(first.id.clone()).await.unwrap().is_some());
        assert_eq!(sub.recv().await.unwrap().payload, 2);
        assert_eq!(hub.query(Query::default()).await.unwrap().len(), 2);
    }
}
