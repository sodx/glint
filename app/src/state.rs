//! What the app remembers between runs, in `~/.glint/app-state.json`:
//!
//! - `seen_failure`: the id of the newest failure the user has seen (the window was in front
//!   after it arrived). On start, a newer failure turns the spark red again, so a failure nobody
//!   looked at stays red across a restart or an update.
//! - `open_at_login`: false once "Open at login" was turned off in the menu; `glint install`
//!   then leaves the login item off.
//!
//! Also the pure parts of the watcher: which events count as failures (not the sample run of
//! "Show me an example"), and whether a failure found at start is one the user has not seen.

use std::path::PathBuf;

use serde_json::Value;

pub fn glint_dir() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default().join(".glint")
}

fn path() -> PathBuf {
    glint_dir().join("app-state.json")
}

pub fn get(key: &str) -> Value {
    std::fs::read_to_string(path()).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).map(|v| v[key].clone()).unwrap_or(Value::Null)
}

/// Sets one key and keeps the others (the file may gain keys from other versions).
pub fn set(key: &str, value: Value) {
    let mut all = std::fs::read_to_string(path()).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).filter(Value::is_object).unwrap_or_else(|| serde_json::json!({}));
    all[key] = value;
    let _ = std::fs::create_dir_all(glint_dir());
    let tmp = glint_dir().join(format!(".app-state.json.{}", std::process::id()));
    if std::fs::write(&tmp, serde_json::to_string_pretty(&all).unwrap_or_default() + "\n").is_ok() {
        let _ = std::fs::rename(&tmp, path());
    }
}

/// The project of the sample run "Show me an example" sends from the window.
pub const EXAMPLE_PROJECT: &str = "example";

/// A failure worth the red spark: not an update or a resume (which carry level error for other
/// reasons) and not the sample run.
pub fn counts(e: &Value) -> bool {
    !matches!(e["kind"].as_str(), Some("core.update" | "ctl.resume")) && e["project"].as_str() != Some(EXAMPLE_PROJECT) && !e["run"].as_str().is_some_and(|r| r.starts_with("example-"))
}

/// The newest failure that counts, of a list sorted oldest first.
pub fn newest_failure(list: &[Value]) -> Option<&Value> {
    list.iter().rev().find(|e| counts(e))
}

/// At start: is the newest failure one the user has not seen? Ids sort by time. With nothing
/// remembered yet (the first start of this version), what is there counts as seen.
pub fn unseen_at_start(newest: &str, seen: Option<&str>) -> bool {
    seen.is_some_and(|s| newest > s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_example_run_is_not_a_failure() {
        let list = vec![
            json!({ "id": "01A", "kind": "core.exception", "project": "shop", "run": "php-1" }),
            json!({ "id": "01B", "kind": "core.exception", "project": "example", "run": "example-x1" }),
            json!({ "id": "01C", "kind": "core.update", "project": "shop" }),
        ];
        assert_eq!(newest_failure(&list).unwrap()["id"], "01A");
        assert!(newest_failure(&list[1..]).is_none());
        assert!(!counts(&json!({ "kind": "core.exception", "run": "example-x2" })), "named by its run alone");
    }

    #[test]
    fn a_failure_newer_than_the_one_seen_stays_red_after_a_restart() {
        assert!(unseen_at_start("01M8GM", Some("01M8FQ")));
        assert!(!unseen_at_start("01M8FQ", Some("01M8FQ")));
        assert!(!unseen_at_start("01M8FQ", Some("01M8GM")), "the seen one was pruned; an older one is not new");
        assert!(!unseen_at_start("01M8GM", None), "first start of this version: nothing to compare with");
    }

    #[test]
    fn set_keeps_the_other_keys() {
        let home = std::env::temp_dir().join(format!("glint-app-state-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        // SAFETY: this is the only test in the app that reads or changes HOME.
        unsafe { std::env::set_var("HOME", &home) };
        assert_eq!(get("seen_failure"), Value::Null);
        set("open_at_login", json!(false));
        set("seen_failure", json!("01A"));
        assert_eq!(get("open_at_login"), json!(false));
        assert_eq!(get("seen_failure"), json!("01A"));
        let _ = std::fs::remove_dir_all(&home);
    }
}
