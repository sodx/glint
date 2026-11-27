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
        let mut hosts = vec![format!("127.0.0.1:{port}"), format!("localhost:{port}"), format!("[::1]:{port}")];
        if !addr.ip().is_loopback() && !addr.ip().is_unspecified() {
            hosts.push(addr.to_string());
        }
        let mut origins: Vec<String> = hosts.iter().map(|h| format!("http://{h}")).collect();
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

pub async fn check(State(allow): State<Allowlist>, req: Request, next: Next) -> Response {
    let host = req.headers().get(header::HOST).and_then(|v| v.to_str().ok());
    if !host.is_some_and(|h| allow.host_ok(h)) {
        return (StatusCode::FORBIDDEN, "glint: unexpected Host header").into_response();
    }
    if let Some(origin) = req.headers().get(header::ORIGIN) {
        let ok = origin.to_str().is_ok_and(|o| allow.origin_ok(o));
        if !ok {
            return (StatusCode::FORBIDDEN, "glint: origin not allowed").into_response();
        }
    }
    next.run(req).await
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
}
