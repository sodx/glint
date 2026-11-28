//! Glint for the desktop.
//!
//! - a window with the live UI served by glintd (the same `ui/index.html` the browser gets);
//! - a menu-bar spark that turns red when something fails, amber when a program waits for you
//!   and struck through when glintd is not running, with a status line that names the newest
//!   failure, Open, Open in browser and Quit;
//! - a notification for each new failure and each new pause;
//! - the Dock icon carries the same state as a badge (✗ or ⏸), because on a MacBook with a notch
//!   the menu-bar spark can be hidden behind it;
//! - ⌥⌘G shows and hides the window from anywhere;
//! - glintd is started if nothing answers (the copy inside the app bundle, else ~/.glint/bin);
//! - updates itself from signed releases, glintd and the CLI included (see `update`).
//!
//! Closing the window keeps Glint in the menu bar; Quit (menu or ⌘Q) ends it. glintd keeps
//! running either way, so events are never lost while the window is closed.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod update;

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::Value;
use tauri::image::Image;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
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
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    if cfg!(target_os = "macos") && home.join(format!("Library/LaunchAgents/{SERVICE_LABEL}.plist")).exists() {
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
    let Some(bin) = candidates.into_iter().next() else { return Err("no glintd found in the app or ~/.glint/bin".into()) };
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
        Ok(_) if wait_up(30) => Ok(()),
        Ok(_) => Err(why()),
        Err(e) => Err(format!("could not start {}: {e}", cmd.get_program().to_string_lossy())),
    }
}

// ------------------------------------------------------------------ window

fn show(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
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

// ------------------------------------------------------------------ set up this Mac

/// The `glint` CLI inside this app bundle (Contents/Resources/bin/glint).
fn bundled_cli() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let p = exe.parent()?.parent()?.join("Resources/bin/glint");
    p.is_file().then_some(p)
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

/// "Set up this Mac…": runs the bundled `glint install` in a login shell, so that the user's PATH
/// (Homebrew, nvm, pyenv) decides which php, python3 and node get glint(). The full output goes to
/// ~/.glint/setup.log; a notification sums it up.
fn set_up_this_mac(app: &AppHandle) {
    let Some(cli) = bundled_cli() else {
        let _ = app.notification().builder().title("Glint").body("This copy of Glint has no command line tool inside; reinstall the app.").show();
        return;
    };
    let app = app.clone();
    std::thread::spawn(move || {
        let shell = login_shell();
        let cmd = format!("'{}' install 2>&1", cli.display());
        let out = Command::new(&shell).args(["-lic", &cmd]).env("SHELL", &shell).stdin(Stdio::null()).output();
        let text = match out {
            Ok(o) => String::from_utf8_lossy(&o.stdout).into_owned(),
            Err(e) => format!("could not run {shell}: {e}"),
        };
        if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
            let _ = std::fs::create_dir_all(home.join(".glint"));
            let _ = std::fs::write(home.join(".glint/setup.log"), &text);
        }
        let ok: Vec<&str> = text.lines().filter(|l| l.trim_start().starts_with('✓')).filter_map(|l| l.split_whitespace().nth(1)).collect();
        let failed = text.lines().filter(|l| l.trim_start().starts_with('✗')).count();
        let mut done: Vec<&str> = ok.clone();
        done.dedup();
        let body = if failed == 0 && !done.is_empty() {
            format!("Ready: {}. Open a new terminal, then call glint() in any PHP, Python or Node program.", done.join(", "))
        } else if failed > 0 {
            format!("{failed} step(s) need a look; the details are in ~/.glint/setup.log.")
        } else {
            "Nothing was set up; the details are in ~/.glint/setup.log.".to_owned()
        };
        let _ = app.notification().builder().title("Glint is set up").body(body).show();
    });
}

fn open_in_browser() {
    let url = base_url();
    #[cfg(target_os = "macos")]
    let _ = Command::new("open").arg(&url).spawn();
    #[cfg(target_os = "linux")]
    let _ = Command::new("xdg-open").arg(&url).spawn();
    #[cfg(target_os = "windows")]
    let _ = Command::new("cmd").args(["/C", "start", "", &url]).spawn();
}

// ------------------------------------------------------------------ watcher

/// The same names the UI gives runs: a program's run by its request line (`POST /checkout/8`)
/// or its script (`worker.py`), others by what started them.
fn run_title(run: &str) -> String {
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
        .filter(|e| !matches!(e["kind"].as_str(), Some("core.update" | "ctl.resume")))
        .filter(|e| {
            let ts = e["ts"].as_str().and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok());
            matches!((ts, midnight), (Some(t), Some(m)) if t >= m)
        })
        .count()
}

/// Polls glintd every two seconds: notifies about new failures and pauses, and keeps the tray
/// icon and status line current. A failure stays red until you look at the window.
fn watch(app: AppHandle, status: MenuItem<Wry>, seen_by_user: Arc<AtomicBool>) {
    let mut last_error: Option<String> = None;
    let mut fail_title = String::new();
    let mut known_pauses: Vec<String> = Vec::new();
    let mut first = true;
    let mut failing = false;
    let mut shown: Option<(State, String)> = None;
    let mut misses = 0u32;
    let mut down_why: Option<String> = None;
    loop {
        let errors = get("/v1/events?level=error&limit=500");
        let pauses = get("/v1/ctl");
        let state;
        let line;
        match (errors, pauses) {
            (Some(errors), Some(pauses)) => {
                let list = errors["events"].as_array().cloned().unwrap_or_default();
                let today = failures_today(&list);
                if let Some(e) = list.iter().rev().find(|e| !matches!(e["kind"].as_str(), Some("core.update" | "ctl.resume"))) {
                    let id = e["id"].as_str().unwrap_or("").to_owned();
                    if last_error.as_deref() != Some(&id) {
                        fail_title = run_title(e["run"].as_str().unwrap_or(""));
                        if !first {
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
                if misses >= 3 {
                    down_why = ensure_daemon().err();
                    misses = 0;
                }
                (state, line) = (State::Offline, match &down_why {
                    Some(why) => format!("Not recording: glintd is not running ({})", clip(why, 80)),
                    None => "Not recording: glintd is not running".to_owned(),
                });
            }
        }
        if state != State::Offline {
            down_why = None;
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
            let browser = MenuItem::with_id(app, "browser", "Open in Browser", true, None::<&str>)?;
            let setup = MenuItem::with_id(app, "setup", "Set up this Mac…", bundled_cli().is_some(), None::<&str>)?;
            let updates = MenuItem::with_id(app, "update", "Check for Updates…", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit Glint", true, Some("CmdOrCtrl+Q"))?;
            let menu = Menu::with_items(app, &[&status, &PredefinedMenuItem::separator(app)?, &open, &browser, &setup, &updates, &PredefinedMenuItem::separator(app)?, &quit])?;
            update::start(app.handle(), updates);
            TrayIconBuilder::with_id("main")
                .icon(Image::from_bytes(TRAY_OK)?)
                .icon_as_template(true)
                .tooltip("Glint")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, e| match e.id.as_ref() {
                    "open" => show(app),
                    "browser" => open_in_browser(),
                    "setup" => set_up_this_mac(app),
                    "update" => update::menu_clicked(app),
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, e| {
                    if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = e {
                        toggle(tray.app_handle());
                    }
                })
                .build(app)?;

            let url = base_url().parse().map_err(|e| format!("GLINT_URL: {e}"))?;
            let nav = handle.clone();
            let window = WebviewWindowBuilder::new(app, "main", WebviewUrl::External(url))
                // The version shows in the title bar, and in About.
                .title(format!("Glint {}", update::VERSION))
                // The update banner links to glint-app://update; the click never leaves the page.
                // The page's "Start glintd" button links to glint-app://start-glintd.
                .on_navigation(move |url| {
                    if url.scheme() == "glint-app" {
                        if url.host_str() == Some("start-glintd") {
                            std::thread::spawn(ensure_daemon);
                        } else {
                            update::banner_clicked(&nav);
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
            if first_run && bundled_cli().is_some() {
                let _ = app.notification().builder().title("Welcome to Glint").body("Choose \"Set up this Mac…\" in the menu bar spark to get glint() in PHP, Python and Node and connect your coding agents.").show();
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
    }
}
