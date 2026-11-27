use axum::Router;
use axum::body::Bytes;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{DefaultBodyLimit, Path, Query as QueryParams, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::Json;
use futures_util::{SinkExt, StreamExt};
use glint_core::control::ResolveError;
use glint_core::{Hub, PauseState, Query, Resolution, ResumeError, SubmitError};
use glint_ingest::native;
use glint_protocol::{MAX_BODY_BYTES, PROTOCOL_VERSION};
use serde_json::json;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::watch;

/// Kinds the bundled UI renders specially. Everything else renders as a JSON tree.
const KNOWN_KINDS: &[&str] = &[
    "core.value",
    "core.log",
    "core.text",
    "core.json",
    "core.html",
    "core.markdown",
    "core.mermaid",
    "core.table",
    "core.exception",
    "core.trace",
    "core.measure",
    "core.separator",
    "core.screen",
    "core.clear",
    "core.update",
    "ctl.pause",
    "agent.session",
    "agent.step",
    "agent.tool_call",
    "agent.tool_result",
    "agent.permission",
    "llm.call",
    "core.span",
    "ctl.resume",
    "sql.query",
];

#[derive(Clone)]
pub struct AppState {
    pub hub: Hub,
    pub mcp: glint_mcp::McpServer,
    pub stop: watch::Receiver<bool>,
    /// Adapters this daemon runs, for `/v1/info`.
    pub adapters: std::sync::Arc<Vec<String>>,
}

impl axum::extract::FromRef<AppState> for Hub {
    fn from_ref(s: &AppState) -> Hub {
        s.hub.clone()
    }
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/v1/info", get(info))
        .route("/v1/events", post(ingest).get(list))
        .route("/v1/runs", get(runs))
        .route("/v1/events/{id}", get(one))
        .route("/v1/stream", get(stream))
        .route("/v1/hooks/{agent}", post(hook))
        .route("/v1/ctl", get(pauses))
        .route("/v1/ctl/{id}", get(wait_pause))
        .route("/v1/ctl/{id}/resume", post(resume))
        .route("/mcp", post(mcp).get(|| async { StatusCode::METHOD_NOT_ALLOWED }).delete(|| async { StatusCode::NO_CONTENT }))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .with_state(state)
}

async fn index() -> Html<&'static str> {
    Html(crate::INDEX_HTML)
}

async fn info(State(s): State<AppState>) -> Json<serde_json::Value> {
    Json(json!({
        "name": "glint",
        "version": env!("CARGO_PKG_VERSION"),
        "protocol": [PROTOCOL_VERSION],
        "adapters": *s.adapters,
        "kinds": KNOWN_KINDS,
    }))
}

async fn ingest(State(hub): State<Hub>, headers: HeaderMap, body: Bytes) -> Response {
    let ct = headers.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok());
    let decoded = match native::decode(&body, ct) {
        Ok(d) => d,
        Err(e) => return error(StatusCode::BAD_REQUEST, e.to_string()),
    };
    let events = hub.normalise(decoded.envelopes);
    let ids: Vec<String> = events.iter().map(|e| e.id.clone()).collect();
    match hub.submit(events).await {
        Ok(()) => {}
        Err(e @ SubmitError::Closed) => return error(StatusCode::SERVICE_UNAVAILABLE, e.to_string()),
        Err(e @ SubmitError::NotStored(_)) => return error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
    (
        StatusCode::ACCEPTED,
        Json(json!({ "accepted": ids.len(), "ids": ids, "rejected": decoded.rejected })),
    )
        .into_response()
}

#[derive(serde::Deserialize)]
struct RunsParams {
    project: Option<String>,
    limit: Option<usize>,
}

/// Most recently active runs, newest first (`docs/PROTOCOL.md` §7).
async fn runs(State(hub): State<Hub>, QueryParams(p): QueryParams<RunsParams>) -> Response {
    match hub.runs(p.project.filter(|s| !s.is_empty()), p.limit.unwrap_or(50).clamp(1, 500)).await {
        Ok(runs) => Json(json!({ "runs": runs })).into_response(),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn list(State(hub): State<Hub>, QueryParams(q): QueryParams<Query>) -> Response {
    match hub.query(q).await {
        Ok(events) => Json(json!({ "events": events })).into_response(),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn one(State(hub): State<Hub>, Path(id): Path<String>) -> Response {
    match hub.get(id).await {
        Ok(Some(e)) => Json(e).into_response(),
        Ok(None) => error(StatusCode::NOT_FOUND, "no such event".into()),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn stream(State(s): State<AppState>, QueryParams(q): QueryParams<Query>, ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(move |socket| pump(socket, s.hub, s.stop, q))
}

/// Sends every stored event matching `filter` until either side goes away. A client that
/// falls too far behind is told how many events it skipped, then continues live.
async fn pump(socket: WebSocket, hub: Hub, mut stop: watch::Receiver<bool>, filter: Query) {
    let mut events = hub.subscribe();
    drop(hub);
    let (mut tx, mut rx) = socket.split();
    loop {
        tokio::select! {
            // Drop the watch guard inside the branch future; it is not Send.
            () = async { let _ = stop.wait_for(|s| *s).await; } => {
                let _ = tx.send(Message::Close(None)).await;
                break;
            }
            got = events.recv() => {
                let text = match got {
                    Ok(e) if filter.matches(&e) => match serde_json::to_string(&*e) {
                        Ok(t) => t,
                        Err(_) => continue,
                    },
                    Ok(_) => continue,
                    Err(RecvError::Lagged(n)) => json!({ "glint": "lagged", "skipped": n }).to_string(),
                    Err(RecvError::Closed) => break,
                };
                if tx.send(Message::Text(text.into())).await.is_err() {
                    break;
                }
            }
            msg = rx.next() => match msg {
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(_)) => {}
            },
        }
    }
}

/// Coding-agent hooks, forwarded raw by `glint hook`. Answers fast: the agent is waiting.
async fn hook(State(hub): State<Hub>, Path(agent): Path<String>, body: Bytes) -> Response {
    let decoded = match agent.as_str() {
        "claude-code" => glint_ingest::agent::claude_code::decode(&body).map_err(|e| e.to_string()),
        other => return error(StatusCode::NOT_FOUND, format!("no hook adapter for `{other}` (known: claude-code)")),
    };
    let incoming = match decoded {
        Ok(v) => v,
        Err(e) => return error(StatusCode::BAD_REQUEST, e),
    };
    let events = hub.normalise(incoming);
    let n = events.len();
    match hub.submit(events).await {
        Ok(()) => Json(json!({ "accepted": n })).into_response(),
        Err(e) => error(StatusCode::SERVICE_UNAVAILABLE, e.to_string()),
    }
}

async fn pauses(State(hub): State<Hub>) -> Json<serde_json::Value> {
    Json(json!({ "pauses": hub.control().pending() }))
}

#[derive(serde::Deserialize)]
struct WaitParams {
    timeout_s: Option<u64>,
}

/// Long-poll for a pause's answer (`docs/PROTOCOL.md` §5.1): 200 with the answer, 204 while
/// still paused, 404 when the pause is unknown (the client should continue).
async fn wait_pause(State(hub): State<Hub>, Path(id): Path<String>, QueryParams(p): QueryParams<WaitParams>) -> Response {
    let timeout = std::time::Duration::from_secs(p.timeout_s.unwrap_or(30).clamp(1, 60));
    match hub.control().wait(&id, timeout).await {
        PauseState::Resolved(r) => Json(r).into_response(),
        PauseState::Pending => StatusCode::NO_CONTENT.into_response(),
        PauseState::Unknown => error(StatusCode::NOT_FOUND, format!("no pause with id `{id}`")),
    }
}

#[derive(serde::Deserialize, Default)]
struct ResumeBody {
    value: Option<serde_json::Value>,
    #[serde(default)]
    stop: bool,
    by: Option<String>,
}

async fn resume(State(hub): State<Hub>, Path(id): Path<String>, body: Bytes) -> Response {
    let b: ResumeBody = if body.is_empty() {
        ResumeBody::default()
    } else {
        match serde_json::from_slice(&body) {
            Ok(b) => b,
            Err(e) => return error(StatusCode::BAD_REQUEST, e.to_string()),
        }
    };
    let r = if b.stop { Resolution::Stop } else { Resolution::Continue { value: b.value } };
    let by = match b.by.as_deref() {
        Some("ui") => "ui",
        _ => "api",
    };
    match hub.resume(&id, r, by).await {
        Ok(info) => Json(json!({ "resumed": info.id })).into_response(),
        Err(ResumeError::Resolve(e @ ResolveError::Unknown(_))) => error(StatusCode::NOT_FOUND, e.to_string()),
        Err(ResumeError::Resolve(e @ ResolveError::AlreadyResolved(_))) => error(StatusCode::CONFLICT, e.to_string()),
        Err(e) => error(StatusCode::SERVICE_UNAVAILABLE, e.to_string()),
    }
}

/// MCP over Streamable HTTP: JSON in, JSON out (no SSE stream needed for these tools).
async fn mcp(State(s): State<AppState>, body: Bytes) -> Response {
    let msg: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => return (StatusCode::BAD_REQUEST, Json(glint_mcp::error(serde_json::Value::Null, -32700, &format!("parse error: {e}")))).into_response(),
    };
    let is_init = msg.get("method").and_then(serde_json::Value::as_str) == Some("initialize");
    match s.mcp.handle(msg).await {
        Some(reply) => {
            let mut res = Json(reply).into_response();
            if is_init && let Ok(v) = axum::http::HeaderValue::from_str(&format!("glint-{}", std::process::id())) {
                res.headers_mut().insert("mcp-session-id", v);
            }
            res
        }
        None => StatusCode::ACCEPTED.into_response(),
    }
}

pub(crate) fn error(status: StatusCode, message: String) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}
