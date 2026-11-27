//! `glint conformance -- <cmd>`: checks a language SDK against `spec/conformance/`.

mod matcher;

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use glint_core::{Hub, HubConfig, Query};
use glint_store_sqlite::SqliteStore;
use serde_json::{Map, Value};
use tokio::net::TcpListener;
use tokio::process::Command;

const SCENARIOS: &str = include_str!("../../../../spec/conformance/scenarios.json");
const MARKER: &str = "x-conformance.begin";
const RUN_TIMEOUT: Duration = Duration::from_secs(60);
const OFFLINE_LIMIT: Duration = Duration::from_secs(10);

pub struct Args {
    pub command: Vec<String>,
    pub scenarios: Option<String>,
}

/// One named check and what went wrong with it.
struct Outcome {
    name: String,
    errors: Vec<String>,
}

pub fn run(args: &Args) -> Result<(), String> {
    let text = match &args.scenarios {
        Some(p) => std::fs::read_to_string(p).map_err(|e| format!("reading {p}: {e}"))?,
        None => SCENARIOS.to_owned(),
    };
    let spec: Value = serde_json::from_str(&text).map_err(|e| format!("scenarios: {e}"))?;
    let scenarios = spec["scenarios"].as_array().cloned().ok_or("scenarios: missing `scenarios`")?;
    let (program, rest) = args.command.split_first().ok_or("give the command that runs the conformance program after --")?;

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    let outcomes = rt.block_on(async {
        let (hub, url) = recorder().await?;
        let mut outcomes = Vec::new();

        // 1. Normal run.
        let started = Instant::now();
        let out = exec(program, rest, &[("GLINT_URL", url.as_str()), ("GLINT_ENABLED", "1")]).await?;
        let took = started.elapsed();
        let mut errors = Vec::new();
        if !out.status.success() {
            errors.push(format!("exited with {}; stderr:\n{}", out.status, indent(&out.stderr)));
        }
        outcomes.push(Outcome { name: format!("program runs ({:.1} s)", took.as_secs_f64()), errors });
        // POST answers after storing, but a flush on exit may still be in flight.
        tokio::time::sleep(Duration::from_millis(300)).await;
        let events = all_events(&hub).await?;
        outcomes.extend(evaluate(&scenarios, &events));

        // 2. Glint unreachable: the program must still finish, quickly and cleanly.
        let started = Instant::now();
        let out = exec(program, rest, &[("GLINT_URL", "http://127.0.0.1:9"), ("GLINT_ENABLED", "1")]).await?;
        let took = started.elapsed();
        let mut errors = Vec::new();
        if !out.status.success() {
            errors.push(format!("exited with {}; stderr:\n{}", out.status, indent(&out.stderr)));
        }
        if took > OFFLINE_LIMIT {
            errors.push(format!("took {:.1} s; must finish within {} s when Glint is down", took.as_secs_f64(), OFFLINE_LIMIT.as_secs()));
        }
        outcomes.push(Outcome { name: format!("unreachable server: finishes ({:.1} s)", took.as_secs_f64()), errors });

        // 3. Disabled: nothing may arrive.
        let before = all_events(&hub).await?.len();
        let out = exec(program, rest, &[("GLINT_URL", url.as_str()), ("GLINT_ENABLED", "0")]).await?;
        tokio::time::sleep(Duration::from_millis(300)).await;
        let after = all_events(&hub).await?.len();
        let mut errors = Vec::new();
        if !out.status.success() {
            errors.push(format!("exited with {}", out.status));
        }
        if after != before {
            errors.push(format!("{} events arrived with GLINT_ENABLED=0", after - before));
        }
        outcomes.push(Outcome { name: "GLINT_ENABLED=0: sends nothing".into(), errors });
        Ok::<_, String>(outcomes)
    })?;

    println!("glint conformance v{}: {}", spec["version"], args.command.join(" "));
    let mut failed = 0;
    for o in &outcomes {
        if o.errors.is_empty() {
            println!("  ✓ {}", o.name);
        } else {
            failed += 1;
            println!("  ✗ {}", o.name);
            for e in &o.errors {
                println!("      {}", e.replace('\n', "\n      "));
            }
        }
    }
    println!("{}/{} passed", outcomes.len() - failed, outcomes.len());
    if failed > 0 { Err(format!("{failed} conformance checks failed")) } else { Ok(()) }
}

async fn recorder() -> Result<(Hub, String), String> {
    let store = SqliteStore::open_in_memory().map_err(|e| e.to_string())?;
    let (hub, _writer) = Hub::start(Arc::new(store), HubConfig::default());
    let listener = TcpListener::bind("127.0.0.1:0").await.map_err(|e| e.to_string())?;
    let addr = listener.local_addr().map_err(|e| e.to_string())?;
    tokio::spawn(glint_server::serve(listener, hub.clone(), glint_server::Options::default(), std::future::pending()));
    // Plays the human for the `pause` scenario: continues each pause with {"ok": true}.
    let answering = hub.clone();
    let mut rx = hub.subscribe();
    tokio::spawn(async move {
        while let Ok(e) = rx.recv().await {
            if e.kind == "ctl.pause" {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                let ok = glint_core::Resolution::Continue { value: Some(serde_json::json!({ "ok": true })) };
                let _ = answering.resume(&e.id, ok, "conformance").await;
            }
        }
    });
    Ok((hub, format!("http://{addr}")))
}

async fn exec(program: &str, args: &[String], env: &[(&str, &str)]) -> Result<std::process::Output, String> {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .env_remove("NODE_ENV")
        .env_remove("APP_ENV")
        .env_remove("CI") // SDKs skip pauses on CI
        .env("GLINT_FORCE_PAUSE", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let child = cmd.spawn().map_err(|e| format!("starting `{program}`: {e}"))?;
    match tokio::time::timeout(RUN_TIMEOUT, child.wait_with_output()).await {
        Ok(out) => out.map_err(|e| e.to_string()),
        Err(_) => Err(format!("`{program}` did not finish within {} s", RUN_TIMEOUT.as_secs())),
    }
}

async fn all_events(hub: &Hub) -> Result<Vec<Value>, String> {
    let q = Query { limit: Some(Query::MAX_LIMIT), ..Query::default() };
    let events = hub.query(q).await.map_err(|e| e.to_string())?;
    events.iter().map(|e| serde_json::to_value(e).map_err(|e| e.to_string())).collect()
}

fn evaluate(scenarios: &[Value], events: &[Value]) -> Vec<Outcome> {
    let mut out = vec![global_checks(events)];

    // Fold updates into their targets, then split by marker.
    let mut folded: Vec<Value> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for e in events {
        if e["kind"] == "core.update" {
            let target = e["payload"]["target"].as_str().unwrap_or_default();
            if let Some(&i) = index.get(target) {
                apply_update(&mut folded[i], &e["payload"]);
            }
            continue;
        }
        if let Some(id) = e["id"].as_str() {
            index.insert(id.to_owned(), folded.len());
        }
        folded.push(e.clone());
    }
    let mut groups: HashMap<String, Vec<Value>> = HashMap::new();
    let mut current: Option<String> = None;
    let mut stray = 0;
    for e in folded {
        if e["kind"] == MARKER {
            let name = e["payload"]["scenario"].as_str().unwrap_or("?").to_owned();
            groups.entry(name.clone()).or_default();
            current = Some(name);
            continue;
        }
        match &current {
            Some(name) => groups.entry(name.clone()).or_default().push(e),
            None => stray += 1,
        }
    }
    if stray > 0 {
        out[0].errors.push(format!("{stray} events arrived before the first {MARKER} marker"));
    }

    for s in scenarios {
        let name = s["name"].as_str().unwrap_or("?").to_owned();
        let expect = s["expect"].as_array().cloned().unwrap_or_default();
        let mut errors = Vec::new();
        match groups.get(&name) {
            None => errors.push(format!("no {MARKER} marker for this scenario (or it never arrived)")),
            Some(got) => {
                if got.len() != expect.len() {
                    errors.push(format!(
                        "expected {} events, got {}: {}",
                        expect.len(),
                        got.len(),
                        got.iter().map(|e| e["kind"].as_str().unwrap_or("?")).collect::<Vec<_>>().join(", ")
                    ));
                }
                for (i, (exp, act)) in expect.iter().zip(got).enumerate() {
                    matcher::check(exp, act, &format!("event {i}"), &mut errors);
                }
                for pair in s["check_parents"].as_array().into_iter().flatten() {
                    let (c, p) = (pair[0].as_u64().unwrap_or(0) as usize, pair[1].as_u64().unwrap_or(0) as usize);
                    let (child, parent) = (got.get(c), got.get(p));
                    let ok = matches!((child, parent), (Some(ch), Some(pa)) if pa["id"].is_string() && ch["parent"] == pa["id"]);
                    if !ok {
                        errors.push(format!("event {c}.parent: expected the id of event {p}, got {}", child.map(|ch| ch["parent"].to_string()).unwrap_or_else(|| "nothing".into())));
                    }
                }
                if s["check_origin_marker"] == true
                    && let Some(e) = got.first()
                {
                    check_origin_marker(e, &mut errors);
                }
            }
        }
        out.push(Outcome { name: format!("scenario {name}"), errors });
    }
    out
}

fn apply_update(target: &mut Value, update: &Value) {
    if let Some(meta) = update["meta"].as_object() {
        if !target["meta"].is_object() {
            target["meta"] = Value::Object(Map::new());
        }
        for (k, v) in meta {
            target["meta"][k] = v.clone();
        }
    }
    for k in ["hidden", "removed"] {
        if let Some(v) = update.get(k) {
            target[k] = v.clone();
        }
    }
}

fn global_checks(events: &[Value]) -> Outcome {
    let mut errors = Vec::new();
    if events.is_empty() {
        errors.push("no events arrived at all".into());
    }
    let runs: HashSet<&str> = events.iter().filter_map(|e| e["run"].as_str()).collect();
    if runs.len() > 1 {
        errors.push(format!("events use {} different runs; one process must use one run", runs.len()));
    }
    let ids: HashSet<&str> = events.iter().filter_map(|e| e["id"].as_str()).collect();
    let mut bad_origin = 0;
    let mut bad_target = 0;
    for e in events {
        let o = &e["origin"];
        let sdk_ok = o["sdk"].as_str().is_some_and(sdk_format_ok);
        if o["lang"].as_str().is_none_or(str::is_empty) || !sdk_ok {
            bad_origin += 1;
        }
        if e["kind"] == "core.update" && !e["payload"]["target"].as_str().is_some_and(|t| ids.contains(t)) {
            bad_target += 1;
        }
    }
    if bad_origin > 0 {
        let sample = events
            .iter()
            .find(|e| !e["origin"]["sdk"].as_str().is_some_and(sdk_format_ok) || e["origin"]["lang"].as_str().is_none_or(str::is_empty))
            .map(|e| e["origin"].to_string())
            .unwrap_or_default();
        errors.push(format!("{bad_origin} events lack origin.lang or a `<name>/<semver>` origin.sdk, e.g. {sample}"));
    }
    if bad_target > 0 {
        errors.push(format!("{bad_target} core.update events target ids that were never sent"));
    }
    Outcome { name: "every event: one run, origin.lang, origin.sdk, valid update targets".into(), errors }
}

/// `<name>/<major>.<minor>.<patch>[…]`
fn sdk_format_ok(s: &str) -> bool {
    let Some((name, version)) = s.rsplit_once('/') else {
        return false;
    };
    let name_ok = !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || "@._/-".contains(c));
    let mut parts = version.splitn(3, '.');
    let num = |p: Option<&str>| p.is_some_and(|p| p.chars().take_while(char::is_ascii_digit).count() > 0);
    name_ok && num(parts.next()) && num(parts.next()) && num(parts.next())
}

fn check_origin_marker(e: &Value, errors: &mut Vec<String>) {
    let (Some(file), Some(line)) = (e["origin"]["file"].as_str(), e["origin"]["line"].as_u64()) else {
        errors.push("origin.file / origin.line missing".into());
        return;
    };
    let path = Path::new(file);
    let Ok(text) = std::fs::read_to_string(path) else {
        errors.push(format!("origin.file {file} cannot be read (is it absolute?)"));
        return;
    };
    let src = text.lines().nth(usize::try_from(line).unwrap_or(usize::MAX).saturating_sub(1)).unwrap_or("");
    if !src.contains("glint:origin") {
        errors.push(format!("{file}:{line} is `{}`, not the call marked glint:origin", src.trim()));
    }
}

fn indent(bytes: &[u8]) -> String {
    let s = String::from_utf8_lossy(bytes);
    let tail: Vec<&str> = s.lines().rev().take(15).collect();
    tail.into_iter().rev().map(|l| format!("  {l}")).collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sdk_format() {
        assert!(sdk_format_ok("glint-js/0.1.0"));
        assert!(sdk_format_ok("@glint/sdk/1.2.3-beta"));
        assert!(!sdk_format_ok("glint-js"));
        assert!(!sdk_format_ok("glint-js/1.2"));
    }

    #[test]
    fn embedded_scenarios_parse() {
        let v: Value = serde_json::from_str(SCENARIOS).unwrap();
        assert!(v["scenarios"].as_array().unwrap().len() >= 16);
    }
}
