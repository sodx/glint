//! Ray-compatible ingest (`docs/mappings/ray.md`).
//!
//! Ray clients POST `{uuid, payloads: [{type, content, origin}], meta}`. One `ray()` call is
//! one uuid; each chained call (`->color('red')`, `->label('x')`) is another request with the
//! same uuid carrying a command payload. Data payloads become envelopes with ids derived from
//! the uuid (`ray-<uuid>`, `ray-<uuid>-2`, …); command payloads become `core.update`
//! envelopes aimed at those ids.

mod map;

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use glint_protocol::{IncomingEnvelope, MAX_BODY_BYTES, Meta, Origin};
use serde_json::{Map, Value, json};

use map::{Mapped, map_payload};

/// How many uuids the adapter remembers to number follow-up payloads and aim commands.
const REMEMBERED_UUIDS: usize = 10_000;

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum RayError {
    #[error("body is {0} bytes; the limit is {MAX_BODY_BYTES}")]
    TooLarge(usize),
    #[error("body is not valid JSON: {0}")]
    InvalidJson(String),
    #[error("body is not a Ray request: {0}")]
    Shape(&'static str),
}

/// Translates Ray requests. Holds a bounded memory of uuids seen, so that a uuid that
/// sends data twice gets two entries and later commands reach both.
pub struct RayAdapter {
    seen: Mutex<Seen>,
}

struct Seen {
    data_count: HashMap<String, u32>,
    order: VecDeque<String>,
}

impl Default for RayAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl RayAdapter {
    pub fn new() -> Self {
        Self {
            seen: Mutex::new(Seen {
                data_count: HashMap::new(),
                order: VecDeque::new(),
            }),
        }
    }

    pub fn decode(&self, body: &[u8]) -> Result<Vec<IncomingEnvelope>, RayError> {
        if body.len() > MAX_BODY_BYTES {
            return Err(RayError::TooLarge(body.len()));
        }
        let req: Value = serde_json::from_slice(body).map_err(|e| RayError::InvalidJson(e.to_string()))?;
        let uuid = req
            .get("uuid")
            .and_then(Value::as_str)
            .filter(|u| !u.is_empty())
            .ok_or(RayError::Shape("missing `uuid`"))?
            .to_owned();
        let payloads = req
            .get("payloads")
            .and_then(Value::as_array)
            .ok_or(RayError::Shape("missing `payloads` array"))?;
        let meta = req.get("meta").cloned().unwrap_or(Value::Null);
        let client = Client::from_meta(&meta);

        let mut out = Vec::new();
        let mut seen = self.seen.lock().unwrap_or_else(|p| p.into_inner());
        for p in payloads {
            let ty = p.get("type").and_then(Value::as_str).unwrap_or("");
            let content = p.get("content").cloned().unwrap_or(Value::Null);
            let origin = client.origin(p.get("origin"));
            match map_payload(ty, content) {
                Mapped::Event { kind, payload, meta } => {
                    let n = seen.bump(&uuid);
                    out.push(IncomingEnvelope {
                        v: None,
                        id: Some(entry_id(&uuid, n)),
                        ts: None,
                        run: None,
                        parent: None,
                        project: client.project.clone(),
                        origin,
                        kind,
                        payload,
                        meta,
                    });
                }
                Mapped::Update(change) => {
                    // A command that arrives before any data still aims at the first entry;
                    // the UI holds updates for ids it has not seen yet.
                    let n = seen.count(&uuid).max(1);
                    for i in 1..=n {
                        let mut payload = change.clone();
                        payload.insert("target".into(), Value::String(entry_id(&uuid, i)));
                        out.push(IncomingEnvelope {
                            v: None,
                            id: None,
                            ts: None,
                            run: None,
                            parent: None,
                            project: client.project.clone(),
                            origin: origin.clone(),
                            kind: "core.update".into(),
                            payload: Value::Object(payload),
                            meta: Meta::default(),
                        });
                    }
                }
                Mapped::Ignore => {}
            }
        }
        Ok(out)
    }
}

impl Seen {
    fn count(&self, uuid: &str) -> u32 {
        self.data_count.get(uuid).copied().unwrap_or(0)
    }

    fn bump(&mut self, uuid: &str) -> u32 {
        if let Some(n) = self.data_count.get_mut(uuid) {
            *n += 1;
            return *n;
        }
        if self.order.len() >= REMEMBERED_UUIDS
            && let Some(old) = self.order.pop_front()
        {
            self.data_count.remove(&old);
        }
        self.order.push_back(uuid.to_owned());
        self.data_count.insert(uuid.to_owned(), 1);
        1
    }
}

fn entry_id(uuid: &str, n: u32) -> String {
    if n <= 1 { format!("ray-{uuid}") } else { format!("ray-{uuid}-{n}") }
}

/// What the request-level `meta` says about the sending library.
struct Client {
    lang: Option<String>,
    runtime: Option<String>,
    sdk: Option<String>,
    project: Option<String>,
}

impl Client {
    fn from_meta(meta: &Value) -> Self {
        let s = |k: &str| meta.get(k).and_then(Value::as_str).filter(|v| !v.is_empty()).map(str::to_owned);
        let (lang, runtime, sdk) = if let Some(v) = s("php_version") {
            (Some("php"), Some(v), s("ray_package_version").map(|r| format!("spatie/ray/{r}")))
        } else if let Some(v) = s("node_ray_package_version") {
            (Some("js"), None, Some(format!("node-ray/{v}")))
        } else {
            (None, None, s("ray_package_version").map(|r| format!("ray/{r}")))
        };
        Self {
            lang: lang.map(str::to_owned),
            runtime,
            sdk,
            project: s("project_name"),
        }
    }

    fn origin(&self, o: Option<&Value>) -> Origin {
        let o = o.unwrap_or(&Value::Null);
        let s = |k: &str| o.get(k).and_then(Value::as_str).filter(|v| !v.is_empty()).map(str::to_owned);
        let mut extra = Map::new();
        extra.insert("via".into(), json!("ray"));
        Origin {
            lang: self.lang.clone(),
            runtime: self.runtime.clone(),
            sdk: self.sdk.clone(),
            file: s("file"),
            line: o.get("line_number").and_then(Value::as_u64).and_then(|l| u32::try_from(l).ok()),
            function: s("function_name"),
            host: s("hostname"),
            extra,
            ..Origin::default()
        }
    }
}

#[cfg(test)]
mod tests;
