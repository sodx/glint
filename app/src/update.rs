//! Updates, through the Tauri updater plugin.
//!
//! - Glint asks for `latest.json` (endpoint and public key in `tauri.conf.json`) a few seconds
//!   after launch and every six hours. A newer signed build shows as "Update to x.y.z…" in the
//!   menu-bar menu and as a banner in the window; one click downloads it, swaps the app bundle
//!   and restarts Glint.
//! - glintd and the `glint` CLI live inside the bundle, so the swap brings new ones. On the next
//!   start [`sync_tools`] copies them over the older copies `glint install` left in ~/.glint/bin
//!   and restarts a glintd that still runs an older version. This also covers an update made by
//!   dragging a new Glint.app out of a disk image.
//!
//! For testing a release locally (docs/RELEASING.md): `GLINT_UPDATE_URL` points the check at
//! another `latest.json` (plain http only on loopback; the signature is checked either way),
//! `GLINT_UPDATE_NOW=1` installs an update as soon as it is found, and `GLINT_APP_ID` gives the
//! copy its own identifier, so it runs beside the Glint you use instead of handing over to it.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tauri::menu::MenuItem;
use tauri::{AppHandle, Manager, Wry};
use tauri_plugin_notification::NotificationExt;
use tauri_plugin_updater::{Update, UpdaterExt};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

const EVERY: Duration = Duration::from_secs(6 * 60 * 60);

/// What the notification about a new version says: the menu item and the window button by the
/// names they carry.
pub(crate) fn ready_text(version: &str) -> String {
    format!("Click the spark in the menu bar and choose \"Update to {version}…\", or click \"Update and restart\" in the Glint window. Glint restarts in a few seconds.")
}

/// The menu item that reads "Check for updates…" or "Update to x.y.z…", and the update found.
pub struct Updates {
    item: MenuItem<Wry>,
    found: Mutex<Option<Update>>,
    busy: AtomicBool,
}

/// The overrides for testing a release locally.
pub fn configure(ctx: &mut tauri::Context<Wry>) {
    if let Ok(id) = std::env::var("GLINT_APP_ID") {
        ctx.config_mut().identifier = id;
    }
    let Ok(url) = std::env::var("GLINT_UPDATE_URL") else { return };
    let loopback = ["http://127.0.0.1:", "http://localhost:", "http://[::1]:"].iter().any(|p| url.starts_with(p));
    if let Some(cfg) = ctx.config_mut().plugins.0.get_mut("updater").and_then(|v| v.as_object_mut()) {
        cfg.insert("endpoints".into(), serde_json::json!([url]));
        if loopback {
            cfg.insert("dangerousInsecureTransportProtocol".into(), true.into());
        }
    }
}

pub fn start(app: &AppHandle, item: MenuItem<Wry>) {
    app.manage(Updates { item, found: Mutex::new(None), busy: AtomicBool::new(false) });
    let app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(5));
        loop {
            check(&app, false);
            std::thread::sleep(EVERY);
        }
    });
}

/// The menu item: install the update already found, else look for one now.
pub fn menu_clicked(app: &AppHandle) {
    let found = app.state::<Updates>().found.lock().ok().and_then(|f| f.clone());
    match found {
        Some(u) => install(app, u),
        None => {
            let app = app.clone();
            std::thread::spawn(move || check(&app, true));
        }
    }
}

/// The window's banner link (`glint-app://update`) was clicked.
pub fn banner_clicked(app: &AppHandle) {
    if let Some(u) = app.state::<Updates>().found.lock().ok().and_then(|f| f.clone()) {
        install(app, u);
    }
}

fn notify(app: &AppHandle, title: &str, body: &str) {
    let _ = app.notification().builder().title(title).body(body).show();
}

fn check(app: &AppHandle, asked: bool) {
    let st = app.state::<Updates>();
    if st.busy.load(Ordering::Relaxed) {
        return;
    }
    let result = tauri::async_runtime::block_on(async {
        let updater = app.updater().map_err(|e| e.to_string())?;
        updater.check().await.map_err(|e| e.to_string())
    });
    match result {
        Ok(Some(u)) => {
            let fresh = st.found.lock().ok().is_some_and(|f| f.as_ref().map(|f| &f.version) != Some(&u.version));
            let _ = st.item.set_text(format!("Update to {}…", u.version));
            if fresh || asked {
                notify(app, &format!("Glint {} is ready", u.version), &ready_text(&u.version));
            }
            if let Ok(mut f) = st.found.lock() {
                *f = Some(u.clone());
            }
            banner(app);
            if std::env::var_os("GLINT_UPDATE_NOW").is_some() {
                install(app, u);
            }
        }
        Ok(None) => {
            if asked {
                notify(app, "Glint is up to date", &format!("You have Glint {VERSION}, the newest version."));
            }
        }
        Err(e) => {
            if asked {
                notify(app, "Could not check for updates", &e);
            }
        }
    }
}

fn install(app: &AppHandle, update: Update) {
    let st = app.state::<Updates>();
    if st.busy.swap(true, Ordering::Relaxed) {
        return;
    }
    let _ = st.item.set_text(format!("Installing {}…", update.version));
    let _ = st.item.set_enabled(false);
    banner(app);
    let app = app.clone();
    std::thread::spawn(move || {
        let result = tauri::async_runtime::block_on(update.download_and_install(|_, _| {}, || {}));
        match result {
            // The bundle on disk is the new one now: start it. It refreshes glintd on launch.
            Ok(()) => app.restart(),
            Err(e) => {
                let st = app.state::<Updates>();
                st.busy.store(false, Ordering::Relaxed);
                let _ = st.item.set_text(format!("Update to {}…", update.version));
                let _ = st.item.set_enabled(true);
                banner(&app);
                notify(&app, "The update did not install", &e.to_string());
            }
        }
    });
}

/// Show, change or remove the banner in the window. Called again after each page load.
pub fn banner(app: &AppHandle) {
    let (Some(w), Some(st)) = (app.get_webview_window("main"), app.try_state::<Updates>()) else { return };
    let version = st.found.lock().ok().and_then(|f| f.as_ref().map(|u| u.version.clone()));
    let text = match version {
        Some(_) if st.busy.load(Ordering::Relaxed) => "Installing the update; Glint restarts in a moment…".to_owned(),
        Some(v) => format!("Glint {v} is ready · Update and restart"),
        None => String::new(),
    };
    let _ = w.eval(format!("{BANNER_JS}({})", serde_json::to_string(&text).unwrap_or_default()));
}

const BANNER_JS: &str = r"(function(text){
  var b = document.getElementById('glint-update');
  if (!text) { if (b) b.remove(); return; }
  if (!b) {
    b = document.createElement('a');
    b.id = 'glint-update';
    b.href = 'glint-app://update';
    b.style.cssText = 'position:fixed;right:12px;bottom:12px;z-index:2147483647;padding:7px 12px;border-radius:8px;' +
      'font:500 12.5px/1.3 var(--sans,system-ui);text-decoration:none;color:var(--text,#222);' +
      'background:var(--accent-soft,#eef7d6);border:1px solid var(--line-strong,#ccc);box-shadow:var(--shadow,none)';
    document.body.appendChild(b);
  }
  b.textContent = text;
})";

// ------------------------------------------------------------------ glintd and the CLI after an update

/// Versions as numbers, so 0.10.0 sorts after 0.9.0. Anything after a `-` or `+` is ignored.
fn parse(v: &str) -> Option<Vec<u64>> {
    let core = v.trim().trim_start_matches('v').split(['-', '+']).next()?;
    core.split('.').map(|p| p.parse().ok()).collect()
}

pub fn older(have: &str, than: &str) -> bool {
    matches!((parse(have), parse(than)), (Some(a), Some(b)) if a < b)
}

/// The Glint.app this binary runs from.
pub(crate) fn own_bundle() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    exe.ancestors().find(|a| a.extension().is_some_and(|x| x == "app")).map(Path::to_path_buf)
}

/// Copy through a new file and a rename, as `glint install` does: overwriting a binary in place
/// keeps its inode and macOS then kills the next launch (stale signature cache), and the rename
/// leaves a running copy untouched.
fn replace_file(from: &Path, to: &Path) -> std::io::Result<()> {
    let tmp = to.with_extension("new");
    std::fs::copy(from, &tmp)?;
    let _ = Command::new("xattr").args(["-d", "com.apple.quarantine"]).arg(&tmp).stdout(Stdio::null()).stderr(Stdio::null()).status();
    std::fs::rename(&tmp, to)
}

/// After an update: bring ~/.glint/bin up to this version, and restart a glintd older than us.
/// Never downgrades: a newer glintd or CLI (a developer's own build) is left alone.
pub fn sync_tools() {
    let Some(bundle) = own_bundle() else { return };
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let bin = home.join(".glint/bin");
    let new_daemon = bundle.join("Contents/MacOS/glintd");
    let new_cli = bundle.join("Contents/Resources/bin/glint");

    // `glint install` copies the pair together; the CLI tells us which version that was.
    let installed = Command::new(bin.join("glint")).arg("--version").stdin(Stdio::null()).output().ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().rsplit(' ').next().unwrap_or("").to_owned());
    if let Some(have) = installed.filter(|h| older(h, VERSION)) {
        for (from, to) in [(&new_daemon, bin.join("glintd")), (&new_cli, bin.join("glint"))] {
            if from.is_file() && to.exists() {
                let _ = replace_file(from, &to);
            }
        }
        eprintln!("glint: updated ~/.glint/bin from {have} to {VERSION}");
    }

    let running = crate::get("/v1/info").and_then(|i| i["version"].as_str().map(str::to_owned));
    if let Some(have) = running.filter(|h| older(h, VERSION)) {
        eprintln!("glint: glintd {have} is older than this app ({VERSION}); restarting it");
        restart_daemon(&home);
    }
}

fn restart_daemon(home: &Path) {
    // A login service: launchd restarts it from ~/.glint/bin, which now holds the new glintd.
    if home.join(format!("Library/LaunchAgents/{}.plist", crate::SERVICE_LABEL)).exists() {
        let uid = Command::new("id").arg("-u").output().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned()).unwrap_or_default();
        let _ = Command::new("launchctl").args(["kickstart", "-k", &format!("gui/{uid}/{}", crate::SERVICE_LABEL)]).status();
        crate::wait_up(50);
        return;
    }
    // Started by Glint earlier: stop it, and ensure_daemon starts the newest copy. Only a glintd
    // from an app bundle or ~/.glint/bin, never one a developer runs from a build folder.
    let port = crate::base_url().rsplit(':').next().unwrap_or("").to_owned();
    let pids = Command::new("lsof").args(["-nP", &format!("-iTCP:{port}"), "-sTCP:LISTEN", "-t"]).output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
    for pid in pids.split_whitespace() {
        let path = Command::new("ps").args(["-o", "comm=", "-p", pid]).output().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned()).unwrap_or_default();
        if path.ends_with("Contents/MacOS/glintd") || path.ends_with(".glint/bin/glintd") {
            let _ = Command::new("kill").arg(pid).status();
        }
    }
    for _ in 0..30 {
        if !crate::daemon_up() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(test)]
mod tests {
    use super::older;

    #[test]
    fn versions_compare_as_numbers() {
        assert!(older("0.1.0", "0.1.1"));
        assert!(older("0.9.0", "0.10.0"));
        assert!(!older("0.1.1-beta", "0.1.1"));
        assert!(!older("0.2.0", "0.1.9"));
        assert!(!older("0.1.0", "0.1.0"));
        assert!(!older("garbage", "0.1.0"));
    }
}
