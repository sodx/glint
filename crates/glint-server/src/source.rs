//! Source code next to events: `GET /v1/source` returns the lines around a call site or stack
//! frame, and `POST /v1/open` opens it in the editor. Both take an event id (and a frame index),
//! never a path: glintd only reads or opens files that an event on this machine named.

use std::path::{Path, PathBuf};
use std::process::Command;

use axum::Json;
use axum::extract::{Query as QueryParams, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use glint_core::Hub;
use glint_protocol::Envelope;
use serde_json::{Value, json};

const CONTEXT: usize = 6;

static DEFAULT_EDITOR: std::sync::RwLock<Option<String>> = std::sync::RwLock::new(None);

/// The editor from glintd's settings, used when a request names none. Settings change it live.
pub fn set_default_editor(editor: Option<String>) {
    *DEFAULT_EDITOR.write().unwrap_or_else(|p| p.into_inner()) = editor.filter(|e| !e.is_empty());
}

fn configured_editor() -> Option<String> {
    DEFAULT_EDITOR.read().unwrap_or_else(|p| p.into_inner()).clone().or_else(|| std::env::var("GLINT_EDITOR").ok())
}
const MAX_FILE: u64 = 2 * 1024 * 1024;

#[derive(serde::Deserialize)]
pub struct Where {
    event: String,
    /// Index into `payload.frames` (exceptions, traces); the event's own call site when absent.
    frame: Option<usize>,
    /// For `/v1/open`: vscode, cursor, windsurf, zed, phpstorm, idea, webstorm, pycharm, sublime.
    editor: Option<String>,
}

fn err(code: StatusCode, msg: &str) -> Response {
    (code, Json(json!({ "error": msg }))).into_response()
}

async fn locate(hub: &Hub, w: &Where) -> Result<(PathBuf, u32), (StatusCode, String)> {
    let nf = |m: &str| (StatusCode::NOT_FOUND, m.to_owned());
    let e: Envelope = match hub.get(w.event.clone()).await {
        Ok(Some(e)) => e,
        Ok(None) => return Err(nf("no such event")),
        Err(e) => return Err((StatusCode::INTERNAL_SERVER_ERROR, e.to_string())),
    };
    let (file, line) = match w.frame {
        Some(i) => {
            let f = e.payload.get("frames").and_then(Value::as_array).and_then(|a| a.get(i));
            (f.and_then(|f| f.get("file")).and_then(Value::as_str).map(str::to_owned), f.and_then(|f| f.get("line")).and_then(Value::as_u64))
        }
        None => (e.origin.file.clone(), e.origin.line.map(u64::from)),
    };
    let Some(file) = file else { return Err(nf("the event names no file")) };
    let path = PathBuf::from(&file);
    if !path.is_absolute() || !path.is_file() {
        return Err(nf("the file is not on this machine"));
    }
    Ok((path, line.and_then(|l| u32::try_from(l).ok()).unwrap_or(1).max(1)))
}

/// `GET /v1/source?event=&frame=` → `{file, line, start, lines}`.
pub async fn source(State(hub): State<Hub>, QueryParams(w): QueryParams<Where>) -> Response {
    let (path, line) = match locate(&hub, &w).await {
        Ok(x) => x,
        Err((code, msg)) => return err(code, &msg),
    };
    if std::fs::metadata(&path).map(|m| m.len()).unwrap_or(u64::MAX) > MAX_FILE {
        return err(StatusCode::PAYLOAD_TOO_LARGE, "file too large");
    }
    let Ok(text) = std::fs::read(&path) else { return err(StatusCode::NOT_FOUND, "cannot read the file") };
    if text.contains(&0) {
        return err(StatusCode::UNSUPPORTED_MEDIA_TYPE, "not a text file");
    }
    let text = String::from_utf8_lossy(&text);
    let all: Vec<&str> = text.lines().collect();
    let at = (line as usize).min(all.len().max(1));
    let start = at.saturating_sub(CONTEXT).max(1);
    let end = (at + CONTEXT).min(all.len());
    let lines: Vec<String> = all.get(start - 1..end).unwrap_or(&[]).iter().map(|l| l.chars().take(400).collect()).collect();
    Json(json!({ "file": path, "line": at, "start": start, "lines": lines })).into_response()
}

/// `POST /v1/open?event=&frame=&editor=` → opens the file at its line in the editor.
pub async fn open(State(hub): State<Hub>, QueryParams(w): QueryParams<Where>) -> Response {
    let (path, line) = match locate(&hub, &w).await {
        Ok(x) => x,
        Err((code, msg)) => return err(code, &msg),
    };
    let editor = w.editor.clone().or_else(configured_editor).or_else(|| installed().into_iter().next()).unwrap_or_else(|| "vscode".into());
    let Some(url) = editor_url(&editor, &path, line) else { return err(StatusCode::BAD_REQUEST, "unknown editor") };
    let opener = if cfg!(target_os = "macos") { "open" } else if cfg!(windows) { "explorer" } else { "xdg-open" };
    match Command::new(opener).arg(&url).spawn() {
        Ok(_) => Json(json!({ "opened": path, "line": line, "editor": editor })).into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, &format!("{opener}: {e}")),
    }
}

/// `GET /v1/editors` → the editors found here, the default first.
pub async fn editors() -> Json<Value> {
    let mut found = installed();
    if let Some(e) = configured_editor() {
        found.retain(|x| x != &e);
        found.insert(0, e);
    }
    Json(json!({ "editors": found }))
}

const EDITORS: &[(&str, &str)] = &[
    ("vscode", "Visual Studio Code"),
    ("cursor", "Cursor"),
    ("phpstorm", "PhpStorm"),
    ("zed", "Zed"),
    ("windsurf", "Windsurf"),
    ("webstorm", "WebStorm"),
    ("pycharm", "PyCharm"),
    ("idea", "IntelliJ IDEA"),
    ("sublime", "Sublime Text"),
];

/// Editors with an app bundle (macOS) or a command on PATH (elsewhere), in EDITORS order.
pub fn installed() -> Vec<String> {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    EDITORS
        .iter()
        .filter(|(id, app)| {
            if cfg!(target_os = "macos") {
                [PathBuf::from("/Applications"), home.join("Applications")].iter().any(|d| d.join(format!("{app}.app")).exists())
            } else {
                let cmd = match *id {
                    "vscode" => "code",
                    "sublime" => "subl",
                    other => other,
                };
                std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(cmd).exists()))
            }
        })
        .map(|(id, _)| (*id).to_owned())
        .collect()
}

fn editor_url(editor: &str, path: &Path, line: u32) -> Option<String> {
    let p = path.to_string_lossy();
    let enc = |s: &str| s.replace('%', "%25").replace(' ', "%20").replace('#', "%23").replace('?', "%3F").replace('&', "%26");
    Some(match editor {
        "vscode" | "cursor" | "windsurf" | "zed" => {
            let scheme = editor;
            let slash = if p.starts_with('/') { "" } else { "/" };
            format!("{scheme}://file{slash}{}:{line}", enc(&p))
        }
        "phpstorm" | "idea" | "webstorm" | "pycharm" => format!("{editor}://open?file={}&line={line}", enc(&p)),
        "sublime" => format!("subl://open?url=file://{}&line={line}", enc(&p)),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editor_urls() {
        let p = Path::new("/src/my shop/a#b.php");
        assert_eq!(editor_url("vscode", p, 7).unwrap(), "vscode://file/src/my%20shop/a%23b.php:7");
        assert_eq!(editor_url("phpstorm", p, 7).unwrap(), "phpstorm://open?file=/src/my%20shop/a%23b.php&line=7");
        assert!(editor_url("notepad", p, 1).is_none());
    }
}
