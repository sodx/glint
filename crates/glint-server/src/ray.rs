//! Ray-compatible HTTP surface. Ray clients expect, on their own port:
//! - `POST /` with a Ray request body;
//! - `GET /_availability_check` answering with an HTTP error status (the PHP client reads a
//!   404 as "the app is up");
//! - `GET /locks/{name}` → `{"active", "stop_execution"}` while `pause()` waits, backed by the
//!   control plane: Continue / Stop in the UI or `glint_resume` over MCP answer it;
//! - `GET /windows` and `GET /theme`.

use std::sync::Arc;

use axum::Router;
use axum::Json;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use glint_core::{Hub, PauseState, Resolution, SubmitError};
use glint_ingest::ray::RayAdapter;
use glint_protocol::MAX_BODY_BYTES;
use serde_json::json;

use crate::routes::error;

#[derive(Clone)]
struct RayState {
    hub: Hub,
    adapter: Arc<RayAdapter>,
}

pub fn router(hub: Hub) -> Router {
    Router::new()
        .route("/", post(ingest))
        .route("/_availability_check", get(|| async { StatusCode::NOT_FOUND }))
        .route("/locks/{name}", get(lock))
        .route("/windows", get(|| async { Json(json!([])) }))
        .route("/theme", get(|| async { Json(json!({})) }))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .with_state(RayState {
            hub,
            adapter: Arc::new(RayAdapter::new()),
        })
}

/// Ray's `pause()` polls this every second while its lock is "active".
async fn lock(State(s): State<RayState>, Path(name): Path<String>) -> Json<serde_json::Value> {
    let (active, stop) = match s.hub.control().state(&glint_ingest::ray::lock_id(&name)) {
        PauseState::Pending => (true, false),
        PauseState::Resolved(Resolution::Stop) => (false, true),
        PauseState::Resolved(Resolution::Continue { .. }) | PauseState::Unknown => (false, false),
    };
    Json(json!({ "active": active, "stop_execution": stop }))
}

async fn ingest(State(s): State<RayState>, body: Bytes) -> Response {
    let incoming = match s.adapter.decode(&body) {
        Ok(v) => v,
        Err(e) => return error(StatusCode::BAD_REQUEST, e.to_string()),
    };
    let events = s.hub.normalise(incoming);
    match s.hub.submit(events).await {
        Ok(()) => StatusCode::OK.into_response(),
        Err(e @ SubmitError::Closed) => error(StatusCode::SERVICE_UNAVAILABLE, e.to_string()),
        Err(e @ SubmitError::NotStored(_)) => error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}
