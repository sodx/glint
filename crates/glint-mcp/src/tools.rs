//! The tools. Each returns plain text written for an agent: compact first, details on request.

use std::time::Duration;

use glint_core::{Hub, Query, Resolution};
use glint_protocol::{IncomingEnvelope, Level, Meta, Origin, is_valid_kind};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::broadcast::error::RecvError;

use crate::project::{clip, frame, line, summary};

const EVENTS_DEFAULT: usize = 30;
const EVENTS_MAX: usize = 200;
const EVENT_CHARS_DEFAULT: usize = 6000;
const WAIT_DEFAULT_S: u64 = 30;
const WAIT_MAX_S: u64 = 120;
const WAIT_LOOKBACK_DEFAULT_S: u64 = 10;
const WAIT_LOOKBACK_MAX_S: u64 = 3600;
const PROJECT_HELP: &str = "Project to read. Default: the project this agent works in (every reply names it); \"*\" for all projects.";

/// What the connection knows about the calling agent.
#[derive(Debug, Clone, Default)]
pub struct Ctx {
    /// The project the agent works in, when known.
    pub project: Option<String>,
    /// How that is known, for the reply's first line.
    pub source: Option<String>,
}

/// Which events a call reads.
enum Scope {
    One { project: String, why: String },
    All { why: String },
    /// Several projects have events and nothing says which one the agent means: the text says
    /// so and lists them, instead of mixing their events.
    Unknown(String),
}

impl Scope {
    fn project(&self) -> Option<String> {
        match self {
            Scope::One { project, .. } => Some(project.clone()),
            _ => None,
        }
    }

    /// The first line of a reply: which project it covers and why.
    fn head(&self) -> String {
        match self {
            Scope::One { project, why } => format!("Project: {project} ({why})\n"),
            Scope::All { why } => format!("Project: all ({why})\n"),
            Scope::Unknown(_) => String::new(),
        }
    }
}

/// The project a call reads: its `project` argument, else the agent's own, else the only project
/// with events. With several and no way to tell, [`Scope::Unknown`].
async fn scope(hub: &Hub, arg: Option<String>, ctx: &Ctx) -> Result<Scope, String> {
    match arg.as_deref().map(str::trim).filter(|p| !p.is_empty()) {
        Some("*" | "all") => return Ok(Scope::All { why: "as asked".into() }),
        Some(p) => return Ok(Scope::One { project: p.to_owned(), why: "as asked".into() }),
        None => {}
    }
    if let Some(p) = &ctx.project {
        return Ok(Scope::One { project: p.clone(), why: ctx.source.clone().unwrap_or_else(|| "this agent's project".into()) });
    }
    let projects = recent_projects(hub).await?;
    Ok(match projects.as_slice() {
        [] => Scope::All { why: "no events recorded yet".into() },
        [only] => Scope::One { project: only.name.clone(), why: "the only project with events; the agent did not name one".into() },
        many => {
            let mut t = String::from(
                "Glint does not know which project this agent works in (the connection did not say), and it has events \
                 from several. Call again with project=<name> (the session's Glint note names it), or project=\"*\" \
                 for all of them.\nRecent projects (last activity · runs · errors):\n",
            );
            for p in many {
                t.push_str(&format!("- {} · {} · {} · {}\n", p.name, p.last.get(..19).unwrap_or(&p.last), p.runs, p.errors));
            }
            Scope::Unknown(t)
        }
    })
}

struct ProjectSeen {
    name: String,
    last: String,
    runs: usize,
    errors: u64,
}

/// Projects of the most recent runs, newest first, at most ten.
async fn recent_projects(hub: &Hub) -> Result<Vec<ProjectSeen>, String> {
    let runs = hub.runs(None, 200).await.map_err(store_err)?;
    let mut out: Vec<ProjectSeen> = Vec::new();
    for r in runs {
        if r.run == "mcp" {
            continue; // what agents showed, not a program
        }
        if let Some(p) = out.iter_mut().find(|p| p.name == r.project) {
            p.runs += 1;
            p.errors += r.errors;
        } else if out.len() < 10 {
            out.push(ProjectSeen { name: r.project, last: r.last, runs: 1, errors: r.errors });
        }
    }
    Ok(out)
}

pub fn definitions() -> Value {
    let filter = json!({
        "project": { "type": "string", "description": PROJECT_HELP },
        "run": { "type": "string", "description": "Only this run (one process / request / test)." },
        "kind": { "type": "string", "description": "Exact kind, or a prefix ending in * (e.g. sql.*, core.exception)." },
        "level": { "type": "string", "enum": ["trace", "debug", "info", "warn", "error", "fatal"], "description": "Minimum level." },
        "q": { "type": "string", "description": "Full-text search over the event's text." }
    });
    let with = |extra: Value| {
        let mut props = filter.clone();
        if let (Some(p), Some(e)) = (props.as_object_mut(), extra.as_object()) {
            for (k, v) in e {
                p.insert(k.clone(), v.clone());
            }
        }
        props
    };
    json!([
        {
            "name": "glint_runs",
            "title": "Recent runs",
            "description": "List recently active runs (processes, requests, tests, agent sessions), newest first, with event and error counts.",
            "inputSchema": { "type": "object", "properties": {
                "project": { "type": "string", "description": PROJECT_HELP },
                "limit": { "type": "integer", "minimum": 1, "maximum": 100, "default": 15 }
            }},
            "annotations": { "readOnlyHint": true }
        },
        {
            "name": "glint_events",
            "title": "Query events",
            "description": "Recent events matching the filters, oldest first, one line each (id, time, kind, level, label, file:line, summary). Page forward with `after` = the last id you saw, backward with `before`.",
            "inputSchema": { "type": "object", "properties": with(json!({
                "after": { "type": "string", "description": "Only events after this event id." },
                "before": { "type": "string", "description": "Only events before this event id." },
                "limit": { "type": "integer", "minimum": 1, "maximum": EVENTS_MAX, "default": EVENTS_DEFAULT }
            }))},
            "annotations": { "readOnlyHint": true }
        },
        {
            "name": "glint_event",
            "title": "One event in full",
            "description": "The full JSON of one event (payload, origin, meta), truncated to max_chars.",
            "inputSchema": { "type": "object", "required": ["id"], "properties": {
                "id": { "type": "string" },
                "max_chars": { "type": "integer", "minimum": 200, "maximum": 100000, "default": EVENT_CHARS_DEFAULT }
            }},
            "annotations": { "readOnlyHint": true }
        },
        {
            "name": "glint_last_error",
            "title": "Last error",
            "description": "The most recent error-level event (exception, error log, failed job…) with its stack and the events that led up to it in the same run.",
            "inputSchema": { "type": "object", "properties": {
                "project": { "type": "string", "description": PROJECT_HELP },
                "context": { "type": "integer", "minimum": 0, "maximum": 100, "default": 20, "description": "How many earlier events of the same run to include." }
            }},
            "annotations": { "readOnlyHint": true }
        },
        {
            "name": "glint_wait_for",
            "title": "Wait for an event",
            "description": "Return the newest event matching the filters from the last lookback_s seconds (so a program you just ran counts), else block until one arrives (e.g. after you changed code and the user or a watcher reruns it) or the timeout passes. Says which window it searched. lookback_s=0 waits only for new events; after=<id> searches everything after that event instead.",
            "inputSchema": { "type": "object", "properties": with(json!({
                "after": { "type": "string", "description": "Search stored events after this event id instead of the lookback window." },
                "lookback_s": { "type": "integer", "minimum": 0, "maximum": WAIT_LOOKBACK_MAX_S, "default": WAIT_LOOKBACK_DEFAULT_S, "description": "Also count events of the last N seconds before the call." },
                "timeout_s": { "type": "integer", "minimum": 1, "maximum": WAIT_MAX_S, "default": WAIT_DEFAULT_S }
            }))},
            "annotations": { "readOnlyHint": true }
        },
        {
            "name": "glint_expect",
            "title": "Check what a program did",
            "description": "Turn glint() markers into a test: wait until events matching each expectation have arrived (in order unless ordered=false) and no program failed meanwhile, then report PASS or FAIL with the matching event ids. Run the program first and call this (events of the last lookback_s seconds count), or call it and have the user click through. One expectation is space-separated terms that must all hold for one event: label:\"order saved\", kind:sql.query or kind:sql.*, text:refund (payload contains), a comparison on a dot path like payload.value.total>0 or payload.value.paid=true (= != > >= < <=; values are JSON), has:payload.value.email, count>=2.",
            "inputSchema": { "type": "object", "required": ["expect"], "properties": {
                "expect": { "type": "array", "items": { "type": "string" }, "description": "Expectations, e.g. [\"label:cart payload.value.total>0\", \"kind:sql.query text:insert\", \"label:\\\"order saved\\\"\"]." },
                "project": { "type": "string", "description": PROJECT_HELP },
                "run": { "type": "string" },
                "ordered": { "type": "boolean", "default": true },
                "no_errors": { "type": "boolean", "default": true, "description": "Fail as soon as a program error arrives." },
                "after": { "type": "string", "description": "Only events after this event id." },
                "lookback_s": { "type": "integer", "default": 30, "description": "Without after: events of the last N seconds count too." },
                "timeout_s": { "type": "integer", "default": 30, "maximum": 120 }
            }},
            "annotations": { "readOnlyHint": true }
        },
        {
            "name": "glint_pauses",
            "title": "Paused programs",
            "description": "Programs currently paused at a glint pause() call, waiting for someone to resume them.",
            "inputSchema": { "type": "object", "properties": {} },
            "annotations": { "readOnlyHint": true }
        },
        {
            "name": "glint_resume",
            "title": "Resume a paused program",
            "description": "Let a paused program continue, optionally handing it a JSON value (SDKs return it from pause()), or ask it to stop.",
            "inputSchema": { "type": "object", "required": ["id"], "properties": {
                "id": { "type": "string", "description": "Pause id from glint_pauses." },
                "value": { "description": "Any JSON value to return from pause()." },
                "stop": { "type": "boolean", "default": false, "description": "Ask the program to stop instead of continuing." }
            }},
            "annotations": { "readOnlyHint": false, "destructiveHint": false }
        },
        {
            "name": "glint_annotate",
            "title": "Annotate an event",
            "description": "Attach a short note to an event; the human sees it next to the event in Glint (e.g. 'root cause: missing null check').",
            "inputSchema": { "type": "object", "required": ["id", "note"], "properties": {
                "id": { "type": "string" },
                "note": { "type": "string", "maxLength": 2000 },
                "color": { "type": "string", "enum": ["gray", "green", "orange", "red", "blue", "purple"] }
            }},
            "annotations": { "readOnlyHint": false, "destructiveHint": false }
        },
        {
            "name": "glint_show",
            "title": "Show something to the human",
            "description": "Put markdown, a mermaid diagram, text or JSON into the human's Glint feed (findings, a plan, a diagram of a call flow).",
            "inputSchema": { "type": "object", "required": ["format", "content"], "properties": {
                "format": { "type": "string", "enum": ["markdown", "mermaid", "text", "json"] },
                "content": { "type": "string" },
                "label": { "type": "string" },
                "project": { "type": "string", "description": "Project to show it in. Default: the agent's own." }
            }},
            "annotations": { "readOnlyHint": false, "destructiveHint": false }
        },
        {
            "name": "glint_llm_calls",
            "title": "LLM calls",
            "description": "Recent LLM calls (from OpenTelemetry GenAI spans or SDKs): model, tokens, latency, cost, last user message and answer, plus totals. Use glint_event for a full transcript.",
            "inputSchema": { "type": "object", "properties": {
                "project": { "type": "string", "description": PROJECT_HELP },
                "model": { "type": "string", "description": "Substring of the model name." },
                "q": { "type": "string", "description": "Full-text search over prompts and answers." },
                "limit": { "type": "integer", "minimum": 1, "maximum": 100, "default": 20 }
            }},
            "annotations": { "readOnlyHint": true }
        },
        {
            "name": "glint_clear",
            "title": "Start a fresh view",
            "description": "Clear the human's live view (stored events are kept) and get a cursor: pass it as `after` to glint_events to see only what happens next.",
            "inputSchema": { "type": "object", "properties": { "project": { "type": "string", "description": "Project whose view to clear. Default: the agent's own." } } },
            "annotations": { "readOnlyHint": false, "destructiveHint": false }
        }
    ])
}

#[derive(Deserialize, Default)]
struct Filter {
    project: Option<String>,
    run: Option<String>,
    kind: Option<String>,
    level: Option<Level>,
    q: Option<String>,
    after: Option<String>,
    before: Option<String>,
    limit: Option<usize>,
}

impl Filter {
    fn query(self, default_limit: usize, max: usize) -> Query {
        Query {
            project: self.project.filter(|s| !s.is_empty()),
            run: self.run.filter(|s| !s.is_empty()),
            kind: self.kind.filter(|s| !s.is_empty()),
            level: self.level,
            q: self.q.filter(|s| !s.is_empty()),
            after: self.after.filter(|s| !s.is_empty()),
            before: self.before.filter(|s| !s.is_empty()),
            limit: Some(self.limit.unwrap_or(default_limit).clamp(1, max)),
        }
    }
}

fn args<T: for<'de> Deserialize<'de>>(v: Value) -> Result<T, String> {
    let v = if v.is_null() { json!({}) } else { v };
    serde_json::from_value(v).map_err(|e| format!("invalid arguments: {e}"))
}

pub async fn call(hub: &Hub, name: &str, a: Value, ctx: &Ctx) -> Result<String, String> {
    match name {
        "glint_runs" => runs(hub, args(a)?, ctx).await,
        "glint_events" => events(hub, args(a)?, ctx).await,
        "glint_event" => event(hub, args(a)?).await,
        "glint_last_error" => last_error(hub, args(a)?, ctx).await,
        "glint_wait_for" => wait_for(hub, a, ctx).await,
        "glint_expect" => expect(hub, args(a)?, ctx).await,
        "glint_pauses" => Ok(pauses(hub)),
        "glint_resume" => resume(hub, args(a)?).await,
        "glint_annotate" => annotate(hub, args(a)?).await,
        "glint_show" => show(hub, args(a)?, ctx).await,
        "glint_clear" => clear(hub, args(a)?, ctx).await,
        "glint_llm_calls" => llm_calls(hub, args(a)?, ctx).await,
        other => Err(format!("unknown tool `{other}`")),
    }
}

fn store_err(e: impl std::fmt::Display) -> String {
    format!("Glint store error: {e}")
}

#[derive(Deserialize)]
struct RunsArgs {
    project: Option<String>,
    limit: Option<usize>,
}

async fn runs(hub: &Hub, a: RunsArgs, ctx: &Ctx) -> Result<String, String> {
    // Every row names its project, so an unknown one lists them all, saying so.
    let (project, head) = match scope(hub, a.project, ctx).await? {
        Scope::Unknown(_) => (None, "Project: all (this agent's project is unknown; pass project=<name> to narrow)\n".to_owned()),
        s => (s.project(), s.head()),
    };
    let runs = hub.runs(project, a.limit.unwrap_or(15).clamp(1, 100)).await.map_err(store_err)?;
    if runs.is_empty() {
        return Ok(format!("{head}No runs recorded yet."));
    }
    let mut out = format!("{head}run · project · last activity · events · errors · last event id\n");
    for r in runs {
        out.push_str(&format!(
            "{} · {} · {} · {} · {} · {}\n",
            r.run,
            r.project,
            r.last.get(..19).unwrap_or(&r.last),
            r.events,
            r.errors,
            r.last_id
        ));
    }
    Ok(out)
}

async fn events(hub: &Hub, mut f: Filter, ctx: &Ctx) -> Result<String, String> {
    let s = scope(hub, f.project.take(), ctx).await?;
    if let Scope::Unknown(t) = s {
        return Ok(t);
    }
    f.project = s.project();
    let head = s.head();
    let q = f.query(EVENTS_DEFAULT, EVENTS_MAX);
    let limit = q.limit();
    let got = hub.query(q).await.map_err(store_err)?;
    if got.is_empty() {
        return Ok(format!("{head}No matching events."));
    }
    let mut out = head;
    for e in &got {
        out.push_str(&line(e));
        out.push('\n');
    }
    if got.len() == limit {
        out.push_str(&format!(
            "(limit reached: pass before={} for older events)\n",
            got[0].id
        ));
    }
    if let Some(last) = got.last() {
        out.push_str(&format!("(newest id: {} — pass after= this to see only newer events)\n", last.id));
    }
    Ok(out)
}

#[derive(Deserialize)]
struct EventArgs {
    id: String,
    max_chars: Option<usize>,
}

async fn event(hub: &Hub, a: EventArgs) -> Result<String, String> {
    let e = hub.get(a.id.clone()).await.map_err(store_err)?.ok_or(format!("No event with id {}", a.id))?;
    let text = serde_json::to_string_pretty(&e).map_err(|e| e.to_string())?;
    let max = a.max_chars.unwrap_or(EVENT_CHARS_DEFAULT).clamp(200, 100_000);
    Ok(if text.chars().count() > max {
        format!("{}\n… (truncated at {max} chars; raise max_chars for more)", text.chars().take(max).collect::<String>())
    } else {
        text
    })
}

#[derive(Deserialize)]
struct LastErrorArgs {
    project: Option<String>,
    context: Option<usize>,
}

async fn last_error(hub: &Hub, a: LastErrorArgs, ctx: &Ctx) -> Result<String, String> {
    let s = scope(hub, a.project, ctx).await?;
    if let Scope::Unknown(t) = s {
        return Ok(t);
    }
    let q = Query { project: s.project(), level: Some(Level::Error), limit: Some(1), ..Query::default() };
    let Some(e) = hub.query(q).await.map_err(store_err)?.pop() else {
        return Ok(format!("{}No errors recorded.", s.head()));
    };
    let mut out = s.head();
    out.push_str(&format!(
        "Last error {} at {} — run {} (project {})\n",
        e.id,
        e.ts.format("%Y-%m-%d %H:%M:%S%.3f"),
        e.run,
        e.project
    ));
    if let Some(f) = &e.origin.file {
        out.push_str(&format!("reported at {f}{}\n", e.origin.line.map(|l| format!(":{l}")).unwrap_or_default()));
    }
    let p = &e.payload;
    if e.kind == "core.exception" {
        out.push_str(&format!(
            "{}: {}\n",
            p.get("class").and_then(Value::as_str).unwrap_or("Exception"),
            p.get("message").and_then(Value::as_str).unwrap_or("")
        ));
        let frames = p.get("frames").and_then(Value::as_array).cloned().unwrap_or_default();
        for f in frames.iter().take(15) {
            out.push_str(&format!("  at {}\n", frame(f)));
        }
        if frames.len() > 15 {
            out.push_str(&format!("  … {} more frames\n", frames.len() - 15));
        }
        let mut cause = p.get("cause");
        while let Some(c) = cause.filter(|c| c.is_object()) {
            out.push_str(&format!(
                "caused by {}: {}\n",
                c.get("class").and_then(Value::as_str).unwrap_or("?"),
                c.get("message").and_then(Value::as_str).unwrap_or("")
            ));
            cause = c.get("cause");
        }
    } else {
        out.push_str(&format!("{}: {}\n", e.kind, clip(&summary(&e), 2000)));
    }
    let n = a.context.unwrap_or(20).min(100);
    if n > 0 {
        // Same run and project: two programs may both use the default run name.
        let q = Query { project: Some(e.project.clone()), run: Some(e.run.clone()), before: Some(e.id.clone()), limit: Some(n), ..Query::default() };
        let before = hub.query(q).await.map_err(store_err)?;
        if !before.is_empty() {
            out.push_str(&format!("\nWhat happened before it in this run ({} events):\n", before.len()));
            for b in &before {
                out.push_str(&line(b));
                out.push('\n');
            }
        }
    }
    out.push_str(&format!("\nFull payload: glint_event id={}\n", e.id));
    Ok(out)
}

async fn wait_for(hub: &Hub, a: Value, ctx: &Ctx) -> Result<String, String> {
    let timeout = a.get("timeout_s").and_then(Value::as_u64).unwrap_or(WAIT_DEFAULT_S).clamp(1, WAIT_MAX_S);
    let lookback = a.get("lookback_s").and_then(Value::as_u64).unwrap_or(WAIT_LOOKBACK_DEFAULT_S).min(WAIT_LOOKBACK_MAX_S);
    let mut f = args::<Filter>(a)?;
    let s = scope(hub, f.project.take(), ctx).await?;
    if let Scope::Unknown(t) = s {
        return Ok(t);
    }
    f.project = s.project();
    let head = s.head();
    let after = f.after.clone().filter(|x| !x.is_empty());
    let q = f.query(1, 1);
    let bookkeeping = |e: &glint_protocol::Envelope| matches!(e.kind.as_str(), "core.update" | "ctl.resume");
    let mut rx = hub.subscribe(); // first, so nothing falls between the stored check and the stream
    let started = chrono::Utc::now();
    let window = match &after {
        Some(id) => format!("stored events after {id}"),
        None if lookback > 0 => format!("the {lookback} s before the call"),
        None => "only new events (lookback_s=0)".to_owned(),
    };
    if after.is_some() || lookback > 0 {
        let stored = Query { limit: Some(EVENTS_MAX), before: None, ..q.clone() };
        let cutoff = started - chrono::Duration::seconds(i64::try_from(lookback).unwrap_or(0));
        let got = hub.query(stored).await.map_err(store_err)?;
        if let Some(e) = got.iter().rev().find(|e| !bookkeeping(e) && (after.is_some() || e.ts >= cutoff)) {
            let ago = (started - e.ts).num_milliseconds().max(0) as f64 / 1000.0;
            let body = serde_json::to_string(&e.payload).unwrap_or_default();
            return Ok(format!(
                "{head}Found in {window} (it happened {ago:.1} s before the call; newest match):\n{}\npayload: {}",
                line(e),
                clip(&body, 2000)
            ));
        }
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout);
    loop {
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Err(_) => return Ok(format!("{head}No matching event: searched {window}, then waited {timeout} s.")),
            Ok(Err(RecvError::Closed)) => return Err("Glint is shutting down.".into()),
            Ok(Err(RecvError::Lagged(_))) => continue,
            Ok(Ok(e)) => {
                if bookkeeping(&e) || !q.matches(&e) {
                    continue;
                }
                let body = serde_json::to_string(&e.payload).unwrap_or_default();
                return Ok(format!("{head}Arrived after the call (searched {window} first):\n{}\npayload: {}", line(&e), clip(&body, 2000)));
            }
        }
    }
}

async fn expect(hub: &Hub, mut r: glint_core::expect::Request, ctx: &Ctx) -> Result<String, String> {
    let x = r.expectation()?;
    let s = scope(hub, r.scope.project.take(), ctx).await?;
    if let Scope::Unknown(t) = s {
        return Ok(t);
    }
    r.scope.project = s.project();
    r.scope.timeout_s = Some(r.scope.timeout_s.unwrap_or(30).clamp(1, WAIT_MAX_S));
    let v = glint_core::expect::run(hub, x, r.scope).await.map_err(store_err)?;
    Ok(format!("{}{}", s.head(), v.report()))
}

fn pauses(hub: &Hub) -> String {
    let p = hub.control().pending();
    if p.is_empty() {
        return "No program is paused.".into();
    }
    let mut out = String::from("pause id · since · reason · location · run\n");
    for x in p {
        out.push_str(&format!(
            "{} · {} · {} · {}{} · {}\n",
            x.id,
            x.since.format("%H:%M:%S"),
            x.reason.as_deref().unwrap_or("-"),
            x.file.as_deref().unwrap_or("?"),
            x.line.map(|l| format!(":{l}")).unwrap_or_default(),
            x.run
        ));
    }
    out.push_str("Resume with glint_resume id=<pause id> (optionally value=… or stop=true).\n");
    out
}

#[derive(Deserialize)]
struct ResumeArgs {
    id: String,
    value: Option<Value>,
    #[serde(default)]
    stop: bool,
}

async fn resume(hub: &Hub, a: ResumeArgs) -> Result<String, String> {
    let r = if a.stop { Resolution::Stop } else { Resolution::Continue { value: a.value } };
    let info = hub.resume(&a.id, r.clone(), "mcp").await.map_err(|e| e.to_string())?;
    Ok(match r {
        Resolution::Stop => format!("Asked {} to stop (run {}).", info.id, info.run),
        Resolution::Continue { value: None } => format!("Resumed {} (run {}).", info.id, info.run),
        Resolution::Continue { value: Some(v) } => format!("Resumed {} with value {} (run {}).", info.id, v, info.run),
    })
}

fn agent_event(kind: &str, payload: Value, meta: Meta, project: Option<String>) -> IncomingEnvelope {
    IncomingEnvelope {
        v: None,
        id: None,
        ts: None,
        run: Some("mcp".into()),
        parent: None,
        project,
        origin: Origin {
            lang: Some("agent".into()),
            sdk: Some(format!("glint-mcp/{}", env!("CARGO_PKG_VERSION"))),
            ..Origin::default()
        },
        kind: kind.into(),
        payload,
        meta,
    }
}

async fn emit(hub: &Hub, e: IncomingEnvelope) -> Result<String, String> {
    let events = hub.normalise(vec![e]);
    let id = events[0].id.clone();
    hub.submit(events).await.map_err(|e| e.to_string())?;
    Ok(id)
}

#[derive(Deserialize)]
struct AnnotateArgs {
    id: String,
    note: String,
    color: Option<String>,
}

async fn annotate(hub: &Hub, a: AnnotateArgs) -> Result<String, String> {
    let target = hub.get(a.id.clone()).await.map_err(store_err)?.ok_or(format!("No event with id {}", a.id))?;
    let mut meta = json!({ "note": clip(&a.note, 2000) });
    if let Some(c) = a.color {
        meta["color"] = json!(c);
    }
    let update = agent_event("core.update", json!({ "target": a.id, "meta": meta }), Meta::default(), Some(target.project));
    emit(hub, update).await?;
    Ok(format!("Note attached to {}.", a.id))
}

#[derive(Deserialize)]
struct ShowArgs {
    format: String,
    content: String,
    label: Option<String>,
    project: Option<String>,
}

async fn show(hub: &Hub, a: ShowArgs, ctx: &Ctx) -> Result<String, String> {
    let (kind, payload) = match a.format.as_str() {
        "markdown" | "mermaid" | "text" => (format!("core.{}", a.format), json!({ "content": a.content })),
        "json" => {
            let v: Value = serde_json::from_str(&a.content).map_err(|e| format!("content is not JSON: {e}"))?;
            ("core.json".to_owned(), json!({ "content": v }))
        }
        other => return Err(format!("unsupported format `{other}`")),
    };
    debug_assert!(is_valid_kind(&kind));
    let meta = Meta { label: a.label.or(Some("agent".into())), color: Some("purple".into()), ..Meta::default() };
    let project = a.project.filter(|p| !p.is_empty()).or_else(|| ctx.project.clone());
    let id = emit(hub, agent_event(&kind, payload, meta, project.clone())).await?;
    Ok(match project {
        Some(p) => format!("Shown as {id} in project {p}."),
        None => format!("Shown as {id}."),
    })
}

#[derive(Deserialize)]
struct ClearArgs {
    project: Option<String>,
}

async fn clear(hub: &Hub, a: ClearArgs, ctx: &Ctx) -> Result<String, String> {
    let project = a.project.filter(|p| !p.is_empty()).or_else(|| ctx.project.clone());
    let id = emit(hub, agent_event("core.clear", json!({}), Meta::default(), project)).await?;
    Ok(format!("View cleared. Pass after={id} to glint_events to see only what happens from now on."))
}

#[derive(Deserialize)]
struct LlmArgs {
    project: Option<String>,
    model: Option<String>,
    q: Option<String>,
    limit: Option<usize>,
}

async fn llm_calls(hub: &Hub, a: LlmArgs, ctx: &Ctx) -> Result<String, String> {
    let s = scope(hub, a.project, ctx).await?;
    if let Scope::Unknown(t) = s {
        return Ok(t);
    }
    let head = s.head();
    let limit = a.limit.unwrap_or(20).clamp(1, 100);
    let q = Query {
        project: s.project(),
        kind: Some("llm.call".into()),
        q: a.q.filter(|s| !s.is_empty()),
        // Over-fetch so a model filter still fills the page.
        limit: Some(if a.model.is_some() { limit * 5 } else { limit }),
        ..Query::default()
    };
    let mut calls = hub.query(q).await.map_err(store_err)?;
    if let Some(m) = a.model.as_deref().filter(|m| !m.is_empty()) {
        calls.retain(|e| e.payload.get("model").and_then(Value::as_str).is_some_and(|x| x.contains(m)));
    }
    let skip = calls.len().saturating_sub(limit);
    let calls = &calls[skip..];
    if calls.is_empty() {
        return Ok(format!("{head}No LLM calls recorded. Send OpenTelemetry GenAI spans to http://127.0.0.1:4318 or use an SDK."));
    }
    let (mut tin, mut tout, mut cost, mut lat, mut errors) = (0.0, 0.0, 0.0, 0.0, 0);
    let mut out = head;
    for e in calls {
        let p = &e.payload;
        let num = |v: Option<&Value>| v.and_then(Value::as_f64).unwrap_or(0.0);
        tin += num(p.pointer("/usage/input"));
        tout += num(p.pointer("/usage/output"));
        cost += num(p.get("cost_usd"));
        lat += num(p.get("latency_ms"));
        if p.get("error").is_some() {
            errors += 1;
        }
        out.push_str(&line(e));
        out.push('\n');
    }
    let n = calls.len() as f64;
    out.push_str(&format!(
        "Totals over {} calls: {tin:.0} input + {tout:.0} output tokens, avg latency {:.0} ms, {errors} errors{}\n",
        calls.len(),
        lat / n,
        if cost > 0.0 { format!(", cost ${cost:.4}") } else { String::new() }
    ));
    Ok(out)
}
