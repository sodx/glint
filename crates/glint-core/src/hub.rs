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
    /// The writer's queue is full: the events were not taken. Clients drop them (HTTP 429).
    #[error("glint is busy storing events; these were dropped")]
    Busy,
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
        // A full queue answers at once instead of holding every request's body in memory until
        // the writer catches up.
        self.inner.tx.try_send(Job { events, done }).map_err(|e| match e {
            mpsc::error::TrySendError::Full(_) => SubmitError::Busy,
            mpsc::error::TrySendError::Closed(_) => SubmitError::Closed,
        })?;
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
        let result = tokio::task::spawn_blocking(move || {
            let mut batch = batch;
            for e in &mut batch {
                fit(e, MAX_STORED_EVENT_BYTES);
            }
            s.insert(&batch).map(|()| batch)
        })
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

/// Most bytes one event takes in the store (its JSON). The native API refuses larger envelopes;
/// other adapters (OTLP, agent hooks) can produce them, and they are cut down here.
pub const MAX_STORED_EVENT_BYTES: usize = glint_protocol::MAX_ENVELOPE_BYTES;

/// A cheap lower bound of a value's JSON size, so most events skip serialising twice.
fn rough_size(v: &Value) -> usize {
    match v {
        Value::String(s) => s.len() + 2,
        Value::Array(a) => a.iter().map(rough_size).sum::<usize>() + a.len() + 1,
        Value::Object(m) => m.iter().map(|(k, v)| k.len() + 3 + rough_size(v)).sum::<usize>() + 1,
        _ => 4,
    }
}

/// Cuts every string in `v` longer than `max` bytes, saying how much went.
fn cut_strings(v: &mut Value, max: usize) {
    match v {
        Value::String(s) if s.len() > max => {
            let mut end = max;
            while !s.is_char_boundary(end) {
                end -= 1;
            }
            let gone = s.len() - end;
            s.truncate(end);
            s.push_str(&format!("…[truncated {gone} bytes]"));
        }
        Value::Array(a) => a.iter_mut().for_each(|x| cut_strings(x, max)),
        Value::Object(m) => m.values_mut().for_each(|x| cut_strings(x, max)),
        _ => {}
    }
}

/// Makes an event fit in `max` bytes of JSON: cuts its longest strings, and when that is not
/// enough (a huge array of small values), keeps the start of the payload as a preview. Sets
/// `meta.truncated` to the original size. Returns whether it changed anything.
pub fn fit(e: &mut Envelope, max: usize) -> bool {
    let rough = rough_size(&e.payload) + e.meta.extra.iter().map(|(k, v)| k.len() + rough_size(v)).sum::<usize>();
    // JSON escaping can at most sextuple a string (`\u0001`), so anything under max/6 fits.
    if rough < max / 6 {
        return false;
    }
    let size = |e: &Envelope| serde_json::to_vec(e).map_or(usize::MAX, |b| b.len());
    let original = size(e);
    if original <= max {
        return false;
    }
    let mut cut = max / 4;
    while cut >= 64 {
        cut_strings(&mut e.payload, cut);
        for v in e.meta.extra.values_mut() {
            cut_strings(v, cut);
        }
        if size(e) <= max {
            e.meta.extra.insert("truncated".into(), json!(original));
            return true;
        }
        cut /= 4;
    }
    let text = serde_json::to_string(&e.payload).unwrap_or_default();
    let mut end = text.len().min(4096);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    e.payload = json!({ "$truncated": true, "bytes": original, "preview": &text[..end] });
    e.meta.extra.retain(|_, v| rough_size(v) < 1024);
    e.meta.extra.insert("truncated".into(), json!(original));
    true
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

    /// Holds every write until the test lets go.
    struct GatedStore(Arc<Mutex<()>>, MemStore);

    impl Store for GatedStore {
        fn insert(&self, events: &[Envelope]) -> Result<(), StoreError> {
            let _open = self.0.lock().unwrap();
            self.1.insert(events)
        }
        fn query(&self, q: &Query) -> Result<Vec<Envelope>, StoreError> {
            self.1.query(q)
        }
        fn get(&self, id: &str) -> Result<Option<Envelope>, StoreError> {
            self.1.get(id)
        }
        fn runs(&self, p: Option<&str>, l: usize) -> Result<Vec<RunSummary>, StoreError> {
            self.1.runs(p, l)
        }
    }

    fn one(hub: &Hub, n: i64) -> Vec<Envelope> {
        let inc = parse_body(format!(r#"{{"kind":"core.value","payload":{n}}}"#).as_bytes(), BodyFormat::Json).unwrap().into_iter().map(Result::unwrap).collect();
        hub.normalise(inc)
    }

    #[tokio::test(flavor = "multi_thread")]
    #[allow(clippy::await_holding_lock)] // the held gate is the point: it stalls the writer thread
    async fn a_full_queue_answers_busy_instead_of_waiting() {
        let gate = Arc::new(Mutex::new(()));
        let held = gate.lock().unwrap();
        let cfg = HubConfig { queue: 1, batch_max: 1, ..HubConfig::default() };
        let (hub, _w) = Hub::start(Arc::new(GatedStore(gate.clone(), MemStore::default())), cfg);
        // The writer takes the first job and waits on the gate; the second fills the queue.
        let first = tokio::spawn({
            let (h, e) = (hub.clone(), one(&hub, 1));
            async move { h.submit(e).await }
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        let second = tokio::spawn({
            let (h, e) = (hub.clone(), one(&hub, 2));
            async move { h.submit(e).await }
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(matches!(hub.submit(one(&hub, 3)).await, Err(SubmitError::Busy)));
        drop(held);
        first.await.unwrap().unwrap();
        second.await.unwrap().unwrap();
        assert_eq!(hub.query(Query::default()).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn oversized_events_are_cut_before_they_are_stored() {
        let store = Arc::new(MemStore::default());
        let (hub, _w) = Hub::start(store.clone(), HubConfig::default());
        let big = "x".repeat(3 * MAX_STORED_EVENT_BYTES);
        let mut events = one(&hub, 0);
        events[0].payload = json!({ "prompt": big, "small": "kept" });
        hub.submit(events).await.unwrap();
        let got = hub.query(Query::default()).await.unwrap().remove(0);
        assert!(serde_json::to_vec(&got).unwrap().len() <= MAX_STORED_EVENT_BYTES);
        assert_eq!(got.payload["small"], "kept");
        let prompt = got.payload["prompt"].as_str().unwrap();
        assert!(prompt.starts_with("xxxx") && prompt.contains("…[truncated "), "{}", &prompt[prompt.len() - 40..]);
        assert!(got.meta.extra["truncated"].as_u64().unwrap() > 3 * MAX_STORED_EVENT_BYTES as u64);
    }

    #[test]
    fn fit_falls_back_to_a_preview_and_leaves_small_events_alone() {
        let hub_less = |payload: Value| Envelope { payload, ..serde_json::from_value(json!({"v":1,"id":"a","ts":"2026-01-01T00:00:00Z","run":"r","project":"p","kind":"core.json","payload":null})).unwrap() };
        let mut small = hub_less(json!({"a": "b"}));
        assert!(!fit(&mut small, 1000));
        assert!(small.meta.extra.get("truncated").is_none());
        // Many small numbers: cutting strings cannot help.
        let mut many = hub_less(json!((0..5000).collect::<Vec<_>>()));
        assert!(fit(&mut many, 4000));
        assert_eq!(many.payload["$truncated"], true);
        assert!(serde_json::to_vec(&many).unwrap().len() <= 4000 + 4096);
        // Multi-byte text is cut on a character boundary.
        let mut text = hub_less(json!("é".repeat(5000)));
        assert!(fit(&mut text, 4000));
        assert!(text.payload.as_str().unwrap().contains("…[truncated "));
    }
}
