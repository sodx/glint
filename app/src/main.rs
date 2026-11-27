//! Glint for the desktop.
//!
//! - a window with the live UI served by glintd (the same `ui/index.html` the browser gets);
//! - a menu-bar spark that turns red when something fails and amber when a program waits for
//!   you, with a status line, Open, Open in browser and Quit;
//! - a notification for each new failure and each new pause;
//! - ⌥⌘G shows and hides the window from anywhere;
//! - glintd is started if nothing answers (the copy inside the app bundle, else ~/.glint/bin).
//!
//! Closing the window keeps Glint in the menu bar; Quit (menu or ⌘Q) ends it. glintd keeps
//! running either way, so events are never lost while the window is closed.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::Value;
use tauri::image::Image;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, RunEvent, WebviewUrl, WebviewWindowBuilder, WindowEvent, Wry};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};
use tauri_plugin_notification::NotificationExt;

const TRAY_OK: &[u8] = include_bytes!("../icons/tray.png");
const TRAY_FAIL: &[u8] = include_bytes!("../icons/tray-fail.png");
const TRAY_WAIT: &[u8] = include_bytes!("../icons/tray-wait.png");

fn base_url() -> String {
    std::env::var("GLINT_URL").unwrap_or_else(|_| "http://127.0.0.1:23600".into()).trim_end_matches('/').to_owned()
}

fn get(path: &str) -> Option<Value> {
    ureq::get(&format!("{}{path}", base_url())).timeout(Duration::from_millis(1500)).call().ok()?.into_json().ok()
}

// ------------------------------------------------------------------ glintd

fn daemon_up() -> bool {
    get("/v1/info").is_some()
}

/// Start glintd when nothing answers: the copy bundled next to this binary, else the one
/// `glint install` put in ~/.glint/bin. Its output goes to ~/.glint/glintd.log.
fn ensure_daemon() {
    if daemon_up() {
        return;
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        candidates.push(exe.with_file_name("glintd"));
    }
    candidates.push(home.join(".glint/bin/glintd"));
    let Some(bin) = candidates.into_iter().find(|p| p.exists()) else { return };
    let log = std::fs::create_dir_all(home.join(".glint"))
        .and_then(|()| std::fs::OpenOptions::new().create(true).append(true).open(home.join(".glint/glintd.log")));
    let mut cmd = Command::new(bin);
    cmd.stdin(Stdio::null());
    if let Ok(f) = log {
        if let Ok(f2) = f.try_clone() {
            cmd.stdout(f).stderr(f2);
        }
    }
    if cmd.spawn().is_ok() {
        for _ in 0..30 {
            if daemon_up() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
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

/// The same names the UI gives runs.
fn run_title(run: &str) -> String {
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

/// Polls glintd every two seconds: notifies about new failures and pauses, and keeps the tray
/// icon and status line current. A failure stays red until you look at the window.
fn watch(app: AppHandle, status: MenuItem<Wry>, seen_by_user: Arc<AtomicBool>) {
    let mut last_error: Option<String> = None;
    let mut known_pauses: Vec<String> = Vec::new();
    let mut first = true;
    let mut failing = false;
    let mut shown: Option<State> = None;
    loop {
        let errors = get("/v1/events?level=error&limit=1");
        let pauses = get("/v1/ctl");
        let state;
        let line;
        match (errors, pauses) {
            (Some(errors), Some(pauses)) => {
                if let Some(e) = errors["events"].as_array().and_then(|a| a.first()) {
                    let id = e["id"].as_str().unwrap_or("").to_owned();
                    if last_error.as_deref() != Some(&id) {
                        if !first {
                            failing = true;
                            seen_by_user.store(false, Ordering::Relaxed);
                            let title = format!("✗ {} failed", run_title(e["run"].as_str().unwrap_or("")));
                            let _ = app.notification().builder().title(title).body(fail_text(e)).show();
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
                    (State::Fail, "✗ Something failed".to_owned())
                } else {
                    (State::Ok, "✓ All quiet".to_owned())
                };
                first = false;
            }
            _ => {
                (state, line) = (State::Offline, "glintd is not running".to_owned());
                ensure_daemon();
            }
        }
        if shown != Some(state) {
            shown = Some(state);
            let _ = status.set_text(&line);
            if let Some(tray) = app.tray_by_id("main") {
                let (bytes, template) = match state {
                    State::Ok | State::Offline => (TRAY_OK, true),
                    State::Fail => (TRAY_FAIL, false),
                    State::Wait => (TRAY_WAIT, false),
                };
                if let Ok(img) = Image::from_bytes(bytes) {
                    let _ = tray.set_icon(Some(img));
                    let _ = tray.set_icon_as_template(template);
                }
                let _ = tray.set_tooltip(Some(format!("Glint — {line}")));
            }
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

// ------------------------------------------------------------------ main

fn main() {
    ensure_daemon();
    let seen_by_user = Arc::new(AtomicBool::new(true));
    let seen = seen_by_user.clone();
    let hotkey = Shortcut::new(Some(Modifiers::ALT | Modifiers::SUPER), Code::KeyG);

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| show(app)))
        .plugin(tauri_plugin_notification::init())
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
            let quit = MenuItem::with_id(app, "quit", "Quit Glint", true, Some("CmdOrCtrl+Q"))?;
            let menu = Menu::with_items(app, &[&status, &PredefinedMenuItem::separator(app)?, &open, &browser, &PredefinedMenuItem::separator(app)?, &quit])?;
            TrayIconBuilder::with_id("main")
                .icon(Image::from_bytes(TRAY_OK)?)
                .icon_as_template(true)
                .tooltip("Glint")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, e| match e.id.as_ref() {
                    "open" => show(app),
                    "browser" => open_in_browser(),
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
            let window = WebviewWindowBuilder::new(app, "main", WebviewUrl::External(url))
                .title("Glint")
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
            std::thread::spawn(move || watch(handle, status, seen));
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("Glint could not start");

    app.run(|app, event| {
        #[cfg(target_os = "macos")]
        if let RunEvent::Reopen { .. } = event {
            show(app);
        }
        let _ = (app, event);
    });
}
