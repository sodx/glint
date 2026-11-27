//! `glintd`: the Glint daemon. Owns the store and serves the API and UI.
//!
//! Configuration: `~/.glint/config.toml` (or the file `GLINT_CONFIG` names), each key
//! overridable by its environment variable:
//!
//! | key | env | default |
//! |---|---|---|
//! | `addr` | `GLINT_ADDR` | `127.0.0.1:23600` |
//! | `db` | `GLINT_DB` | `~/.glint/glint.db` |
//! | `ray_addr` | `GLINT_RAY_ADDR` | `127.0.0.1:23517`; `off` disables; skipped if the port is taken (the Ray app) |
//! | `otlp_addr` | `GLINT_OTLP_ADDR` | `127.0.0.1:4318`; `off` disables; skipped if taken (a collector) |
//! | `keep_days` | `GLINT_KEEP_DAYS` | `7`: older events are deleted (`0` keeps them) |
//! | `max_events` | `GLINT_MAX_EVENTS` | `100000`: the oldest beyond this are deleted (`0`: no cap) |
//! | `editor` | `GLINT_EDITOR` | the first installed of vscode, cursor, phpstorm, zed, … |
//!
//! `RUST_LOG` sets the log filter (default `info`).

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use glint_core::{Hub, HubConfig};
use glint_store_sqlite::SqliteStore;
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

const DEFAULT_ADDR: &str = "127.0.0.1:23600";
const DEFAULT_RAY_ADDR: &str = "127.0.0.1:23517";
const DEFAULT_OTLP_ADDR: &str = "127.0.0.1:4318";

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr())) // plain text in log files
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .init();

    let cfg = Config::load();
    let addr: SocketAddr = cfg
        .get("GLINT_ADDR", "addr")
        .unwrap_or_else(|| DEFAULT_ADDR.to_owned())
        .parse()
        .context("addr (GLINT_ADDR) must be host:port")?;
    if !addr.ip().is_loopback() {
        tracing::warn!(%addr, "listening beyond loopback; anyone who can reach this address can read and write events");
    }
    let db = match cfg.get("GLINT_DB", "db") {
        Some(p) => match (p.strip_prefix("~/"), home()) {
            (Some(rest), Some(h)) => h.join(rest),
            _ => PathBuf::from(p),
        },
        None => db_path()?,
    };
    let keep_days: u32 = cfg.get("GLINT_KEEP_DAYS", "keep_days").and_then(|v| v.parse().ok()).unwrap_or(7);
    let max_events: u64 = cfg.get("GLINT_MAX_EVENTS", "max_events").and_then(|v| v.parse().ok()).unwrap_or(100_000);
    trim_log();
    if let Some(dir) = db.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }

    let store = Arc::new(SqliteStore::open(&db).with_context(|| format!("opening {}", db.display()))?);
    let (hub, writer) = Hub::start(store.clone(), HubConfig::default());

    // Retention: shortly after start, then every 10 minutes.
    let pruner = store.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(20)).await;
        loop {
            let s = pruner.clone();
            match tokio::task::spawn_blocking(move || s.prune(Some(keep_days), Some(max_events))).await {
                Ok(Ok(n)) if n > 0 => tracing::info!(removed = n, keep_days, max_events, "pruned old events"),
                Ok(Err(e)) => tracing::warn!(%e, "pruning failed"),
                _ => {}
            }
            tokio::time::sleep(Duration::from_secs(600)).await;
        }
    });
    let stats_store = store.clone();
    let db_shown = db.display().to_string();
    let data: glint_server::DataInfo = Arc::new(move || {
        let (events, bytes) = stats_store.stats().unwrap_or((0, 0));
        serde_json::json!({ "db": db_shown, "events": events, "bytes": bytes, "keep_days": keep_days, "max_events": max_events })
    });
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding {addr}; is another glintd running?"))?;

    let ray_listener = optional_listener(&cfg, "GLINT_RAY_ADDR", "ray_addr", DEFAULT_RAY_ADDR, "Ray-compatible ingest", "is the Ray app running?").await?;
    let otlp_listener = optional_listener(&cfg, "GLINT_OTLP_ADDR", "otlp_addr", DEFAULT_OTLP_ADDR, "OTLP receiver", "is an OpenTelemetry collector running?").await?;

    let mut adapters = vec!["native".to_owned()];
    if ray_listener.is_some() {
        adapters.push("ray".to_owned());
    }
    if otlp_listener.is_some() {
        adapters.push("otlp".to_owned());
    }
    tracing::info!(
        url = %format!("http://{}", listener.local_addr()?),
        ray = %ray_listener.as_ref().and_then(|l| l.local_addr().ok()).map_or("off".to_owned(), |a| a.to_string()),
        otlp = %otlp_listener.as_ref().and_then(|l| l.local_addr().ok()).map_or("off".to_owned(), |a| a.to_string()),
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
    let otlp = otlp_listener.map(|l| tokio::spawn(glint_server::serve_otlp(l, hub.clone(), stopped())));
    glint_server::serve(listener, hub, glint_server::Options { adapters, data: Some(data), editor: cfg.get("GLINT_EDITOR", "editor") }, stopped()).await?;
    if let Some(ray) = ray {
        ray.await.context("ray listener")??;
    }
    if let Some(otlp) = otlp {
        otlp.await.context("otlp listener")??;
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

/// Binds an optional listener from `var` (`off` disables). A taken port is not fatal: the
/// daemon starts without that adapter and says why.
async fn optional_listener(cfg: &Config, var: &str, key: &str, default: &str, what: &str, hint: &str) -> Result<Option<TcpListener>> {
    let value = cfg.get(var, key).unwrap_or_else(|| default.to_owned());
    if value == "off" {
        return Ok(None);
    }
    let addr: SocketAddr = value.parse().with_context(|| format!("{var} must be host:port or `off`"))?;
    match TcpListener::bind(addr).await {
        Ok(l) => Ok(Some(l)),
        Err(err) => {
            tracing::warn!(%addr, %err, "{what} disabled: port unavailable ({hint})");
            Ok(None)
        }
    }
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

/// `~/.glint/config.toml`: plain `key = value` lines (a TOML subset: numbers, "strings", bare
/// words; `#` comments). Environment variables win over it.
struct Config(HashMap<String, String>);

impl Config {
    fn load() -> Self {
        let path = std::env::var_os("GLINT_CONFIG").map(PathBuf::from).or_else(|| home().map(|h| h.join(".glint/config.toml")));
        let text = path.and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_default();
        Self(parse_config(&text))
    }

    fn get(&self, env: &str, key: &str) -> Option<String> {
        std::env::var(env).ok().filter(|v| !v.is_empty()).or_else(|| self.0.get(key).cloned())
    }
}

fn parse_config(text: &str) -> HashMap<String, String> {
    text.lines()
        .filter_map(|l| {
            let l = l.split_once('#').map_or(l, |(a, _)| if a.matches('"').count() % 2 == 0 { a } else { l }).trim();
            let (k, v) = l.split_once('=')?;
            let v = v.trim();
            let v = v.strip_prefix('"').and_then(|x| x.strip_suffix('"')).unwrap_or(v);
            Some((k.trim().to_owned(), v.to_owned())).filter(|(k, _)| !k.is_empty() && !k.starts_with('['))
        })
        .collect()
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from)
}

/// launchd and systemd append glintd's output to ~/.glint/glintd.log; keep it from growing
/// without end.
fn trim_log() {
    if let Some(log) = home().map(|h| h.join(".glint/glintd.log"))
        && std::fs::metadata(&log).is_ok_and(|m| m.len() > 10 * 1024 * 1024)
        && let Ok(f) = std::fs::OpenOptions::new().write(true).open(&log)
    {
        let _ = f.set_len(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_lines() {
        let c = parse_config("# Glint\nkeep_days = 14\neditor = \"cursor\" # mine\naddr=127.0.0.1:23700\n[section]\n\nbad line\n");
        assert_eq!(c.get("keep_days").map(String::as_str), Some("14"));
        assert_eq!(c.get("editor").map(String::as_str), Some("cursor"));
        assert_eq!(c.get("addr").map(String::as_str), Some("127.0.0.1:23700"));
        assert_eq!(c.len(), 3);
    }
}
