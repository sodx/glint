//! `glintd`: the Glint daemon. Owns the store and serves the API and UI.
//!
//! Configuration: `~/.glint/config.toml` (or the file `GLINT_CONFIG` names), each key
//! overridable by its environment variable:
//!
//! | key | env | default |
//! |---|---|---|
//! | `addr` | `GLINT_ADDR` | `127.0.0.1:23600` |
//! | `db` | `GLINT_DB` | `~/.glint/glint.db` |
//! | `otlp_addr` | `GLINT_OTLP_ADDR` | `127.0.0.1:4318`; `off` disables; skipped if taken (a collector) |
//! | `remote_addr` | `GLINT_REMOTE_ADDR` | `off`; e.g. the Docker bridge `172.17.0.1:23600`: events in, nothing out. Retried until the address exists |
//! | `keep_days` | `GLINT_KEEP_DAYS` | `7`: older events are deleted (`0` keeps them) |
//! | `max_events` | `GLINT_MAX_EVENTS` | `100000`: the oldest beyond this are deleted (`0`: no cap) |
//! | `editor` | `GLINT_EDITOR` | the first installed of vscode, cursor, phpstorm, zed, … |
//!
//! `RUST_LOG` sets the log filter (default `info`).
//!
//! Start-up never crash-loops a login service: the port is bound before the store is opened, and
//! with `GLINT_WAIT_FOR_PORT=1` (set by the LaunchAgent and the systemd unit `glint install`
//! writes) a taken port is waited for instead of being an error, so a glintd started by hand or
//! by the app hands over to the service when it stops. A database file that is not a database or
//! is damaged is moved aside as `glint.db.corrupt-<unix seconds>` and glintd starts empty.
//! SIGTERM and Ctrl-C stop it gracefully.

use std::net::SocketAddr;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use anyhow::{Context, Result};
use glint_core::config::{KEYS, Settings};
use glint_core::{Hub, HubConfig, Redactor};
use glint_store_sqlite::SqliteStore;
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;


#[tokio::main]
async fn main() -> Result<()> {
    // `glintd --version`: the release script checks that a bundle carries the version it names.
    if std::env::args().nth(1).as_deref() == Some("--version") {
        println!("glintd {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    tracing_subscriber::fmt()
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr())) // plain text in log files
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .init();

    let (settings, from_env) = glint_core::config::load();
    let addr: SocketAddr = settings.addr.parse().context("addr (GLINT_ADDR) must be host:port")?;
    if !addr.ip().is_loopback() {
        tracing::warn!(%addr, "listening beyond loopback; anyone who can reach this address can read and write events");
    }
    let db = glint_core::config::expand(&settings.db);
    trim_log();
    // The port first: a second glintd must not touch the database the first one is using.
    let listener = bind_main(addr).await?;
    if let Some(dir) = db.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }

    let (store, aside) = SqliteStore::open_or_recover(&db).with_context(|| format!("opening {}", db.display()))?;
    if let Some(aside) = aside {
        tracing::warn!(db = %db.display(), moved_to = %aside.display(), "the database was damaged or not a database; moved it aside and started with an empty one");
    }
    let store = Arc::new(store);
    let (hub, writer) = Hub::start(store.clone(), HubConfig::default());
    hub.set_redactor(redactor(&settings));
    if !settings.redact {
        tracing::warn!("secrets redaction is off: passwords and tokens in events are stored as sent");
    }
    let admin = Arc::new(Admin { store: store.clone(), hub: hub.clone(), settings: RwLock::new(settings.clone()), from_env, db: db.display().to_string() });
    glint_server::set_default_editor(Some(settings.editor.clone()));

    // Retention: shortly after start, then every 10 minutes, with the settings of the moment.
    // The size cap is checked every 10 s, so a program writing fast cannot fill the disk between
    // two rounds.
    let pruner = admin.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(20)).await;
        let mut last_full: Option<std::time::Instant> = None;
        loop {
            let full = last_full.is_none_or(|t| t.elapsed() >= Duration::from_secs(600));
            let a = pruner.clone();
            match tokio::task::spawn_blocking(move || if full { a.prune() } else { a.prune_size() }).await {
                Ok(Ok(n)) if n > 0 => tracing::info!(removed = n, "pruned old events"),
                Ok(Err(e)) => tracing::warn!(%e, "pruning failed"),
                _ => {}
            }
            if full {
                last_full = Some(std::time::Instant::now());
            }
            tokio::time::sleep(Duration::from_secs(10)).await;
        }
    });
    let otlp_listener = optional_listener(&settings.otlp_addr, "otlp_addr (GLINT_OTLP_ADDR)", "OTLP receiver", "is an OpenTelemetry collector running?").await?;

    let mut adapters = vec!["native".to_owned()];
    if otlp_listener.is_some() {
        adapters.push("otlp".to_owned());
    }
    tracing::info!(
        url = %format!("http://{}", listener.local_addr()?),
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
        stop_signal().await;
        let _ = stop_tx.send(true);
    });

    let remote = remote_listener(&settings.remote_addr, hub.clone(), stopped())?;
    let otlp = otlp_listener.map(|l| tokio::spawn(glint_server::serve_otlp(l, hub.clone(), stopped())));
    glint_server::serve(listener, hub, glint_server::Options { adapters, admin: Some(admin), editor: Some(settings.editor.clone()) }, stopped()).await?;
    if let Some(remote) = remote {
        remote.await.context("remote listener")??;
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

/// Ctrl-C, or SIGTERM (launchd, systemd, `kill`, and `glint install` replacing this glintd).
async fn stop_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        if let Ok(mut term) = signal(SignalKind::terminate()) {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = term.recv() => {}
            }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}

/// Binds the main address. When it is taken: with `GLINT_WAIT_FOR_PORT=1` (a login service),
/// wait for it to come free, so the service takes over from a glintd the app or a terminal
/// started, without exiting and being restarted in a loop. Otherwise fail at once and say who
/// holds the port.
async fn bind_main(addr: SocketAddr) -> Result<TcpListener> {
    let wait = std::env::var("GLINT_WAIT_FOR_PORT").is_ok_and(|v| !v.is_empty() && v != "0");
    let mut logged = false;
    loop {
        match TcpListener::bind(addr).await {
            Ok(l) => {
                if logged {
                    tracing::info!(%addr, "the port is free now; taking over");
                }
                return Ok(l);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                let holder = holder(addr).await;
                if !wait {
                    anyhow::bail!("binding {addr}: the port is taken by {holder}");
                }
                if !logged {
                    tracing::warn!(%addr, "the port is taken by {holder}; waiting for it to come free");
                    logged = true;
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            Err(e) => return Err(e).with_context(|| format!("binding {addr}")),
        }
    }
}

/// Who answers on `addr`: "glintd <version> (pid N)" when it is a glintd, else "another program".
async fn holder(addr: SocketAddr) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let ask = async {
        let mut s = tokio::net::TcpStream::connect(addr).await.ok()?;
        s.write_all(format!("GET /v1/info HTTP/1.0\r\nHost: {addr}\r\n\r\n").as_bytes()).await.ok()?;
        let mut buf = Vec::new();
        s.take(64 * 1024).read_to_end(&mut buf).await.ok()?;
        let text = String::from_utf8_lossy(&buf);
        let body = text.split_once("\r\n\r\n")?.1;
        let v: serde_json::Value = serde_json::from_str(body).ok()?;
        (v["name"] == "glint").then(|| {
            let pid = v["pid"].as_u64().map(|p| format!(" (pid {p})")).unwrap_or_default();
            format!("glintd {}{pid}", v["version"].as_str().unwrap_or("?"))
        })
    };
    tokio::time::timeout(Duration::from_secs(2), ask).await.ok().flatten().unwrap_or_else(|| "another program".into())
}

/// The listener for containers and other machines. Its address may not exist yet when glintd
/// starts at login (Docker creates its bridge later), so binding is retried every 10 s.
fn remote_listener(
    value: &str,
    hub: glint_core::Hub,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> Result<Option<tokio::task::JoinHandle<std::io::Result<()>>>> {
    if value.eq_ignore_ascii_case("off") || value.is_empty() {
        return Ok(None);
    }
    let addr: SocketAddr = value.parse().context("remote_addr (GLINT_REMOTE_ADDR) must be host:port or `off`")?;
    if addr.ip().is_unspecified() {
        tracing::warn!(%addr, "remote_addr takes events from every network this machine is on; prefer the Docker bridge address");
    }
    Ok(Some(tokio::spawn(async move {
        let mut shutdown = Box::pin(shutdown);
        let mut warned = false;
        let listener = loop {
            match TcpListener::bind(addr).await {
                Ok(l) => break l,
                Err(err) => {
                    if !warned {
                        tracing::warn!(%addr, %err, "remote_addr not available yet; retrying every 10 s");
                        warned = true;
                    }
                    tokio::select! {
                        () = &mut shutdown => return Ok(()),
                        () = tokio::time::sleep(Duration::from_secs(10)) => {}
                    }
                }
            }
        };
        tracing::info!(%addr, "taking events from containers and other machines (events in, nothing out)");
        glint_server::serve_remote(listener, hub, shutdown).await
    })))
}

/// Binds an optional listener from `var` (`off` disables). A taken port is not fatal: the
/// daemon starts without that adapter and says why.
async fn optional_listener(value: &str, name: &str, what: &str, hint: &str) -> Result<Option<TcpListener>> {
    if value == "off" {
        return Ok(None);
    }
    let addr: SocketAddr = value.parse().with_context(|| format!("{name} must be host:port or `off`"))?;
    match TcpListener::bind(addr).await {
        Ok(l) => Ok(Some(l)),
        Err(err) => {
            tracing::warn!(%addr, %err, "{what} disabled: port unavailable ({hint})");
            Ok(None)
        }
    }
}


/// launchd and systemd append glintd's output to ~/.glint/glintd.log; keep it from growing
/// without end.
fn trim_log() {
    if let Some(log) = glint_core::config::home().map(|h| h.join(".glint/glintd.log"))
        && std::fs::metadata(&log).is_ok_and(|m| m.len() > 10 * 1024 * 1024)
        && let Ok(f) = std::fs::OpenOptions::new().write(true).open(&log)
    {
        let _ = f.set_len(0);
    }
}

/// The redaction rules the settings ask for. A broken extra pattern (hand-edited file or
/// environment) is dropped with a warning; the built-in rules still run.
fn redactor(s: &Settings) -> Arc<Redactor> {
    let cfg = s.redaction();
    Arc::new(Redactor::new(&cfg).unwrap_or_else(|e| {
        tracing::warn!(%e, "ignoring the extra redaction patterns");
        Redactor::new(&glint_core::RedactConfig { patterns: Vec::new(), ..cfg }).unwrap_or_default()
    }))
}

/// What the Settings page sees and changes: settings applied live (retention, editor, agent
/// switches) and saved to the config file; data stats, prune, delete all.
struct Admin {
    store: Arc<SqliteStore>,
    hub: Hub,
    settings: RwLock<Settings>,
    /// Keys an environment variable sets: shown, not editable.
    from_env: Vec<String>,
    db: String,
}

impl Admin {
    fn current(&self) -> Settings {
        self.settings.read().unwrap_or_else(|p| p.into_inner()).clone()
    }

    fn prune(&self) -> Result<u64, String> {
        let s = self.current();
        let n = self.store.prune(Some(s.keep_days), Some(s.max_events)).map_err(|e| e.to_string())?;
        Ok(n + self.prune_size()?)
    }

    /// Keeps the database under `max_db_bytes`.
    fn prune_size(&self) -> Result<u64, String> {
        self.store.prune_bytes(self.current().max_db_bytes).map_err(|e| e.to_string())
    }
}

impl glint_server::Admin for Admin {
    fn settings(&self) -> serde_json::Value {
        let (events, bytes) = self.store.stats().unwrap_or((0, 0));
        serde_json::json!({
            "settings": self.current(),
            "from_env": self.from_env,
            "restart_keys": KEYS.iter().filter(|k| k.2).map(|k| k.0).collect::<Vec<_>>(),
            "env_names": KEYS.iter().map(|k| (k.0, k.1)).collect::<std::collections::HashMap<_, _>>(),
            "editors": glint_server::installed_editors(),
            "path": glint_core::config::path().map(|p| p.display().to_string()),
            "data": { "db": self.db, "events": events, "bytes": bytes },
        })
    }

    fn update(&self, changes: &serde_json::Map<String, serde_json::Value>) -> Result<serde_json::Value, String> {
        let mut next = self.current();
        for (k, v) in changes {
            if !KEYS.iter().any(|x| x.0 == k) {
                return Err(format!("unknown setting `{k}`"));
            }
            if self.from_env.contains(k) {
                return Err(format!("`{k}` is set by an environment variable; change it there"));
            }
            let text = match v {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            next.set(k, &text);
        }
        next.validate()?;
        // The file gets the new values; keys the environment sets keep their file values.
        let mut file_view = glint_core::config::load_file();
        let json = serde_json::to_value(&next).unwrap_or_default();
        for (k, _, _) in KEYS {
            if !self.from_env.iter().any(|e| e == k) {
                let text = match json.get(*k) {
                    Some(serde_json::Value::String(s)) => s.clone(),
                    Some(other) => other.to_string(),
                    None => continue,
                };
                file_view.set(k, &text);
            }
        }
        file_view.save()?;
        glint_server::set_default_editor(Some(next.editor.clone()));
        self.hub.set_redactor(redactor(&next));
        *self.settings.write().unwrap_or_else(|p| p.into_inner()) = next;
        tracing::info!(?changes, "settings changed");
        Ok(glint_server::Admin::settings(self))
    }

    fn prune_now(&self) -> Result<u64, String> {
        self.prune()
    }

    fn clear_all(&self) -> Result<u64, String> {
        let n = self.store.clear().map_err(|e| e.to_string())?;
        tracing::info!(removed = n, "all events deleted from the Settings page");
        Ok(n)
    }
}
