//! Glint for the desktop.
//!
//! - a window with the live UI served by glintd (the same `ui/index.html` the browser gets);
//! - a menu-bar spark that turns red when something fails, amber when a program waits for you
//!   and struck through when glintd is not running. Clicking it opens its menu: Open Glint, a
//!   status line that names the newest failure, Open in browser, Set up this Mac…, Open at login,
//!   Check for updates…, Save diagnostics report…, Glint help, Uninstall Glint… and Quit;
//! - a notification for each new failure and each new pause (not for the sample run of "Show me
//!   an example"); a failure nobody looked at stays red across a restart (see `state`);
//! - the Dock icon carries the same state as a badge (✗ or ⏸), because on a MacBook with a notch
//!   the menu-bar spark can be hidden behind it;
//! - ⌥⌘G shows and hides the window from anywhere;
//! - glintd is started if nothing answers (the copy inside the app bundle, else ~/.glint/bin);
//! - updates itself from signed releases, glintd and the CLI included (see `update`).
//!
//! Closing the window keeps Glint in the menu bar; Quit (menu or ⌘Q) ends it. When `glint install`
//! set glintd up as a login service, glintd keeps running after Quit, so events are never lost;
//! without one, Quit also stops the glintd this app started. Setup makes Glint.app open at login
//! (`glint install`'s login step; started that way it gets `--login` and keeps its window
//! closed). `glint uninstall` leaves ~/.glint/uninstalled, and a running Glint quits instead of
//! starting glintd again.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod state;
mod update;

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::process::Child;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use tauri::image::Image;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::TrayIconBuilder;
use tauri::webview::PageLoadEvent;
use tauri::{AppHandle, Manager, RunEvent, WebviewUrl, WebviewWindowBuilder, WindowEvent, Wry};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};
use tauri_plugin_notification::NotificationExt;

const TRAY_OK: &[u8] = include_bytes!("../icons/tray.png");
const TRAY_FAIL: &[u8] = include_bytes!("../icons/tray-fail.png");
const TRAY_WAIT: &[u8] = include_bytes!("../icons/tray-wait.png");
const TRAY_OFFLINE: &[u8] = include_bytes!("../icons/tray-offline.png");

fn base_url() -> String {
    std::env::var("GLINT_URL").unwrap_or_else(|_| "http://127.0.0.1:23600".into()).trim_end_matches('/').to_owned()
}

fn get(path: &str) -> Option<Value> {
    ureq::get(&format!("{}{path}", base_url())).set("X-Glint", "1").timeout(Duration::from_millis(1500)).call().ok()?.into_json().ok()
}

// ------------------------------------------------------------------ glintd

fn daemon_up() -> bool {
    get("/v1/info").is_some()
}

const SERVICE_LABEL: &str = "local.glint.glintd";

fn wait_up(tenths: u32) -> bool {
    for _ in 0..tenths {
        if daemon_up() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

/// Start glintd when nothing answers, and say why when it still does not. When `glint install`
/// set it up as a login service, ask the service manager first. When the service does not come
/// up (a broken copy in ~/.glint/bin, a service launchd gave up on), run the newest of
/// ~/.glint/bin/glintd and the copy bundled next to this binary, logging to ~/.glint/glintd.log;
/// the service's glintd waits for the port (GLINT_WAIT_FOR_PORT) and takes over when this one
/// stops, so the two never fight.
fn ensure_daemon() -> Result<(), String> {
    if daemon_up() {
        return Ok(());
    }
    if uninstalled() {
        return Err("Glint was uninstalled".into());
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    if has_service() {
        let uid = Command::new("id").arg("-u").output().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned()).unwrap_or_default();
        let _ = Command::new("launchctl").args(["kickstart", &format!("gui/{uid}/{SERVICE_LABEL}")]).status();
        if wait_up(50) {
            return Ok(());
        }
        eprintln!("glint: the login service did not start glintd; starting the bundled one");
    }
    if cfg!(target_os = "linux") && home.join(".config/systemd/user/glintd.service").exists() {
        let _ = Command::new("systemctl").args(["--user", "start", "glintd.service"]).status();
        if wait_up(50) {
            return Ok(());
        }
    }
    let mut candidates = vec![home.join(".glint/bin/glintd")];
    if let Ok(exe) = std::env::current_exe() {
        candidates.push(exe.with_file_name("glintd"));
    }
    candidates.retain(|p| p.exists());
    candidates.sort_by_key(|p| std::cmp::Reverse(std::fs::metadata(p).and_then(|m| m.modified()).ok()));
    let why = || {
        let log = std::fs::read_to_string(home.join(".glint/glintd.log")).unwrap_or_default();
        log.lines().rev().find(|l| !l.trim().is_empty()).map(|l| l.chars().take(200).collect::<String>()).unwrap_or_else(|| "no log yet".into())
    };
    let Some(bin) = candidates.into_iter().next() else { return Err(logged("no glintd found in the app or ~/.glint/bin".into())) };
    let log = std::fs::create_dir_all(home.join(".glint"))
        .and_then(|()| std::fs::OpenOptions::new().create(true).append(true).open(home.join(".glint/glintd.log")));
    let mut cmd = Command::new(bin);
    cmd.stdin(Stdio::null());
    if let Ok(f) = log
        && let Ok(f2) = f.try_clone()
    {
        cmd.stdout(f).stderr(f2);
    }
    match cmd.spawn() {
        Ok(child) => {
            if let Ok(mut own) = OWN_DAEMON.lock() {
                if let Some(mut old) = own.take() {
                    let _ = old.try_wait();
                }
                *own = Some(child);
            }
            if wait_up(30) { Ok(()) } else { Err(why()) }
        }
        Err(e) => Err(logged(format!("could not start {}: {e}", cmd.get_program().to_string_lossy()))),
    }
}

/// The glintd this app started, if any: stopped on Quit when there is no login service.
static OWN_DAEMON: Mutex<Option<Child>> = Mutex::new(None);

/// Set while "Uninstall Glint…" runs, which stops glintd on purpose.
static UNINSTALLING: AtomicBool = AtomicBool::new(false);

/// Did `glint install` set glintd up to start at login?
fn has_service() -> bool {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    if cfg!(target_os = "macos") {
        home.join(format!("Library/LaunchAgents/{SERVICE_LABEL}.plist")).exists()
    } else {
        cfg!(target_os = "linux") && home.join(".config/systemd/user/glintd.service").exists()
    }
}

/// `glint uninstall` ran since this app started (see `glint_cli::install::UNINSTALLED`).
fn uninstalled() -> bool {
    state::glint_dir().join("uninstalled").exists()
}

/// Why glintd is not running goes to glintd.log, which the window's diagnostics report includes;
/// the menu bar only says "Not recording".
fn logged(why: String) -> String {
    use std::io::Write as _;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(state::glint_dir().join("glintd.log")) {
        let _ = writeln!(f, "glint app: {why}");
    }
    why
}

/// Stops the glintd this app started, unless a login service keeps recording (then the service's
/// glintd, which waits for the port, takes over). `always`: stop it anyway (uninstall).
fn stop_own_daemon(always: bool) {
    if !always && has_service() {
        return;
    }
    let Some(mut child) = OWN_DAEMON.lock().ok().and_then(|mut c| c.take()) else { return };
    if !matches!(child.try_wait(), Ok(None)) {
        return; // it ended already (setup swapped it for the service)
    }
    // SIGTERM first: glintd finishes its writes and closes the database.
    let _ = Command::new("kill").args(["-TERM", &child.id().to_string()]).status();
    for _ in 0..30 {
        if !matches!(child.try_wait(), Ok(None)) {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = child.kill();
    let _ = child.wait();
}

// ------------------------------------------------------------------ window

fn show(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}

/// Is the window open and in front, where the user sees it?
fn in_front(app: &AppHandle) -> bool {
    app.get_webview_window("main").is_some_and(|w| w.is_visible().unwrap_or(false) && w.is_focused().unwrap_or(false))
}

fn toggle(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        if w.is_visible().unwrap_or(false) && w.is_focused().unwrap_or(false) {
            let _ = w.hide();
        } else {
            show(app);
        }
    }
}

// ------------------------------------------------------------------ the menu's actions

/// The `glint` CLI inside this app bundle (Contents/Resources/bin/glint).
fn bundled_cli() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let p = exe.parent()?.parent()?.join("Resources/bin/glint");
    p.is_file().then_some(p)
}

/// The CLI to run from the menu. macOS kills a quarantined command line tool as soon as it starts,
/// so the bundled one runs from a fresh copy in ~/.glint/cache (as glintd's setup does, same
/// file); without a bundle, the one `glint install` put in ~/.glint/bin.
fn menu_cli() -> Option<PathBuf> {
    let installed = state::glint_dir().join("bin/glint");
    let Some(bundled) = bundled_cli() else { return installed.is_file().then_some(installed) };
    let copy = state::glint_dir().join("cache/glint");
    let bytes = std::fs::read(&bundled).ok()?;
    if std::fs::read(&copy).is_ok_and(|b| b == bytes) {
        return Some(copy);
    }
    let made = std::fs::create_dir_all(state::glint_dir().join("cache"))
        .and_then(|()| {
            // Remove first: rewriting a file in place can keep macOS's cached signature of the old one.
            let _ = std::fs::remove_file(&copy);
            std::fs::write(&copy, &bytes)
        })
        .and_then(|()| {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&copy, std::fs::Permissions::from_mode(0o755))
        });
    Some(if made.is_ok() { copy } else { bundled })
}

/// The user's login shell (an app started from Finder has no $SHELL to go by).
fn login_shell() -> String {
    let user = std::env::var("USER").unwrap_or_default();
    Command::new("dscl")
        .args([".", "-read", &format!("/Users/{user}"), "UserShell"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8_lossy(&o.stdout).split_whitespace().nth(1).map(str::to_owned))
        .filter(|s| std::path::Path::new(s).is_file())
        .unwrap_or_else(|| "/bin/zsh".into())
}

/// Runs `glint <args>`; in the login shell when `shell`, so the user's PATH decides which php and
/// python3 it finds, as in their terminal. Gives everything it printed, or why it could not run.
fn run_cli(args: &str, shell: bool, timeout: Duration) -> Result<String, String> {
    let cli = menu_cli().ok_or("this copy of Glint has no command line tool inside; reinstall the app")?;
    let quoted = format!("'{}' {args} 2>&1", cli.display().to_string().replace('\'', "'\\''"));
    let mut cmd = if shell {
        let sh = login_shell();
        let mut c = Command::new(&sh);
        c.args(["-lc", &quoted]).env("SHELL", &sh);
        c
    } else {
        let mut c = Command::new("/bin/sh");
        c.args(["-c", &quoted]);
        c
    };
    if let Some(b) = update::own_bundle() {
        cmd.env("GLINT_APP", b);
    }
    let out_file = state::glint_dir().join(format!("cache/menu-{}.out", std::process::id()));
    let _ = std::fs::create_dir_all(state::glint_dir().join("cache"));
    let out = std::fs::File::create(&out_file).map_err(|e| e.to_string())?;
    let mut child = cmd.stdin(Stdio::null()).stdout(out).stderr(Stdio::null()).spawn().map_err(|e| format!("could not run glint: {e}"))?;
    let started = std::time::Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) if started.elapsed() < timeout => std::thread::sleep(Duration::from_millis(200)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let text = std::fs::read_to_string(&out_file).unwrap_or_default();
    let _ = std::fs::remove_file(&out_file);
    match status {
        None => Err(format!("glint {args} did not finish within {} s", timeout.as_secs())),
        Some(s) if s.code() == Some(137) || text.trim().is_empty() && !s.success() => Err(format!("glint {args} stopped without a word ({s})")),
        Some(_) => Ok(text),
    }
}

/// Lines the CLI marks as failed (`  ✗  name  why`).
fn failed_steps(text: &str) -> usize {
    text.lines().filter(|l| l.trim_start().starts_with('✗')).count()
}

fn steps_word(n: usize) -> String {
    if n == 1 { "1 step".into() } else { format!("{n} steps") }
}

/// "Set up this Mac…": the same setup as the window's button (glintd's POST /v1/setup, which
/// copies the CLI out of the quarantined bundle and runs it in the login shell with a time
/// limit). The window opens on This Mac and shows each step; when setup ends and the window is
/// not in front, a notification says how it went (see `setup_note`).
fn set_up_this_mac(app: &AppHandle) {
    show(app);
    let Some(w) = app.get_webview_window("main") else { return };
    if daemon_up() {
        let _ = w.eval("window.glintSetup ? window.glintSetup() : location.reload()");
    } else {
        // The window says why glintd is not running and offers to start it.
        let _ = w.eval("location.reload()");
    }
}

/// The notification for the end of a setup the window ran (`glint-app://setup-done?failed=2`).
fn setup_note(query: &str) -> (String, String) {
    let param = |k: &str| query.split('&').find_map(|kv| kv.strip_prefix(k)?.strip_prefix('=')).map(str::to_owned);
    if param("error").is_some() {
        return ("Setup did not finish".into(), "Open Glint to see why.".into());
    }
    match param("failed").and_then(|n| n.parse::<usize>().ok()) {
        Some(0) => ("This Mac is set up".into(), "Open a new terminal, then call glint() in any PHP, Python or Node program.".into()),
        Some(n) => (format!("Setup needs a look: {}", steps_word(n)), "Open Glint to see which.".into()),
        None => ("Setup ended".into(), "Open Glint to see how it went.".into()),
    }
}

/// "Open at login": the login item `glint install` writes (one code path with setup and
/// uninstall). The choice is remembered, so setting up again does not undo turning it off.
fn toggle_login(app: &AppHandle, item: CheckMenuItem<Wry>) {
    let want = item.is_checked().unwrap_or(false);
    let app = app.clone();
    std::thread::spawn(move || {
        state::set("open_at_login", want.into());
        let args = if want { "install --only login" } else { "uninstall --only login" };
        let result = run_cli(args, false, Duration::from_secs(20));
        let now = opens_at_login();
        let _ = item.set_checked(now);
        if now != want {
            let why = match result {
                Ok(t) => t.lines().find(|l| l.contains("login")).unwrap_or("").trim().to_owned(),
                Err(e) => e,
            };
            let title = if want { "Could not make Glint open at login" } else { "Could not stop Glint from opening at login" };
            let _ = app.notification().builder().title(title).body(why).show();
        }
    });
}

fn opens_at_login() -> bool {
    std::env::var_os("HOME").map(PathBuf::from).is_some_and(|h| h.join("Library/LaunchAgents/local.glint.app.login.plist").exists())
}

/// "Save diagnostics report…": the window's report (glintd's POST /v1/report) when glintd runs,
/// else `glint report` itself, which matters most exactly when glintd does not start. Either way
/// the file lands in Downloads and Finder shows it.
fn save_report(app: &AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || {
        let note = |title: &str, body: String| {
            let _ = app.notification().builder().title(title).body(body).show();
        };
        let from_glintd = ureq::post(&format!("{}/v1/report", base_url())).set("X-Glint", "1").timeout(Duration::from_secs(150)).call().ok().and_then(|r| r.into_json::<Value>().ok());
        if let Some(path) = from_glintd.as_ref().and_then(|v| v["path"].as_str()) {
            return note("Diagnostics report saved", format!("{} is in Downloads. Send it to whoever helps you with Glint.", base(path)));
        }
        let args = update::own_bundle().map_or_else(|| "report".to_owned(), |b| format!("report --app '{}'", b.display().to_string().replace('\'', "'\\''")));
        match run_cli(&args, true, Duration::from_secs(120)) {
            Ok(t) => match t.lines().find_map(|l| l.trim().strip_prefix("Wrote ")) {
                Some(path) => note("Diagnostics report saved", format!("{} is in Downloads. Send it to whoever helps you with Glint.", base(path))),
                None => note("Could not save the diagnostics report", clip(t.trim(), 180)),
            },
            Err(e) => note("Could not save the diagnostics report", e),
        }
    });
}

const HELP_URL: &str = "https://github.com/sodx/glint/blob/main/docs/INSTALL.md";

/// Asks before uninstalling, in a standard dialog (no dialog plugin needed).
fn confirm_uninstall() -> bool {
    let script = r#"display dialog "Uninstall Glint?

This stops recording and removes what setup added: the background service, glint() in PHP, Python and Node, Glint in your coding agents, and opening at login. Your events stay in ~/.glint.

Afterwards, move Glint to the Trash." with title "Glint" buttons {"Cancel", "Uninstall"} default button "Cancel" cancel button "Cancel" with icon caution"#;
    Command::new("osascript").args(["-e", script]).stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
}

/// "Uninstall Glint…": `glint uninstall` in the login shell, then stop glintd and quit. The full
/// output goes to ~/.glint/uninstall.log.
fn uninstall(app: &AppHandle, status: MenuItem<Wry>) {
    let app = app.clone();
    std::thread::spawn(move || {
        if !confirm_uninstall() {
            return;
        }
        UNINSTALLING.store(true, Ordering::SeqCst);
        let _ = status.set_text("Uninstalling…");
        let result = run_cli("uninstall", true, Duration::from_secs(180));
        let text = result.clone().unwrap_or_else(|e| e);
        let _ = std::fs::write(state::glint_dir().join("uninstall.log"), &text);
        stop_own_daemon(true);
        let (title, body) = match result {
            Ok(t) if failed_steps(&t) == 0 => ("Glint is uninstalled".to_owned(), "Your events are kept in ~/.glint. Move Glint to the Trash to finish.".to_owned()),
            Ok(t) => (format!("Uninstall needs a look: {}", steps_word(failed_steps(&t))), "The details are in ~/.glint/uninstall.log.".to_owned()),
            Err(e) => ("Uninstall did not finish".to_owned(), clip(&e, 180)),
        };
        let _ = app.notification().builder().title(title).body(body).show();
        app.exit(0);
    });
}

fn open_url(url: &str) {
    #[cfg(target_os = "macos")]
    let _ = Command::new("open").arg(url).spawn();
    #[cfg(target_os = "linux")]
    let _ = Command::new("xdg-open").arg(url).spawn();
    #[cfg(target_os = "windows")]
    let _ = Command::new("cmd").args(["/C", "start", "", url]).spawn();
}

fn open_in_browser() {
    open_url(&base_url());
}

// ------------------------------------------------------------------ watcher

/// The same names the UI gives runs: a program's run by its request line (`POST /checkout/8`)
/// or its script (`worker.py`), others by what started them.
fn run_title(run: &str) -> String {
    if run.starts_with("example-") {
        return "Example run".into();
    }
    if run.starts_with("js-") || run.starts_with("python-") || run.starts_with("php-") {
        let summary = get("/v1/runs?limit=200").and_then(|r| r["runs"].as_array()?.iter().find(|x| x["run"] == run).cloned());
        if let Some(r) = summary {
            if let Some(head) = r["head"].as_str().filter(|h| h.split_once(' ').is_some_and(|(m, p)| !m.is_empty() && m.chars().all(|c| c.is_ascii_uppercase()) && p.starts_with('/'))) {
                return head.split(' ').take(2).collect::<Vec<_>>().join(" ");
            }
            if let Some(f) = r["file"].as_str() {
                return base(f).to_owned();
            }
        }
    }
    let prefixes = [("claude-code:", "Agent session"), ("otel-", "Trace"), ("pipe-", "Piped output"), ("send-", "glint send"), ("js-", "Node process"), ("python-", "Python process"), ("php-", "PHP process")];
    prefixes.iter().find(|(p, _)| run.starts_with(p)).map(|(_, t)| (*t).to_owned()).unwrap_or_else(|| run.to_owned())
}

fn fail_text(e: &Value) -> String {
    let p = &e["payload"];
    let s = |k: &str| p[k].as_str().unwrap_or("").trim().to_owned();
    let text = match (s("class"), s("message")) {
        (c, m) if !c.is_empty() && !m.is_empty() => format!("{c}: {m}"),
        (c, m) if m.is_empty() => c,
        (_, m) => m,
    };
    let text = if text.is_empty() { s("output").lines().find(|l| !l.trim().is_empty()).unwrap_or("").to_owned() } else { text };
    if text.is_empty() { e["kind"].as_str().unwrap_or("error").to_owned() } else { clip(&text, 180) }
}

fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n { s.to_owned() } else { format!("{}…", s.chars().take(n).collect::<String>()) }
}

fn base(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Ok,
    Fail,
    Wait,
    Offline,
}

/// Failures stored since local midnight, of the newest `list` (oldest first).
fn failures_today(list: &[Value]) -> usize {
    let midnight = chrono::Local::now().date_naive().and_hms_opt(0, 0, 0).and_then(|m| m.and_local_timezone(chrono::Local).earliest());
    list.iter()
        .filter(|e| state::counts(e))
        .filter(|e| {
            let ts = e["ts"].as_str().and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok());
            matches!((ts, midnight), (Some(t), Some(m)) if t >= m)
        })
        .count()
}

/// What the menu bar says while glintd does not answer. The reason (a line of glintd.log, a
/// missing binary) is for the window and the diagnostics report, not the menu bar.
const OFFLINE_LINE: &str = "Not recording. Open Glint to see why.";

/// Polls glintd every two seconds: notifies about new failures and pauses, and keeps the tray
/// icon and status line current. A failure stays red until you look at the window, also across a
/// restart: the newest failure you saw is remembered (`state`).
fn watch(app: AppHandle, status: MenuItem<Wry>, seen_by_user: Arc<AtomicBool>) {
    let mut last_error: Option<String> = None;
    let mut seen: Option<String> = state::get("seen_failure").as_str().map(str::to_owned);
    let mut fail_title = String::new();
    let mut known_pauses: Vec<String> = Vec::new();
    let mut first = true;
    let mut failing = false;
    let mut shown: Option<(State, String)> = None;
    let mut misses = 0u32;
    loop {
        // `glint uninstall` ran in a terminal: do not start glintd again, quit.
        if uninstalled() && !UNINSTALLING.load(Ordering::SeqCst) {
            let _ = app.notification().builder().title("Glint was uninstalled").body("Glint has quit. Your events are kept in ~/.glint.").show();
            stop_own_daemon(true);
            app.exit(0);
            return;
        }
        let errors = get("/v1/events?level=error&limit=500");
        let pauses = get("/v1/ctl");
        let state;
        let line;
        match (errors, pauses) {
            (Some(errors), Some(pauses)) => {
                let list = errors["events"].as_array().cloned().unwrap_or_default();
                let today = failures_today(&list);
                if let Some(e) = state::newest_failure(&list) {
                    let id = e["id"].as_str().unwrap_or("").to_owned();
                    if last_error.as_deref() != Some(&id) {
                        fail_title = run_title(e["run"].as_str().unwrap_or(""));
                        if first {
                            // Old failures are not news, but one nobody saw before a restart or
                            // an update is still red.
                            if state::unseen_at_start(&id, seen.as_deref()) && !in_front(&app) {
                                failing = true;
                                seen_by_user.store(false, Ordering::Relaxed);
                            }
                        } else {
                            failing = true;
                            seen_by_user.store(false, Ordering::Relaxed);
                            let _ = app.notification().builder().title(format!("✗ {fail_title} failed")).body(fail_text(e)).show();
                        }
                        last_error = Some(id);
                    }
                }
                if seen_by_user.load(Ordering::Relaxed) {
                    failing = false;
                }
                if !failing && last_error.is_some() && seen != last_error {
                    seen.clone_from(&last_error);
                    state::set("seen_failure", seen.clone().into());
                }
                let list = pauses["pauses"].as_array().cloned().unwrap_or_default();
                for p in &list {
                    let id = p["id"].as_str().unwrap_or("").to_owned();
                    if !known_pauses.contains(&id) && !first {
                        let at = p["file"].as_str().map(|f| format!("{}{}", base(f), p["line"].as_u64().map(|l| format!(":{l}")).unwrap_or_default()));
                        let body = [p["reason"].as_str().map(|r| format!("“{r}”")), at].into_iter().flatten().collect::<Vec<_>>().join(" · ");
                        let _ = app
                            .notification()
                            .builder()
                            .title(format!("⏸ {} is waiting for you", run_title(p["run"].as_str().unwrap_or(""))))
                            .body(if body.is_empty() { "Continue it in Glint".into() } else { body })
                            .show();
                    }
                }
                known_pauses = list.iter().filter_map(|p| p["id"].as_str().map(str::to_owned)).collect();
                (state, line) = if !list.is_empty() {
                    (State::Wait, format!("⏸ {} waiting for you", if list.len() == 1 { "A program is".into() } else { format!("{} programs are", list.len()) }))
                } else if failing {
                    (State::Fail, format!("✗ {} failed · {today} today", clip(&fail_title, 60)))
                } else {
                    (State::Ok, "✓ All quiet".to_owned())
                };
                first = false;
                misses = 0;
            }
            _ => {
                // A restart (e.g. `glint install` replacing the binary) takes under a second.
                misses += 1;
                if misses >= 3 && !UNINSTALLING.load(Ordering::SeqCst) {
                    if let Err(why) = ensure_daemon() {
                        eprintln!("glint: glintd is not running: {why}");
                    }
                    misses = 0;
                }
                (state, line) = (State::Offline, OFFLINE_LINE.to_owned());
            }
        }
        if UNINSTALLING.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_secs(2));
            continue;
        }
        if shown.as_ref() != Some(&(state, line.clone())) {
            shown = Some((state, line.clone()));
            let _ = status.set_text(&line);
            if let Some(tray) = app.tray_by_id("main") {
                let (bytes, template) = match state {
                    State::Ok => (TRAY_OK, true),
                    State::Offline => (TRAY_OFFLINE, true),
                    State::Fail => (TRAY_FAIL, false),
                    State::Wait => (TRAY_WAIT, false),
                };
                if let Ok(img) = Image::from_bytes(bytes) {
                    let _ = tray.set_icon(Some(img));
                    let _ = tray.set_icon_as_template(template);
                }
                let _ = tray.set_tooltip(Some(format!("Glint: {line}")));
            }
            if let Some(w) = app.get_webview_window("main") {
                let badge = match state {
                    State::Fail => Some("✗".to_owned()),
                    State::Wait => Some("⏸".to_owned()),
                    State::Ok | State::Offline => None,
                };
                let _ = w.set_badge_label(badge);
            }
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

// ------------------------------------------------------------------ main

/// A downloaded copy of Glint carries macOS's quarantine flag on every file inside, and macOS
/// kills the bundled glintd and glint while they have it. Glint is running, so the user has
/// already let it open: clear the flag on our own bundle so its tools run too.
fn clear_own_quarantine() {
    if !cfg!(target_os = "macos") {
        return;
    }
    let bundle = std::env::current_exe().ok().and_then(|e| e.ancestors().find(|a| a.extension().is_some_and(|x| x == "app")).map(std::path::Path::to_path_buf));
    if let Some(b) = bundle {
        let _ = Command::new("xattr").args(["-dr", "com.apple.quarantine"]).arg(&b).stdout(Stdio::null()).stderr(Stdio::null()).status();
    }
}

fn main() {
    clear_own_quarantine();
    // Opened after `glint uninstall`: someone wants Glint again, so it may start glintd.
    let _ = std::fs::remove_file(state::glint_dir().join("uninstalled"));
    // Opened by the login item: the spark only, the window stays closed until asked for.
    let at_login = std::env::args().any(|a| a == "--login");
    update::sync_tools();
    if let Err(why) = ensure_daemon() {
        eprintln!("glint: glintd is not running: {why}");
    }
    let seen_by_user = Arc::new(AtomicBool::new(true));
    let seen = seen_by_user.clone();
    let hotkey = Shortcut::new(Some(Modifiers::ALT | Modifiers::SUPER), Code::KeyG);

    let app = tauri::Builder::default()
        // A short app menu (Glint, Edit, Window): the default one is wide, and on a MacBook with
        // a notch every extra title pushes menu-bar icons, the spark included, behind it.
        .menu(|app| {
            Menu::with_items(
                app,
                &[
                    &Submenu::with_items(
                        app,
                        "Glint",
                        true,
                        &[
                            &PredefinedMenuItem::about(app, Some("About Glint"), None)?,
                            &PredefinedMenuItem::separator(app)?,
                            &PredefinedMenuItem::hide(app, None)?,
                            &PredefinedMenuItem::hide_others(app, None)?,
                            &PredefinedMenuItem::separator(app)?,
                            &PredefinedMenuItem::quit(app, Some("Quit Glint"))?,
                        ],
                    )?,
                    &Submenu::with_items(
                        app,
                        "Edit",
                        true,
                        &[
                            &PredefinedMenuItem::undo(app, None)?,
                            &PredefinedMenuItem::redo(app, None)?,
                            &PredefinedMenuItem::separator(app)?,
                            &PredefinedMenuItem::cut(app, None)?,
                            &PredefinedMenuItem::copy(app, None)?,
                            &PredefinedMenuItem::paste(app, None)?,
                            &PredefinedMenuItem::select_all(app, None)?,
                        ],
                    )?,
                    &Submenu::with_items(
                        app,
                        "Window",
                        true,
                        &[&PredefinedMenuItem::minimize(app, None)?, &PredefinedMenuItem::close_window(app, Some("Close"))?],
                    )?,
                ],
            )
        })
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| show(app)))
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(move |app, shortcut, event| {
                    if shortcut == &hotkey && event.state() == ShortcutState::Pressed {
                        toggle(app);
                    }
                })
                .build(),
        )
        .setup(move |app| {
            let handle = app.handle().clone();
            let _ = app.global_shortcut().register(hotkey);

            let status = MenuItem::with_id(app, "status", "Connecting…", false, None::<&str>)?;
            let open = MenuItem::with_id(app, "open", "Open Glint", true, Some("Alt+CmdOrCtrl+G"))?;
            let browser = MenuItem::with_id(app, "browser", "Open in browser", true, None::<&str>)?;
            let setup = MenuItem::with_id(app, "setup", "Set up this Mac…", true, None::<&str>)?;
            let login = CheckMenuItem::with_id(app, "login", "Open at login", update::own_bundle().is_some() && menu_cli().is_some(), opens_at_login(), None::<&str>)?;
            let updates = MenuItem::with_id(app, "update", "Check for updates…", true, None::<&str>)?;
            let report = MenuItem::with_id(app, "report", "Save diagnostics report…", true, None::<&str>)?;
            let help = MenuItem::with_id(app, "help", "Glint help", true, None::<&str>)?;
            let remove = MenuItem::with_id(app, "uninstall", "Uninstall Glint…", menu_cli().is_some(), None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit Glint", true, Some("CmdOrCtrl+Q"))?;
            let sep = || PredefinedMenuItem::separator(app);
            let menu = Menu::with_items(
                app,
                &[&open, &status, &sep()?, &browser, &setup, &login, &sep()?, &updates, &report, &help, &sep()?, &remove, &quit],
            )?;
            update::start(app.handle(), updates);
            let (login_item, status_item) = (login.clone(), status.clone());
            // A click on the spark opens this menu, as menu-bar items do on a Mac; ⌥⌘G (or
            // Open Glint) opens the window.
            TrayIconBuilder::with_id("main")
                .icon(Image::from_bytes(TRAY_OK)?)
                .icon_as_template(true)
                .tooltip("Glint")
                .menu(&menu)
                .show_menu_on_left_click(true)
                .on_menu_event(move |app, e| match e.id.as_ref() {
                    "open" => show(app),
                    "browser" => open_in_browser(),
                    "setup" => set_up_this_mac(app),
                    "login" => toggle_login(app, login_item.clone()),
                    "update" => update::menu_clicked(app),
                    "report" => save_report(app),
                    "help" => open_url(HELP_URL),
                    "uninstall" => uninstall(app, status_item.clone()),
                    "quit" => app.exit(0),
                    _ => {}
                })
                .build(app)?;

            let url = base_url().parse().map_err(|e| format!("GLINT_URL: {e}"))?;
            let nav = handle.clone();
            let window = WebviewWindowBuilder::new(app, "main", WebviewUrl::External(url))
                // The version shows in the title bar, and in About.
                .title(format!("Glint {}", update::VERSION))
                // The update banner links to glint-app://update; the click never leaves the page.
                // The page's "Start glintd" button links to glint-app://start-glintd.
                // When setup the window ran ends, it links to glint-app://setup-done?failed=N.
                .on_navigation(move |url| {
                    if url.scheme() == "glint-app" {
                        match url.host_str() {
                            Some("start-glintd") => {
                                std::thread::spawn(ensure_daemon);
                            }
                            Some("setup-done") => {
                                if !in_front(&nav) {
                                    let (title, body) = setup_note(url.query().unwrap_or(""));
                                    let _ = nav.notification().builder().title(title).body(body).show();
                                }
                            }
                            _ => update::banner_clicked(&nav),
                        }
                        return false;
                    }
                    true
                })
                .on_page_load(|w, p| {
                    if p.event() == PageLoadEvent::Finished {
                        // Tells the page it runs in the app, which can start glintd for it.
                        let _ = w.eval("window.glintApp = true");
                        update::banner(w.app_handle());
                    }
                })
                .visible(!at_login)
                .inner_size(560.0, 820.0)
                .min_inner_size(380.0, 420.0)
                .build()?;
            let w = window.clone();
            let seen = seen.clone();
            window.on_window_event(move |e| match e {
                WindowEvent::CloseRequested { api, .. } => {
                    api.prevent_close();
                    let _ = w.hide();
                }
                WindowEvent::Focused(true) => seen.store(true, Ordering::Relaxed),
                _ => {}
            });

            let seen = seen_by_user.clone();
            // First run on this Mac: point at the one-click setup.
            let first_run = std::env::var_os("HOME").map(PathBuf::from).is_some_and(|h| !h.join(".glint/bin/glint").exists());
            if first_run && !at_login && bundled_cli().is_some() {
                let _ = app
                    .notification()
                    .builder()
                    .title("Welcome to Glint")
                    .body("Click \"Set up this Mac\" in the Glint window, or click the spark in the menu bar and choose \"Set up this Mac…\", to get glint() in PHP, Python and Node and connect your coding agents.")
                    .show();
            }
            std::thread::spawn(move || watch(handle, status, seen));
            Ok(())
        })
        .build({
            let mut ctx = tauri::generate_context!();
            update::configure(&mut ctx);
            ctx
        })
        .expect("Glint could not start");

    app.run(|app, event| {
        #[cfg(target_os = "macos")]
        if let RunEvent::Reopen { .. } = event {
            show(app);
        }
        // Quit: without a login service nothing else records, so the glintd this app started
        // stops too, and nothing keeps recording behind the user's back.
        if let RunEvent::Exit = event {
            stop_own_daemon(false);
        }
        let _ = (app, event);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_failures_since_local_midnight_only() {
        let now = chrono::Local::now();
        let midnight = now.date_naive().and_hms_opt(0, 0, 0).unwrap().and_local_timezone(chrono::Local).earliest().unwrap();
        let ts = |t: chrono::DateTime<chrono::Local>| t.with_timezone(&chrono::Utc).to_rfc3339();
        let list = vec![
            serde_json::json!({ "kind": "core.exception", "ts": ts(midnight - chrono::Duration::minutes(1)) }),
            serde_json::json!({ "kind": "core.exception", "ts": ts(midnight + chrono::Duration::seconds(1)) }),
            serde_json::json!({ "kind": "core.update", "ts": ts(now) }),
            serde_json::json!({ "kind": "core.log", "ts": ts(now) }),
        ];
        assert_eq!(failures_today(&list), 2);
        let example = vec![serde_json::json!({ "kind": "core.exception", "project": "example", "run": "example-abc", "ts": ts(now) })];
        assert_eq!(failures_today(&example), 0, "the sample run is not counted");
    }

    #[test]
    fn the_example_run_has_the_windows_name() {
        assert_eq!(run_title("example-mabc123"), "Example run");
        assert_eq!(run_title("pipe-1"), "Piped output");
    }

    #[test]
    fn setup_notifications_say_how_it_went() {
        assert_eq!(setup_note("failed=0").0, "This Mac is set up");
        assert_eq!(setup_note("failed=2"), ("Setup needs a look: 2 steps".into(), "Open Glint to see which.".into()));
        assert_eq!(setup_note("failed=1").0, "Setup needs a look: 1 step");
        assert_eq!(setup_note("error=1").0, "Setup did not finish");
    }

    #[test]
    fn the_menu_bar_never_shows_internals_when_offline() {
        assert!(!OFFLINE_LINE.contains("glintd") && !OFFLINE_LINE.contains('('));
    }

    #[test]
    fn failed_steps_are_counted_from_the_checklist() {
        let out = "  ✓  service  stopped\n  ✗  php      could not write\n  –  data     kept\n  ✗  node     no\n";
        assert_eq!(failed_steps(out), 2);
        assert_eq!(failed_steps("  ✓  login    removed\n"), 0);
    }

    #[test]
    fn the_update_notification_names_the_real_menu_item_and_button() {
        let t = update::ready_text("0.2.0");
        assert!(t.contains("\"Update to 0.2.0…\"") && t.contains("\"Update and restart\""));
    }
}
