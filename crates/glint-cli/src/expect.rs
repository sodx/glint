//! `glint expect`: a test from what programs did.
//!
//! ```text
//! glint expect 'label:cart payload.value.total>0' 'label:"order saved"' -- php artisan app:checkout 7
//! glint expect --timeout 60 'kind:sql.query text:"insert into orders"'      # wait while you click
//! ```
//!
//! With a command: notes where the stream is, runs the command, then waits for the expectations
//! among the events after that point. Without one: events of the last `--lookback` seconds count,
//! and it waits for the rest. Exit 0 PASS, 1 FAIL, 2 when it could not check.

use std::process::Command;
use std::time::Duration;

use serde_json::{Value, json};

pub struct Args {
    pub expect: Vec<String>,
    pub project: Option<String>,
    pub all_projects: bool,
    pub unordered: bool,
    pub allow_errors: bool,
    pub timeout: u64,
    pub lookback: u64,
    pub command: Vec<String>,
}

/// Ok(true) PASS, Ok(false) FAIL, Err: could not check.
pub fn run(base: &str, a: &Args) -> Result<bool, String> {
    let base = base.trim_end_matches('/');
    let http = crate::client::builder().timeout_connect(Duration::from_secs(1)).build();
    let project = if a.all_projects {
        None
    } else {
        a.project.clone().or_else(|| {
            let cwd = std::env::current_dir().ok()?.display().to_string();
            glint_core::project::resolve(&cwd).map(|r| r.project).or_else(|| glint_core::project::folder_name(&cwd))
        })
    };
    let mut body = json!({
        "expect": a.expect,
        "ordered": !a.unordered,
        "no_errors": !a.allow_errors,
        "project": project,
        "timeout_s": a.timeout,
        "lookback_s": a.lookback,
    });
    if !a.command.is_empty() {
        // Where the stream is now: only what happens from here on counts.
        let newest: Value = http
            .get(&format!("{base}/v1/events?limit=1"))
            .call()
            .map_err(|e| format!("glintd is not reachable at {base}: {e}"))?
            .into_json()
            .map_err(|e| e.to_string())?;
        match newest["events"][0]["id"].as_str() {
            Some(id) => body["after"] = json!(id),
            None => body["lookback_s"] = json!(0),
        }
        let status = Command::new(&a.command[0]).args(&a.command[1..]).status().map_err(|e| format!("cannot run `{}`: {e}", a.command[0]))?;
        if !status.success() {
            eprintln!("glint: `{}` exited with {status}; checking what it did anyway", a.command.join(" "));
        }
    }
    let res = http
        .post(&format!("{base}/v1/expect"))
        .timeout(Duration::from_secs(a.timeout + 10))
        .send_json(body)
        .map_err(|e| match e {
            ureq::Error::Status(_, r) => r.into_json::<Value>().ok().and_then(|v| v["error"].as_str().map(str::to_owned)).unwrap_or_else(|| "bad request".into()),
            other => format!("glintd is not reachable at {base}: {other}"),
        })?;
    let v: Value = res.into_json().map_err(|e| e.to_string())?;
    print!("{}", v["report"].as_str().unwrap_or(""));
    if let Some(p) = &project {
        println!("  (project {p})");
    }
    Ok(v["pass"] == true)
}
