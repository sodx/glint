//! `glintd`: the Glint daemon. Owns the store and serves the API and UI.
//!
//! Configuration (environment):
//! - `GLINT_ADDR` — listen address, default `127.0.0.1:23600`.
//! - `GLINT_DB`   — database file, default `~/.glint/glint.db`.
//! - `RUST_LOG`   — log filter, default `info`.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use glint_core::{Hub, HubConfig};
use glint_store_sqlite::SqliteStore;
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

const DEFAULT_ADDR: &str = "127.0.0.1:23600";

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .init();

    let addr: SocketAddr = std::env::var("GLINT_ADDR")
        .unwrap_or_else(|_| DEFAULT_ADDR.to_owned())
        .parse()
        .context("GLINT_ADDR must be host:port")?;
    if !addr.ip().is_loopback() {
        tracing::warn!(%addr, "listening beyond loopback; anyone who can reach this address can read and write events");
    }
    let db = db_path()?;
    if let Some(dir) = db.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }

    let store = SqliteStore::open(&db).with_context(|| format!("opening {}", db.display()))?;
    let (hub, writer) = Hub::start(Arc::new(store), HubConfig::default());
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding {addr}; is another glintd running?"))?;
    tracing::info!(url = %format!("http://{}", listener.local_addr()?), db = %db.display(), "glint is ready");

    glint_server::serve(listener, hub, async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await?;

    // Every accepted event is already stored (submit waits for the write), and the server
    // has released its hub, so the writer finishes promptly. The timeout only guards bugs.
    match tokio::time::timeout(std::time::Duration::from_secs(5), writer).await {
        Ok(joined) => joined.context("event writer")?,
        Err(_) => tracing::warn!("event writer did not stop within 5 s"),
    }
    tracing::info!("stopped");
    Ok(())
}

fn db_path() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("GLINT_DB") {
        return Ok(PathBuf::from(p));
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .context("cannot find home directory; set GLINT_DB")?;
    Ok(PathBuf::from(home).join(".glint").join("glint.db"))
}
