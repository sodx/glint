//! A small blocking client for `POST /v1/events`.

use std::time::Duration;

use serde_json::Value;

pub struct Client {
    agent: ureq::Agent,
    url: String,
}

#[derive(Debug)]
pub struct Ack {
    pub ids: Vec<String>,
    pub rejected: Vec<(u64, String)>,
}

#[derive(Debug)]
pub enum ClientError {
    /// No answer: daemon down, wrong URL, timeout.
    Unreachable(String),
    /// The daemon answered with an error.
    Refused { status: u16, message: String },
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientError::Unreachable(m) => write!(f, "{m}"),
            ClientError::Refused { status, message } => write!(f, "HTTP {status}: {message}"),
        }
    }
}

impl Client {
    pub fn new(base: &str) -> Self {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(1))
            .timeout(Duration::from_secs(10))
            .build();
        Self {
            agent,
            url: format!("{}/v1/events", base.trim_end_matches('/')),
        }
    }

    pub fn base(&self) -> &str {
        self.url.trim_end_matches("/v1/events")
    }

    pub fn post(&self, envelopes: &[Value]) -> Result<Ack, ClientError> {
        let body = Value::Array(envelopes.to_vec()).to_string();
        let res = self
            .agent
            .post(&self.url)
            .set("Content-Type", "application/json")
            .send_string(&body);
        match res {
            Ok(r) => {
                let v: Value = r
                    .into_json()
                    .map_err(|e| ClientError::Refused { status: 202, message: format!("unreadable answer: {e}") })?;
                Ok(Ack {
                    ids: v["ids"]
                        .as_array()
                        .map(|a| a.iter().filter_map(|i| i.as_str().map(str::to_owned)).collect())
                        .unwrap_or_default(),
                    rejected: v["rejected"]
                        .as_array()
                        .map(|a| {
                            a.iter()
                                .map(|r| (r["index"].as_u64().unwrap_or(0), r["error"].as_str().unwrap_or("").to_owned()))
                                .collect()
                        })
                        .unwrap_or_default(),
                })
            }
            Err(ureq::Error::Status(status, r)) => {
                let text = r.into_string().unwrap_or_default();
                let message = serde_json::from_str::<Value>(&text)
                    .ok()
                    .and_then(|v| v["error"].as_str().map(str::to_owned))
                    .unwrap_or(text);
                Err(ClientError::Refused { status, message })
            }
            Err(e) => Err(ClientError::Unreachable(e.to_string())),
        }
    }
}
