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
    pub admin: Option<std::sync::Arc<dyn crate::Admin>>,
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
        .route("/v1/projects", get(projects))
        .route("/v1/events/{id}", get(one))
        .route("/v1/stream", get(stream))
        .route("/v1/hooks/{agent}", post(hook))
        .route("/v1/browser.js", get(browser_js))
        .route("/v1/expect", post(expect))
        .route("/v1/settings", get(settings_get).put(settings_put))
        .route("/v1/setup", get(crate::setup::status).post(crate::setup::install))
        .route("/v1/report", post(crate::setup::report))
        .route("/v1/data/prune", post(data_prune))
        .route("/v1/data/clear", post(data_clear))
        .route("/v1/source", get(crate::source::source))
        .route("/v1/open", post(crate::source::open))
        .route("/v1/editors", get(crate::source::editors))
        .route("/v1/agents/stats", get(agent_stats))
        .route("/v1/agents/reads", get(agent_reads))
        .route("/v1/agents/activity", post(agent_activity))
        .route("/v1/ctl", get(pauses))
        .route("/v1/ctl/{id}", get(wait_pause))
        .route("/v1/ctl/{id}/resume", post(resume))
        .route("/mcp", post(mcp).get(|| async { StatusCode::METHOD_NOT_ALLOWED }).delete(|| async { StatusCode::NO_CONTENT }))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .with_state(state)
}

/// `POST /v1/expect`: wait for expectations over events (see glint_core::expect) and report.
/// `{expect: [...], ordered?, no_errors?, project?, run?, after?, lookback_s?, timeout_s?}` →
/// `{pass, steps, failure?, events_seen, report}`.
async fn expect(State(hub): State<Hub>, Json(r): Json<glint_core::expect::Request>) -> Response {
    let x = match r.expectation() {
        Ok(x) => x,
        Err(e) => return error(StatusCode::BAD_REQUEST, e),
    };
    match glint_core::expect::run(&hub, x, r.scope).await {
        Ok(v) => {
            let mut body = serde_json::to_value(&v).unwrap_or_default();
            body["report"] = json!(v.report());
            Json(body).into_response()
        }
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

// ---- settings and data (the Settings page)

fn no_admin() -> Response {
    error(StatusCode::NOT_FOUND, "settings are not available here".into())
}

async fn settings_get(State(s): State<AppState>) -> Response {
    match &s.admin {
        Some(a) => Json(a.settings()).into_response(),
        None => no_admin(),
    }
}

async fn settings_put(State(s): State<AppState>, Json(changes): Json<serde_json::Map<String, serde_json::Value>>) -> Response {
    let Some(a) = s.admin.clone() else { return no_admin() };
    match tokio::task::spawn_blocking(move || a.update(&changes)).await {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => error(StatusCode::BAD_REQUEST, e),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn data_prune(State(s): State<AppState>) -> Response {
    let Some(a) = s.admin.clone() else { return no_admin() };
    match tokio::task::spawn_blocking(move || a.prune_now()).await {
        Ok(Ok(n)) => Json(json!({ "removed": n })).into_response(),
        Ok(Err(e)) => error(StatusCode::INTERNAL_SERVER_ERROR, e),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn data_clear(State(s): State<AppState>) -> Response {
    let Some(a) = s.admin.clone() else { return no_admin() };
    match tokio::task::spawn_blocking(move || a.clear_all()).await {
        Ok(Ok(n)) => Json(json!({ "removed": n })).into_response(),
        Ok(Err(e)) => error(StatusCode::INTERNAL_SERVER_ERROR, e),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

/// The browser SDK (`sdks/browser/glint.js`), for a `<script>` tag in a page under development.
async fn browser_js() -> Response {
    (
        [
            (header::CONTENT_TYPE, "application/javascript; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
            // Pages that are cross-origin isolated (WordPress's block editor is) may only load
            // scripts from other origins that allow it.
            (header::HeaderName::from_static("cross-origin-resource-policy"), "cross-origin"),
        ],
        include_str!("../../../sdks/browser/glint.js"),
    )
        .into_response()
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
        "data": s.admin.as_ref().map(|a| a.settings()["data"].clone()),
        "settings": s.admin.as_ref().map(|a| a.settings()["settings"].clone()),
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
    match hub.runs(p.project.filter(|s| !s.is_empty()), p.limit.unwrap_or(50).clamp(1, 5000)).await {
        Ok(runs) => Json(json!({ "runs": runs })).into_response(),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

/// Every project, most recently active first, with its run counts: what the project menu lists,
/// however many runs there are.
async fn projects(State(hub): State<Hub>) -> Response {
    match hub.projects().await {
        Ok(projects) => Json(json!({ "projects": projects })).into_response(),
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
async fn mcp(State(s): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let msg: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => return (StatusCode::BAD_REQUEST, Json(glint_mcp::error(serde_json::Value::Null, -32700, &format!("parse error: {e}")))).into_response(),
    };
    let header = |name: &str| headers.get(name).and_then(|v| v.to_str().ok()).map(str::to_owned).filter(|v| !v.is_empty());
    let caller = glint_mcp::Caller { session: header("mcp-session-id"), project: header("x-glint-project") };
    let out = s.mcp.handle_from(msg, &caller).await;
    match out.reply {
        Some(reply) => {
            let mut res = Json(reply).into_response();
            if let Some(v) = out.session.and_then(|sid| axum::http::HeaderValue::from_str(&sid).ok()) {
                res.headers_mut().insert("mcp-session-id", v);
            }
            res
        }
        None => StatusCode::ACCEPTED.into_response(),
    }
}

// ---- what agents did with Glint (glint_core::agents)

#[derive(serde::Deserialize)]
struct StatsParams {
    days: Option<u32>,
    project: Option<String>,
    within_min: Option<u64>,
}

/// `GET /v1/agents/stats?days=7&project=&within_min=10`: agents, tool calls, hook actions and
/// how fast failures reached an agent, per day and project.
async fn agent_stats(State(hub): State<Hub>, QueryParams(p): QueryParams<StatsParams>) -> Response {
    let days = p.days.unwrap_or(7).clamp(1, 3650);
    match hub.agent_stats(days, p.project.filter(|s| !s.is_empty()), p.within_min.unwrap_or(10).clamp(1, 1440)).await {
        Ok(st) => Json(st).into_response(),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

#[derive(serde::Deserialize)]
struct ReadsParams {
    run: String,
}

/// `GET /v1/agents/reads?run=R`: which events of a run agents saw, and when.
async fn agent_reads(State(hub): State<Hub>, QueryParams(p): QueryParams<ReadsParams>) -> Response {
    match hub.reads_in_run(p.run).await {
        Ok(reads) => Json(json!({ "reads": reads })).into_response(),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

/// `POST /v1/agents/activity`: one activity or a list, from `glint hook` (what it told the agent).
async fn agent_activity(State(hub): State<Hub>, body: Bytes) -> Response {
    let v: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => return error(StatusCode::BAD_REQUEST, e.to_string()),
    };
    let list = match v {
        serde_json::Value::Array(items) => items,
        one => vec![one],
    };
    let mut parsed = Vec::with_capacity(list.len());
    for item in list.into_iter().take(100) {
        match serde_json::from_value::<glint_core::agents::Activity>(item) {
            // Only hooks report here; MCP calls are recorded by the MCP server itself.
            Ok(a) if a.action.starts_with("hook.") => parsed.push(glint_core::agents::Activity { error_events: 0, ..a }),
            Ok(a) => return error(StatusCode::BAD_REQUEST, format!("unexpected action `{}` (hook.* only)", a.action)),
            Err(e) => return error(StatusCode::BAD_REQUEST, e.to_string()),
        }
    }
    let n = parsed.len();
    hub.record_activity(parsed).await;
    Json(json!({ "accepted": n })).into_response()
}

pub(crate) fn error(status: StatusCode, message: String) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}
