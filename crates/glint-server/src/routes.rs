use axum::Router;
use axum::body::Bytes;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{DefaultBodyLimit, Path, Query as QueryParams, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::Json;
use futures_util::{SinkExt, StreamExt};
use glint_core::{Hub, Query, SubmitError};
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
    "sql.query",
];

#[derive(Clone)]
pub struct AppState {
    pub hub: Hub,
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
        .route("/v1/events/{id}", get(one))
        .route("/v1/stream", get(stream))
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

pub(crate) fn error(status: StatusCode, message: String) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}
