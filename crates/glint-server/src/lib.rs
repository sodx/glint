//! HTTP, WebSocket and UI server for Glint (`docs/PROTOCOL.md` §2, §7).
//!
//! Security model for a loopback dev tool: only requests whose `Host` names this loopback
//! listener are served (stops DNS rebinding), and requests carrying a foreign `Origin` are
//! refused (stops random web pages from writing or reading).

mod guard;
mod otlp;
mod ray;
mod routes;
pub(crate) mod source;
mod setup;

use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use glint_core::Hub;
use tokio::net::TcpListener;
use tokio::sync::watch;

pub use guard::Allowlist;

/// The UI bundled into the binary. M0 ships a single dependency-free page.
pub const INDEX_HTML: &str = include_str!("../../../ui/index.html");

/// What a server instance offers beyond the defaults.
#[derive(Clone)]
pub struct Options {
    /// Adapters running in this daemon, reported by `/v1/info`.
    pub adapters: Vec<String>,
    /// Settings and data management (the Settings page); absent in tests and the conformance
    /// runner, where those endpoints answer 404.
    pub admin: Option<Arc<dyn Admin>>,
    /// The editor files open in when the request names none (config `editor`, `GLINT_EDITOR`).
    pub editor: Option<String>,
}

impl std::fmt::Debug for Options {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Options").field("adapters", &self.adapters).field("editor", &self.editor).finish_non_exhaustive()
    }
}

/// What the daemon lets the Settings page see and do.
pub trait Admin: Send + Sync {
    /// `{settings, from_env, restart_keys, editors, data: {db, events, bytes}, path}`.
    fn settings(&self) -> serde_json::Value;
    /// Applies changes (only the keys given), saves the file, returns `settings()`.
    fn update(&self, changes: &serde_json::Map<String, serde_json::Value>) -> Result<serde_json::Value, String>;
    /// Runs retention now; returns how many events went.
    fn prune_now(&self) -> Result<u64, String>;
    /// Deletes every event; returns how many.
    fn clear_all(&self) -> Result<u64, String>;
}

pub use source::{installed as installed_editors, set_default_editor};

impl Default for Options {
    fn default() -> Self {
        Self {
            adapters: vec!["native".to_owned(), "hooks:claude-code".to_owned()],
            admin: None,
            editor: None,
        }
    }
}

/// Builds the router for a server reachable at `addr`. Live streams end when `stop`
/// turns true.
pub fn router(hub: Hub, addr: SocketAddr, stop: watch::Receiver<bool>, opts: Options) -> Router {
    source::set_default_editor(opts.editor.clone());
    let state = routes::AppState {
        mcp: glint_mcp::McpServer::new(hub.clone()),
        hub,
        stop,
        adapters: Arc::new(opts.adapters),
        admin: opts.admin,
    };
    routes::router(state).layer(axum::middleware::from_fn_with_state(Allowlist::for_addr(addr), guard::check))
}

/// Serves the native API and UI until `shutdown` resolves, then closes live streams so
/// every hub clone the server holds is released.
pub async fn serve(
    listener: TcpListener,
    hub: Hub,
    opts: Options,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    let addr = listener.local_addr()?;
    let (stop_tx, stop_rx) = watch::channel(false);
    axum::serve(listener, router(hub, addr, stop_rx, opts))
        .with_graceful_shutdown(async move {
            shutdown.await;
            let _ = stop_tx.send(true);
        })
        .await
}

/// Serves the listener for containers and other machines (`remote_addr`) until `shutdown`
/// resolves: programs there may send events and wait for their own pause, and nothing else, so
/// reaching this address never lets anyone read events, settings or files.
pub async fn serve_remote(
    listener: TcpListener,
    hub: Hub,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    let addr = listener.local_addr()?;
    let (stop_tx, stop_rx) = watch::channel(false);
    let opts = Options { adapters: vec!["native".to_owned()], admin: None, editor: None };
    let app = router(hub, addr, stop_rx, opts).layer(axum::middleware::from_fn(guard::ingest_only));
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown.await;
            let _ = stop_tx.send(true);
        })
        .await
}

/// Serves the OTLP/HTTP receiver (`docs/mappings/otlp.md`) until `shutdown` resolves.
pub async fn serve_otlp(
    listener: TcpListener,
    hub: Hub,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    let addr = listener.local_addr()?;
    let app = otlp::router(hub).layer(axum::middleware::from_fn_with_state(Allowlist::for_addr(addr), guard::check));
    axum::serve(listener, app).with_graceful_shutdown(shutdown).await
}

/// Serves the Ray-compatible endpoints (`docs/mappings/ray.md`) until `shutdown` resolves.
pub async fn serve_ray(
    listener: TcpListener,
    hub: Hub,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    let addr = listener.local_addr()?;
    let app = ray::router(hub).layer(axum::middleware::from_fn_with_state(Allowlist::for_addr(addr), guard::check));
    axum::serve(listener, app).with_graceful_shutdown(shutdown).await
}
