//! HTTP, WebSocket and UI server for Glint (`docs/PROTOCOL.md` §2, §7).
//!
//! Security model for a loopback dev tool: only requests whose `Host` names this loopback
//! listener are served (stops DNS rebinding), and requests carrying a foreign `Origin` are
//! refused (stops random web pages from writing or reading).

mod guard;
mod routes;

use std::future::Future;
use std::net::SocketAddr;

use axum::Router;
use glint_core::Hub;
use tokio::net::TcpListener;
use tokio::sync::watch;

pub use guard::Allowlist;

/// The UI bundled into the binary. M0 ships a single dependency-free page.
pub const INDEX_HTML: &str = include_str!("../../../ui/index.html");

/// Builds the router for a server reachable at `addr`. Live streams end when `stop`
/// turns true.
pub fn router(hub: Hub, addr: SocketAddr, stop: watch::Receiver<bool>) -> Router {
    routes::router(routes::AppState { hub, stop }).layer(axum::middleware::from_fn_with_state(
        Allowlist::for_addr(addr),
        guard::check,
    ))
}

/// Serves until `shutdown` resolves, then closes live streams so every hub clone the
/// server holds is released.
pub async fn serve(
    listener: TcpListener,
    hub: Hub,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    let addr = listener.local_addr()?;
    let (stop_tx, stop_rx) = watch::channel(false);
    axum::serve(listener, router(hub, addr, stop_rx))
        .with_graceful_shutdown(async move {
            shutdown.await;
            let _ = stop_tx.send(true);
        })
        .await
}
