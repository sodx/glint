//! `glintd`: the Glint daemon. Owns the store and serves the API and UI.
//!
//! Configuration (environment):
//! - `GLINT_ADDR` — listen address, default `127.0.0.1:23600`.
//! - `GLINT_DB`   — database file, default `~/.glint/glint.db`.
//! - `GLINT_RAY_ADDR` — Ray-compatible listener, default `127.0.0.1:23517`; `off` disables it.
//!   If the port is taken (the Ray app is running), Glint starts without it.
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
const DEFAULT_RAY_ADDR: &str = "127.0.0.1:23517";

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

    let ray_listener = match std::env::var("GLINT_RAY_ADDR").as_deref() {
        Ok("off") => None,
        other => {
            let ray_addr: SocketAddr = other
                .unwrap_or(DEFAULT_RAY_ADDR)
                .parse()
                .context("GLINT_RAY_ADDR must be host:port or `off`")?;
            match TcpListener::bind(ray_addr).await {
                Ok(l) => Some(l),
                Err(err) => {
                    tracing::warn!(%ray_addr, %err, "Ray-compatible ingest disabled: port unavailable (is the Ray app running?)");
                    None
                }
            }
        }
    };

    let mut adapters = vec!["native".to_owned()];
    if ray_listener.is_some() {
        adapters.push("ray".to_owned());
    }
    tracing::info!(
        url = %format!("http://{}", listener.local_addr()?),
        ray = %ray_listener.as_ref().and_then(|l| l.local_addr().ok()).map_or("off".to_owned(), |a| a.to_string()),
        db = %db.display(),
        "glint is ready"
    );

    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let stopped = move || {
        let mut rx = stop_rx.clone();
        async move {
            let _ = rx.wait_for(|s| *s).await;
        }
    };
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        let _ = stop_tx.send(true);
    });

    let ray = ray_listener.map(|l| tokio::spawn(glint_server::serve_ray(l, hub.clone(), stopped())));
    glint_server::serve(listener, hub, glint_server::Options { adapters }, stopped()).await?;
    if let Some(ray) = ray {
        ray.await.context("ray listener")??;
    }

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
