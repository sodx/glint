//! OTLP/HTTP receiver (`/v1/traces`, `/v1/logs`), protobuf or JSON. Point any OpenTelemetry
//! SDK's OTLP HTTP exporter at it: `OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:4318`.

use axum::Router;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use glint_core::Hub;
use glint_ingest::otlp;
use glint_protocol::{IncomingEnvelope, MAX_BODY_BYTES};

pub fn router(hub: Hub) -> Router {
    Router::new()
        .route("/v1/traces", post(traces))
        .route("/v1/logs", post(logs))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .with_state(hub)
}

async fn traces(State(hub): State<Hub>, headers: HeaderMap, body: Bytes) -> Response {
    receive(hub, &headers, &body, otlp::decode_traces).await
}

async fn logs(State(hub): State<Hub>, headers: HeaderMap, body: Bytes) -> Response {
    receive(hub, &headers, &body, otlp::decode_logs).await
}

type Decoder = fn(&[u8], Option<&str>) -> Result<Vec<IncomingEnvelope>, otlp::OtlpError>;

async fn receive(hub: Hub, headers: &HeaderMap, body: &[u8], decode: Decoder) -> Response {
    if headers.get(header::CONTENT_ENCODING).is_some_and(|v| v.as_bytes() != b"identity") {
        return (StatusCode::UNSUPPORTED_MEDIA_TYPE, "glint: compressed OTLP is not supported yet; set OTEL_EXPORTER_OTLP_COMPRESSION=none").into_response();
    }
    let ct = headers.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok());
    let json = ct.is_some_and(|c| c.starts_with("application/json"));
    let incoming = match decode(body, ct) {
        Ok(v) => v,
        Err(e) => return (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    };
    let events = hub.normalise(incoming);
    if let Err(e) = hub.submit(events).await {
        return (StatusCode::SERVICE_UNAVAILABLE, e.to_string()).into_response();
    }
    // An empty Export*ServiceResponse: zero bytes in protobuf, `{}` in JSON.
    if json {
        ([(header::CONTENT_TYPE, "application/json")], "{}").into_response()
    } else {
        ([(header::CONTENT_TYPE, "application/x-protobuf")], Vec::<u8>::new()).into_response()
    }
}
