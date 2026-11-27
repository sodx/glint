//! "Set up this Mac" from the window: `GET /v1/setup` says what is set up (`glint doctor`), and
//! `POST /v1/setup` sets it up (`glint install`). Both run the glint CLI through the user's login
//! shell, so the user's PATH (Homebrew, nvm, pyenv) decides which php, python3 and node are
//! checked and set up, the same as in their terminal. glintd itself often runs with a bare PATH
//! (started at login, or by the desktop app).
//!
//! Only this server's own pages can call these (the origin guard lets other origins send events
//! and nothing else).

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

static RUNNING: AtomicBool = AtomicBool::new(false);

/// The glint CLI: next to this glintd (a build, ~/.glint/bin), inside Glint.app, or installed.
fn cli() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        candidates.push(exe.with_file_name("glint"));
        if let Some(contents) = exe.parent().and_then(|p| p.parent()) {
            candidates.push(contents.join("Resources/bin/glint")); // Glint.app/Contents/MacOS/glintd
        }
    }
    candidates.push(home.join(".glint/bin/glint"));
    candidates.push(PathBuf::from("/Applications/Glint.app/Contents/Resources/bin/glint"));
    candidates.push(home.join("Applications/Glint.app/Contents/Resources/bin/glint"));
    candidates.into_iter().find(|p| p.is_file())
}

/// The user's login shell, from the user database (a process started at login has no $SHELL).
fn login_shell() -> String {
    let user = std::env::var("USER").unwrap_or_default();
    let from_db = if cfg!(target_os = "macos") {
        std::process::Command::new("dscl")
            .args([".", "-read", &format!("/Users/{user}"), "UserShell"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8_lossy(&o.stdout).split_whitespace().nth(1).map(str::to_owned))
    } else {
        std::process::Command::new("getent")
            .args(["passwd", &user])
            .output()
            .ok()
            .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().rsplit(':').next().map(str::to_owned))
    };
    from_db
        .or_else(|| std::env::var("SHELL").ok())
        .filter(|s| std::path::Path::new(s).is_file())
        .unwrap_or_else(|| "/bin/sh".into())
}

/// Runs `glint <args> --json` in a login shell; returns the steps and everything it printed.
async fn run(args: &str, timeout: Duration) -> Result<(Value, String), String> {
    let cli = cli().ok_or("the glint command line tool was not found next to glintd, in Glint.app or in ~/.glint/bin")?;
    let shell = login_shell();
    let line = format!("'{}' {args} --json 2>&1", cli.display());
    let child = tokio::process::Command::new(&shell)
        .args(["-lic", &line])
        .env("SHELL", &shell)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("could not start {shell}: {e}"))?;
    let out = tokio::time::timeout(timeout, child.wait_with_output())
        .await
        .map_err(|_| format!("glint {args} did not finish within {} s", timeout.as_secs()))?
        .map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let steps = text
        .lines()
        .find_map(|l| l.trim().strip_prefix("GLINT_JSON:"))
        .and_then(|j| serde_json::from_str(j).ok())
        .ok_or_else(|| format!("glint {args} gave no result; it printed:\n{}", text.lines().rev().take(15).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n")))?;
    let log = text.lines().filter(|l| !l.trim().starts_with("GLINT_JSON:")).collect::<Vec<_>>().join("\n");
    Ok((steps, log))
}

/// `GET /v1/setup` → `{steps: [{name, status: ok|skip|fail, detail}], shell}`.
pub async fn status() -> Response {
    match run("doctor", Duration::from_secs(40)).await {
        Ok((steps, _)) => Json(json!({ "steps": steps, "shell": login_shell() })).into_response(),
        Err(e) => crate::routes::error(StatusCode::SERVICE_UNAVAILABLE, e),
    }
}

/// `POST /v1/setup` → `{steps, log}`; the log also goes to ~/.glint/setup.log.
pub async fn install() -> Response {
    if RUNNING.swap(true, Ordering::SeqCst) {
        return crate::routes::error(StatusCode::CONFLICT, "setup is already running".into());
    }
    let result = run("install", Duration::from_secs(180)).await;
    RUNNING.store(false, Ordering::SeqCst);
    match result {
        Ok((steps, log)) => {
            if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
                let _ = std::fs::write(home.join(".glint/setup.log"), &log);
            }
            Json(json!({ "steps": steps, "log": log })).into_response()
        }
        Err(e) => crate::routes::error(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}
