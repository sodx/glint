//! "Set up this Mac" from the window: `GET /v1/setup` says what is set up (`glint doctor`), and
//! `POST /v1/setup` sets it up (`glint install`). Both run the glint CLI through the user's login
//! shell, so the user's PATH (Homebrew, nvm, pyenv) decides which php, python3 and node are
//! checked and set up, the same as in their terminal. glintd itself often runs with a bare PATH
//! (started at login, or by the desktop app).
//!
//! `POST /v1/report` ("Send diagnostics") runs `glint report` the same way (see `report`).
//!
//! Only this server's own pages and the CLI can call these: the guard wants Glint's origin or the
//! `X-Glint` header. Each one starts shells, so they also take turns (one at a time, see
//! `SHELLS`), and `GET /v1/setup` answers from a recent check instead of starting another one.

use std::path::PathBuf;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::report::login_shell;

static RUNNING: AtomicBool = AtomicBool::new(false);

/// Held while a login shell runs for any of these routes, so at most one runs at a time. It keeps
/// the last `glint doctor` answer and when it was taken.
static SHELLS: LazyLock<tokio::sync::Mutex<Option<(Instant, Value)>>> = LazyLock::new(|| tokio::sync::Mutex::new(None));

/// A check this recent is answered again instead of starting another shell.
const CHECK_FRESH: Duration = Duration::from_secs(5);

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
    let found = candidates.into_iter().find(|p| exact_file(p))?;
    Some(unquarantined(&found, &home).unwrap_or(found))
}

/// A file with exactly this name. macOS file systems ignore case, so inside Glint.app
/// `Contents/MacOS/glint` finds the window's own executable `Glint`, which must not run as the CLI.
fn exact_file(p: &std::path::Path) -> bool {
    let (Some(dir), Some(name)) = (p.parent(), p.file_name()) else { return false };
    p.is_file() && std::fs::read_dir(dir).is_ok_and(|mut d| d.any(|e| e.is_ok_and(|e| e.file_name() == name)))
}

/// A Glint.app downloaded from the internet carries macOS's quarantine flag on every file, and
/// macOS kills a quarantined command line tool as soon as it starts (exit 137, no output). The
/// app tries to clear the flag on its bundle, but macOS may not let it change files in
/// /Applications. So the CLI inside a bundle runs from a fresh copy in ~/.glint/cache: a new file
/// has no quarantine flag, and its signature is inside the binary.
fn unquarantined(cli: &std::path::Path, home: &std::path::Path) -> Option<PathBuf> {
    if !cfg!(target_os = "macos") || !cli.components().any(|c| c.as_os_str().to_string_lossy().ends_with(".app")) {
        return None;
    }
    let bytes = std::fs::read(cli).ok()?;
    let dir = home.join(".glint/cache");
    let copy = dir.join("glint");
    if std::fs::read(&copy).is_ok_and(|b| b == bytes) {
        return Some(copy);
    }
    std::fs::create_dir_all(&dir).ok()?;
    // Remove first: rewriting a file in place can keep macOS's cached signature of the old one.
    let _ = std::fs::remove_file(&copy);
    std::fs::write(&copy, &bytes).ok()?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&copy, std::fs::Permissions::from_mode(0o755)).ok()?;
    Some(copy)
}

/// Runs `glint <args> --json` in a login shell; returns the steps and everything it printed.
/// An interactive shell reads ~/.zshrc, where nvm and pyenv usually live; when that gives no
/// result (a profile that exits or starts another program), it tries a plain login shell.
async fn run(args: &str, timeout: Duration) -> Result<(Value, String), String> {
    let cli = cli().ok_or("the glint command line tool was not found next to glintd, in Glint.app or in ~/.glint/bin")?;
    let shell = login_shell();
    let line = format!("'{}' {args} --json 2>&1", cli.display());
    let mut problem = String::new();
    for flags in ["-lic", "-lc"] {
        match attempt(&shell, flags, &line, timeout).await {
            Ok(text) => match text.lines().find_map(|l| l.trim().strip_prefix("GLINT_JSON:")).and_then(|j| serde_json::from_str(j).ok()) {
                Some(steps) => {
                    let log = text.lines().filter(|l| !l.trim().starts_with("GLINT_JSON:")).collect::<Vec<_>>().join("\n");
                    return Ok((steps, log));
                }
                None if problem.is_empty() => problem = describe(&cli, args, &text),
                None => {}
            },
            Err(e) if problem.is_empty() => problem = e,
            Err(_) => {}
        }
    }
    Err(problem)
}

/// One run of `<shell> <flags> <line>`: its output, or why it gave none.
async fn attempt(shell: &str, flags: &str, line: &str, timeout: Duration) -> Result<String, String> {
    let child = tokio::process::Command::new(shell)
        .args([flags, line])
        .env("SHELL", shell)
        // The CLI may run from a copy in ~/.glint/cache (see `unquarantined`), where it cannot
        // find this glintd next to itself; `glint install` copies the one named here.
        .envs(std::env::current_exe().ok().map(|e| ("GLINT_DAEMON", e)))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("could not start {shell}: {e}"))?;
    let out = tokio::time::timeout(timeout, child.wait_with_output())
        .await
        .map_err(|_| format!("glint did not finish within {} s", timeout.as_secs()))?
        .map_err(|e| e.to_string())?;
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    let err = String::from_utf8_lossy(&out.stderr);
    let code = out.status.code();
    use std::os::unix::process::ExitStatusExt;
    if code == Some(137) || out.status.signal() == Some(9) {
        return Err(format!(
            "macOS stopped the glint tool as soon as it started. This happens when Glint.app still carries the download quarantine flag. Run this in Terminal, then press Check again:\nxattr -dr com.apple.quarantine /Applications/Glint.app\n{}",
            err.trim()
        ));
    }
    if !text.contains("GLINT_JSON:") {
        // Show the shell's own complaints (a broken profile, a missing command) and how it ended.
        let tail: Vec<&str> = err.lines().rev().take(10).collect();
        if !tail.is_empty() {
            text.push('\n');
            text.push_str(&tail.into_iter().rev().collect::<Vec<_>>().join("\n"));
        }
        text.push_str(&format!("\n({shell} {flags} ended with {})", code.map_or_else(|| format!("signal {}", out.status.signal().unwrap_or(0)), |c| format!("exit code {c}"))));
    }
    Ok(text)
}

fn describe(cli: &std::path::Path, args: &str, text: &str) -> String {
    let tail = text.trim().lines().rev().take(15).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n");
    format!("glint {args} gave no result ({}); it printed:\n{tail}", cli.display())
}

/// `GET /v1/setup` → `{steps: [{name, status: ok|skip|fail, detail}], shell}`.
pub async fn status() -> Response {
    let doctor = || async { run("doctor", Duration::from_secs(40)).await.map(|(steps, _)| json!({ "steps": steps, "shell": login_shell() })) };
    match checked(doctor).await {
        Ok(body) => Json(body).into_response(),
        Err(e) => crate::routes::error(StatusCode::SERVICE_UNAVAILABLE, e),
    }
}

/// Runs `doctor` unless a check finished within `CHECK_FRESH`. Requests that arrive while a check
/// (or setup, or a report) runs wait for it, and then share that answer.
async fn checked<F, Fut>(doctor: F) -> Result<Value, String>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<Value, String>>,
{
    let mut last = SHELLS.lock().await;
    if let Some((_, body)) = last.as_ref().filter(|(at, _)| at.elapsed() < CHECK_FRESH) {
        return Ok(body.clone());
    }
    let body = doctor().await?;
    *last = Some((Instant::now(), body.clone()));
    Ok(body)
}

/// `POST /v1/setup` → `{steps, log}`; the log also goes to ~/.glint/setup.log.
pub async fn install() -> Response {
    if RUNNING.swap(true, Ordering::SeqCst) {
        return crate::routes::error(StatusCode::CONFLICT, "setup is already running".into());
    }
    let result = {
        let mut last = SHELLS.lock().await;
        *last = None; // what was checked before is out of date now
        run("install", Duration::from_secs(180)).await
    };
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

/// `POST /v1/report` → `{path, complete}`: "Send diagnostics" in the window. Runs `glint report` in
/// the login shell; when the CLI cannot run (quarantine, a broken profile), writes what glintd can
/// see itself, with the reason. Either way the file is in ~/Downloads and shown in Finder.
pub async fn report() -> Response {
    let bundle = crate::report::own_bundle();
    let mut args = "report --no-reveal".to_owned();
    if let Some(b) = &bundle {
        args.push_str(&format!(" --app '{}'", b.display().to_string().replace('\'', "'\\''")));
    }
    let ran = {
        let _turn = SHELLS.lock().await;
        run(&args, Duration::from_secs(90)).await
    };
    let from_cli = match &ran {
        Ok((v, _)) => v.get("path").and_then(Value::as_str).map(PathBuf::from).filter(|p| p.is_file()),
        Err(_) => None,
    };
    let complete = from_cli.is_some();
    let why = match ran {
        Ok((v, log)) => format!("glint report gave no file: {v}\n{log}"),
        Err(e) => e,
    };
    let path = match from_cli {
        Some(p) => Ok(p),
        None => tokio::task::spawn_blocking(move || {
            let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
            let mut sections = crate::report::common(&home, bundle.as_deref(), "could not run (see below)", env!("CARGO_PKG_VERSION"));
            sections.insert(1, ("glint report".into(), format!("The glint command line tool could not write this report, so glintd wrote what it can see.\n{why}")));
            crate::report::save(&home, None, &crate::report::render(&sections))
        })
        .await
        .map_err(|e| std::io::Error::other(e.to_string()))
        .and_then(|r| r),
    };
    match path {
        Ok(p) => {
            let shown = p.clone();
            let _ = tokio::task::spawn_blocking(move || crate::report::reveal(&shown)).await;
            Json(json!({ "path": p.display().to_string(), "complete": complete })).into_response()
        }
        Err(e) => crate::routes::error(StatusCode::INTERNAL_SERVER_ERROR, format!("could not write the report: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test(flavor = "multi_thread")]
    async fn checks_at_once_share_one_shell() {
        let runs = Arc::new(AtomicUsize::new(0));
        let calls: Vec<_> = (0..12)
            .map(|_| {
                let runs = runs.clone();
                tokio::spawn(super::checked(move || async move {
                    runs.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                    Ok(serde_json::json!({ "steps": [] }))
                }))
            })
            .collect();
        for c in calls {
            assert!(c.await.unwrap().is_ok());
        }
        assert_eq!(runs.load(Ordering::SeqCst), 1, "twelve checks at once started more than one shell");
    }
    #[test]
    fn exact_file_ignores_a_name_that_only_matches_without_case() {
        let dir = std::env::temp_dir().join(format!("glint-exact-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Glint"), "").unwrap();
        assert!(super::exact_file(&dir.join("Glint")));
        assert!(!super::exact_file(&dir.join("glint")));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
