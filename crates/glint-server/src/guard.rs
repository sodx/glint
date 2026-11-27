use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

/// Hosts and origins this server answers to.
#[derive(Clone, Debug)]
pub struct Allowlist {
    hosts: Arc<Vec<String>>,
    origins: Arc<Vec<String>>,
}

impl Allowlist {
    pub fn for_addr(addr: SocketAddr) -> Self {
        let port = addr.port();
        // host.docker.internal is how containers reach the host (Docker Desktop, or
        // `host-gateway` on Linux). A web page cannot make a browser send it as Host.
        let mut hosts = vec![
            format!("127.0.0.1:{port}"),
            format!("localhost:{port}"),
            format!("[::1]:{port}"),
            format!("host.docker.internal:{port}"),
        ];
        if !addr.ip().is_loopback() && !addr.ip().is_unspecified() {
            hosts.push(addr.to_string());
        }
        let mut origins: Vec<String> = hosts
            .iter()
            .filter(|h| !h.starts_with("host.docker.internal"))
            .map(|h| format!("http://{h}"))
            .collect();
        // The Tauri shell (M4) serves the UI from these origins.
        origins.extend(["tauri://localhost".to_owned(), "http://tauri.localhost".to_owned()]);
        Self {
            hosts: Arc::new(hosts),
            origins: Arc::new(origins),
        }
    }

    fn host_ok(&self, host: &str) -> bool {
        self.hosts.iter().any(|h| h.eq_ignore_ascii_case(host))
    }

    fn origin_ok(&self, origin: &str) -> bool {
        self.origins.iter().any(|o| o.eq_ignore_ascii_case(origin))
    }
}

/// A page served by a local development server: `http(s)://localhost`, `127.0.0.1`, `[::1]`,
/// `*.localhost` or `*.test`, any port. Only such pages may send events from a browser (the
/// browser script, `/v1/browser.js`); a page on the internet can never have such an origin.
pub fn dev_origin(origin: &str) -> bool {
    let Some(rest) = origin.strip_prefix("http://").or_else(|| origin.strip_prefix("https://")) else { return false };
    let host = if let Some(v6) = rest.strip_prefix('[') { v6.split(']').next().map(|h| format!("[{h}]")).unwrap_or_default() } else { rest.split(':').next().unwrap_or("").to_ascii_lowercase() };
    matches!(host.as_str(), "localhost" | "127.0.0.1" | "[::1]") || host.ends_with(".localhost") || host.ends_with(".test")
}

/// Whether the listener for containers and other machines serves this request: sending events,
/// waiting for the program's own pause (its id is known only to it), and the browser script.
pub fn ingest_allowed(method: &axum::http::Method, path: &str) -> bool {
    use axum::http::Method;
    let own_pause = path.strip_prefix("/v1/ctl/").is_some_and(|id| !id.is_empty() && !id.contains('/'));
    match *method {
        Method::POST => path == "/v1/events",
        Method::GET => own_pause || path == "/v1/browser.js",
        _ => false,
    }
}

pub async fn ingest_only(req: Request, next: Next) -> Response {
    if ingest_allowed(req.method(), req.uri().path()) {
        next.run(req).await
    } else {
        (StatusCode::FORBIDDEN, "glint: this address only takes events (remote_addr); read them on 127.0.0.1").into_response()
    }
}

pub async fn check(State(allow): State<Allowlist>, req: Request, next: Next) -> Response {
    let host = req.headers().get(header::HOST).and_then(|v| v.to_str().ok());
    if !host.is_some_and(|h| allow.host_ok(h)) {
        return (StatusCode::FORBIDDEN, "glint: unexpected Host header").into_response();
    }
    // The browser SDK is a public static file: any page may load it, also in CORS mode, which
    // cross-origin isolated pages (WordPress's block editor) need.
    if req.uri().path() == "/v1/browser.js" {
        let mut r = next.run(req).await;
        r.headers_mut().insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, header::HeaderValue::from_static("*"));
        return r;
    }
    let Some(origin) = req.headers().get(header::ORIGIN).and_then(|o| o.to_str().ok()).map(str::to_owned) else {
        return next.run(req).await;
    };
    if allow.origin_ok(&origin) {
        return next.run(req).await;
    }
    // Pages of local dev servers may send events and wait for an answer to their own pause
    // (`GET /v1/ctl/{id}`, whose id only the page knows), and nothing else.
    let path = req.uri().path();
    let events = path == "/v1/events";
    let own_pause = req.method() == axum::http::Method::GET && path.starts_with("/v1/ctl/") && !path.ends_with("/resume") && path.len() > "/v1/ctl/".len();
    if own_pause && dev_origin(&origin) {
        let mut r = next.run(req).await;
        if let Ok(v) = header::HeaderValue::from_str(&origin) {
            r.headers_mut().insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, v);
        }
        return r;
    }
    if events && dev_origin(&origin) {
        let cors = |mut r: Response| {
            let h = r.headers_mut();
            if let Ok(v) = header::HeaderValue::from_str(&origin) {
                h.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, v);
            }
            h.insert(header::VARY, header::HeaderValue::from_static("Origin"));
            r
        };
        if req.method() == axum::http::Method::OPTIONS {
            let mut r = StatusCode::NO_CONTENT.into_response();
            let h = r.headers_mut();
            h.insert(header::ACCESS_CONTROL_ALLOW_METHODS, header::HeaderValue::from_static("POST"));
            h.insert(header::ACCESS_CONTROL_ALLOW_HEADERS, header::HeaderValue::from_static("content-type"));
            h.insert(header::ACCESS_CONTROL_MAX_AGE, header::HeaderValue::from_static("600"));
            return cors(r);
        }
        if req.method() == axum::http::Method::POST {
            return cors(next.run(req).await);
        }
    }
    (StatusCode::FORBIDDEN, "glint: origin not allowed").into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_hosts_and_origins() {
        let a = Allowlist::for_addr("127.0.0.1:23600".parse().unwrap());
        assert!(a.host_ok("127.0.0.1:23600") && a.host_ok("localhost:23600") && a.host_ok("LOCALHOST:23600"));
        assert!(!a.host_ok("evil.example:23600") && !a.host_ok("127.0.0.1:80"));
        assert!(a.origin_ok("http://localhost:23600") && a.origin_ok("tauri://localhost"));
        assert!(!a.origin_ok("https://evil.example") && !a.origin_ok("null"));
    }

    #[test]
    fn remote_listener_takes_events_only() {
        use axum::http::Method;
        assert!(ingest_allowed(&Method::POST, "/v1/events"));
        assert!(ingest_allowed(&Method::GET, "/v1/ctl/01ABC"));
        assert!(ingest_allowed(&Method::GET, "/v1/browser.js"));
        for (m, p) in [
            (Method::GET, "/v1/events"), (Method::GET, "/"), (Method::GET, "/v1/info"), (Method::GET, "/v1/ctl"),
            (Method::POST, "/v1/ctl/01ABC/resume"), (Method::GET, "/v1/ctl/"), (Method::POST, "/mcp"),
            (Method::GET, "/v1/settings"), (Method::POST, "/v1/open"), (Method::GET, "/v1/source"),
        ] {
            assert!(!ingest_allowed(&m, p), "{m} {p}");
        }
        let a = Allowlist::for_addr("172.17.0.1:23600".parse().unwrap());
        assert!(a.host_ok("host.docker.internal:23600") && a.host_ok("172.17.0.1:23600"));
    }

    #[test]
    fn dev_origins() {
        for ok in ["http://localhost:5173", "http://127.0.0.1:3000", "https://shop.test", "http://app.localhost:8080", "http://[::1]:5173"] {
            assert!(dev_origin(ok), "{ok}");
        }
        for no in ["https://evil.com", "http://localhost.evil.com", "http://127.0.0.1.nip.io", "null", "file://", "https://test"] {
            assert!(!dev_origin(no), "{no}");
        }
    }
}
