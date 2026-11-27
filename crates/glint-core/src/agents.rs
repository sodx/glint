//! What coding agents did with Glint: every MCP tool call and every hook action, recorded next
//! to the events (same store, same retention, never sent anywhere), and the numbers that say
//! whether agents use Glint and whether it helps them.
//!
//! An [`Activity`] names the events it put in front of the agent (`events`): the ids a tool
//! answer contained, or the failures a hook listed. That is what "read by" means in the window
//! and what the time from a failure to the agent reading it is measured with.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use chrono::{DateTime, Local, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// An agent connected over MCP (`initialize`).
pub const MCP_CONNECT: &str = "mcp.connect";
/// An agent called a Glint tool.
pub const MCP_CALL: &str = "mcp.call";
/// A hook put Glint's context (and N failures) in front of the agent.
pub const HOOK_CONTEXT: &str = "hook.context";
/// The Stop guard sent the agent back because programs failed during its turn.
pub const HOOK_STOP_GUARD: &str = "hook.stop_guard";
/// What the agent did after the Stop guard: `detail.outcome` is `fixed` or `explained`.
pub const HOOK_STOP_OUTCOME: &str = "hook.stop_outcome";

/// Most event ids kept per activity.
pub const EVENTS_MAX: usize = 200;

/// One thing an agent did with Glint.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Activity {
    #[serde(default = "Utc::now")]
    pub ts: DateTime<Utc>,
    /// Who: the MCP client's name (`claude-code`, `cursor`…) or the hook's agent.
    pub agent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    /// [`MCP_CONNECT`], [`MCP_CALL`], [`HOOK_CONTEXT`], [`HOOK_STOP_GUARD`], [`HOOK_STOP_OUTCOME`].
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// Size of the answer the agent got, in bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_bytes: Option<u64>,
    /// The tool answered with an error.
    #[serde(default)]
    pub is_error: bool,
    /// Event ids put in front of the agent.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<String>,
    /// How many of `events` are program failures; filled in by the store.
    #[serde(default)]
    pub error_events: u32,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub detail: Value,
}

impl Activity {
    pub fn new(agent: impl Into<String>, action: &str) -> Self {
        Self {
            ts: Utc::now(),
            agent: agent.into(),
            session: None,
            project: None,
            action: action.to_owned(),
            tool: None,
            duration_ms: None,
            result_bytes: None,
            is_error: false,
            events: Vec::new(),
            error_events: 0,
            detail: Value::Null,
        }
    }

    /// Trims what a client sent to sane sizes.
    pub fn bounded(mut self) -> Self {
        let cut = |s: &mut String, n: usize| {
            if s.len() > n {
                let mut i = n;
                while !s.is_char_boundary(i) {
                    i -= 1;
                }
                s.truncate(i);
            }
        };
        cut(&mut self.agent, 80);
        cut(&mut self.action, 40);
        for s in [&mut self.session, &mut self.project, &mut self.tool].into_iter().flatten() {
            cut(s, 200);
        }
        self.project = self.project.filter(|p| !p.is_empty());
        let mut seen = BTreeSet::new();
        self.events.retain(|id| !id.is_empty() && id.len() <= 128 && seen.insert(id.clone()));
        self.events.truncate(EVENTS_MAX);
        if self.detail.to_string().len() > 4096 {
            self.detail = Value::Null;
        }
        self
    }
}

/// A program failure (error level or worse, not an agent's own tool failure).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Failure {
    pub id: String,
    pub ts: DateTime<Utc>,
    pub project: String,
}

/// An agent saw this event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventRead {
    pub event: String,
    pub agent: String,
    /// When it first saw it.
    pub ts: DateTime<Utc>,
    /// `mcp` (the agent asked) or `hook` (Glint told it).
    pub via: String,
}

/// Which agents, which tools, and how fast failures reached the agent: per day and project, and
/// in total.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentStats {
    pub since: Option<DateTime<Utc>>,
    /// A failure counts as looked at in time when an agent read it within this many minutes.
    pub within_min: u64,
    pub totals: Group,
    /// Newest day first; within a day, by project.
    pub groups: Vec<Group>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Group {
    /// Local date, `2026-11-27`; absent in totals.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub day: Option<String>,
    /// Absent in totals; empty when the agent named no project.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    pub agents: BTreeMap<String, AgentUse>,
    /// MCP calls by tool.
    pub tools: BTreeMap<String, u64>,
    pub calls: u64,
    /// Calls the tool answered with an error.
    pub call_errors: u64,
    /// Calls whose answer contained a program failure.
    pub calls_with_failures: u64,
    pub result_bytes: u64,
    /// Hooks that gave the agent Glint's context, and the failures they listed.
    pub hook_context: u64,
    pub hook_failures: u64,
    pub stop_guard: u64,
    pub stop_fixed: u64,
    pub stop_explained: u64,
    /// Program failures while an agent was around (same day and project as its activity).
    pub failures: u64,
    /// Of those, read by an agent over MCP at all, and within `within_min`.
    pub read: u64,
    pub read_in_time: u64,
    /// Listed to the agent by a hook (it was told, whether or not it then looked).
    pub told: u64,
    /// Median seconds from a failure to an agent first reading it over MCP.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub median_read_s: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentUse {
    pub sessions: u64,
    pub calls: u64,
    pub hooks: u64,
}

/// Computes [`AgentStats`]. `activity` is everything since `since`, oldest first; `failures` the
/// program failures since then.
pub fn stats(activity: &[Activity], failures: &[Failure], since: Option<DateTime<Utc>>, within_min: u64, project: Option<&str>) -> AgentStats {
    let day = |t: &DateTime<Utc>| t.with_timezone(&Local).format("%Y-%m-%d").to_string();
    let wanted = |p: &str| project.is_none_or(|w| w == p);

    // First MCP read and first hook mention of each event, per agent-agnostic event id.
    let mut first_read: HashMap<&str, DateTime<Utc>> = HashMap::new();
    let mut told: BTreeSet<&str> = BTreeSet::new();
    for a in activity {
        for id in &a.events {
            if a.action == MCP_CALL {
                first_read.entry(id).and_modify(|t| *t = (*t).min(a.ts)).or_insert(a.ts);
            } else if a.action == HOOK_CONTEXT || a.action == HOOK_STOP_GUARD {
                told.insert(id);
            }
        }
    }

    let mut groups: BTreeMap<(String, String), Group> = BTreeMap::new();
    let mut sessions: BTreeMap<(String, String, String), BTreeSet<String>> = BTreeMap::new();
    let mut all_sessions: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for a in activity {
        let p = a.project.clone().unwrap_or_default();
        // A connection names no project over HTTP; its calls do. Only count it where it is known.
        if !wanted(&p) || (a.action == MCP_CONNECT && p.is_empty()) {
            continue;
        }
        let key = (day(&a.ts), p);
        let g = groups.entry(key.clone()).or_default();
        let u = g.agents.entry(a.agent.clone()).or_default();
        sessions.entry((key.0, key.1, a.agent.clone())).or_default().insert(a.session.clone().unwrap_or_default());
        all_sessions.entry(a.agent.clone()).or_default().insert(a.session.clone().unwrap_or_default());
        match a.action.as_str() {
            MCP_CALL => {
                u.calls += 1;
                g.calls += 1;
                *g.tools.entry(a.tool.clone().unwrap_or_default()).or_default() += 1;
                g.call_errors += u64::from(a.is_error);
                g.calls_with_failures += u64::from(a.error_events > 0);
                g.result_bytes += a.result_bytes.unwrap_or(0);
            }
            HOOK_CONTEXT => {
                u.hooks += 1;
                g.hook_context += 1;
                g.hook_failures += a.detail["failures"].as_u64().unwrap_or(a.events.len() as u64);
            }
            HOOK_STOP_GUARD => {
                u.hooks += 1;
                g.stop_guard += 1;
            }
            HOOK_STOP_OUTCOME => match a.detail["outcome"].as_str() {
                Some("fixed") => g.stop_fixed += 1,
                Some("explained") => g.stop_explained += 1,
                _ => {}
            },
            _ => {}
        }
    }
    for ((d, p, agent), s) in sessions {
        if let Some(u) = groups.get_mut(&(d, p)).and_then(|g| g.agents.get_mut(&agent)) {
            u.sessions = s.len() as u64;
        }
    }

    let within = chrono::Duration::minutes(i64::try_from(within_min).unwrap_or(i64::MAX / 60_000));
    let mut delays: BTreeMap<(String, String), Vec<f64>> = BTreeMap::new();
    let mut all_delays = Vec::new();
    for f in failures {
        let Some(g) = groups.get_mut(&(day(&f.ts), f.project.clone())) else { continue };
        g.failures += 1;
        g.told += u64::from(told.contains(f.id.as_str()));
        if let Some(t) = first_read.get(f.id.as_str()) {
            g.read += 1;
            let d = (*t - f.ts).max(chrono::Duration::zero());
            g.read_in_time += u64::from(d <= within);
            let s = d.num_milliseconds() as f64 / 1000.0;
            delays.entry((day(&f.ts), f.project.clone())).or_default().push(s);
            all_delays.push(s);
        }
    }
    for (k, mut v) in delays {
        if let Some(g) = groups.get_mut(&k) {
            g.median_read_s = median(&mut v);
        }
    }

    let mut totals = Group::default();
    for g in groups.values() {
        for (name, u) in &g.agents {
            let t = totals.agents.entry(name.clone()).or_default();
            t.sessions = all_sessions.get(name).map_or(0, |s| s.len() as u64);
            t.calls += u.calls;
            t.hooks += u.hooks;
        }
        for (tool, n) in &g.tools {
            *totals.tools.entry(tool.clone()).or_default() += n;
        }
        totals.calls += g.calls;
        totals.call_errors += g.call_errors;
        totals.calls_with_failures += g.calls_with_failures;
        totals.result_bytes += g.result_bytes;
        totals.hook_context += g.hook_context;
        totals.hook_failures += g.hook_failures;
        totals.stop_guard += g.stop_guard;
        totals.stop_fixed += g.stop_fixed;
        totals.stop_explained += g.stop_explained;
        totals.failures += g.failures;
        totals.read += g.read;
        totals.read_in_time += g.read_in_time;
        totals.told += g.told;
    }
    totals.median_read_s = median(&mut all_delays);

    let mut list: Vec<Group> = groups
        .into_iter()
        .map(|((d, p), mut g)| {
            g.day = Some(d);
            g.project = Some(p);
            g
        })
        .collect();
    list.sort_by(|a, b| b.day.cmp(&a.day).then(a.project.cmp(&b.project)));
    AgentStats { since, within_min, totals, groups: list }
}

fn median(v: &mut [f64]) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(f64::total_cmp);
    let m = v.len() / 2;
    Some(if v.len() % 2 == 1 { v[m] } else { f64::midpoint(v[m - 1], v[m]) })
}

/// A friendly name for an agent id: `claude-code` → `Claude Code`.
pub fn agent_name(id: &str) -> String {
    match id {
        "claude-code" | "claude-ai" => "Claude Code".into(),
        "cursor" | "cursor-vscode" => "Cursor".into(),
        "Visual Studio Code" | "vscode" => "VS Code".into(),
        "windsurf" => "Windsurf".into(),
        "gemini-cli-mcp-client" | "gemini-cli" => "Gemini CLI".into(),
        "codex-mcp-client" | "codex" => "Codex".into(),
        "" => "unknown".into(),
        other => other.into(),
    }
}

/// Seconds as `40 s`, `3.5 min`, `2.1 h`.
pub fn duration(s: f64) -> String {
    if s < 60.0 {
        format!("{s:.0} s")
    } else if s < 3600.0 {
        format!("{:.1} min", s / 60.0)
    } else {
        format!("{:.1} h", s / 3600.0)
    }
}

/// The numbers as text, for `glint stats`.
pub fn render(st: &AgentStats) -> String {
    let mut out = String::new();
    let t = &st.totals;
    if st.groups.is_empty() {
        out.push_str("No agent has used Glint in this period.\n");
        return out;
    }
    out.push_str(&summary_lines(t, st.within_min, ""));
    for g in &st.groups {
        let p = g.project.as_deref().filter(|p| !p.is_empty()).unwrap_or("no project");
        out.push_str(&format!("\n{} · {p}\n", g.day.as_deref().unwrap_or("")));
        out.push_str(&summary_lines(g, st.within_min, "  "));
    }
    out
}

fn summary_lines(g: &Group, within_min: u64, ind: &str) -> String {
    let mut out = String::new();
    let agents: Vec<String> = g
        .agents
        .iter()
        .map(|(n, u)| {
            let mut parts = vec![format!("{} session{}", u.sessions, if u.sessions == 1 { "" } else { "s" })];
            if u.calls > 0 {
                parts.push(format!("{} call{}", u.calls, if u.calls == 1 { "" } else { "s" }));
            }
            if u.hooks > 0 {
                parts.push(format!("{} hook{}", u.hooks, if u.hooks == 1 { "" } else { "s" }));
            }
            format!("{} ({})", agent_name(n), parts.join(", "))
        })
        .collect();
    out.push_str(&format!("{ind}Agents: {}\n", agents.join(", ")));
    if g.calls > 0 {
        let mut tools: Vec<(&String, &u64)> = g.tools.iter().collect();
        tools.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
        let list: Vec<String> = tools.iter().map(|(t, n)| format!("{t} {n}")).collect();
        out.push_str(&format!("{ind}Tool calls: {} ({})", g.calls, list.join(", ")));
        if g.call_errors > 0 {
            out.push_str(&format!(", {} answered with an error", g.call_errors));
        }
        out.push('\n');
        if g.calls_with_failures > 0 {
            out.push_str(&format!("{ind}Calls that showed a failure: {}\n", g.calls_with_failures));
        }
    } else {
        out.push_str(&format!("{ind}Tool calls: none\n"));
    }
    if g.hook_context + g.stop_guard > 0 {
        out.push_str(&format!("{ind}Hooks: context given {} time{} ({} failure{} listed)", g.hook_context, plural(g.hook_context), g.hook_failures, plural(g.hook_failures)));
        if g.stop_guard > 0 {
            out.push_str(&format!(", stop guard fired {} (fixed {}, explained {})", g.stop_guard, g.stop_fixed, g.stop_explained));
        }
        out.push('\n');
    }
    if g.failures > 0 {
        out.push_str(&format!(
            "{ind}Failures: {}, read by an agent {} ({} within {within_min} min), told by a hook {}",
            g.failures, g.read, g.read_in_time, g.told
        ));
        if let Some(m) = g.median_read_s {
            out.push_str(&format!(", median time to read {}", duration(m)));
        }
        out.push('\n');
    } else {
        out.push_str(&format!("{ind}Failures: none\n"));
    }
    out
}

fn plural(n: u64) -> &'static str {
    if n == 1 { "" } else { "s" }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(min: i64) -> DateTime<Utc> {
        // Noon local time keeps every timestamp of the test on one local day.
        let noon = Local::now().date_naive().and_hms_opt(12, 0, 0).unwrap().and_local_timezone(Local).unwrap().with_timezone(&Utc);
        noon + chrono::Duration::minutes(min)
    }

    fn call(min: i64, tool: &str, events: &[&str], project: Option<&str>) -> Activity {
        let mut a = Activity::new("claude-code", MCP_CALL);
        a.ts = at(min);
        a.tool = Some(tool.into());
        a.session = Some("s1".into());
        a.project = project.map(Into::into);
        a.events = events.iter().map(|s| (*s).to_owned()).collect();
        a
    }

    #[test]
    fn counts_calls_reads_and_time_to_read() {
        let mut hook = Activity::new("claude-code", HOOK_CONTEXT);
        hook.ts = at(1);
        hook.project = Some("shop".into());
        hook.session = Some("s1".into());
        hook.events = vec!["F1".into()];
        hook.detail = serde_json::json!({ "failures": 1 });
        let act = vec![
            hook,
            call(3, "glint_last_error", &["F1"], Some("shop")),
            call(30, "glint_events", &["F2", "F1"], Some("shop")),
            call(31, "glint_runs", &[], None),
        ];
        let fails = vec![
            Failure { id: "F1".into(), ts: at(0), project: "shop".into() },
            Failure { id: "F2".into(), ts: at(5), project: "shop".into() },
            Failure { id: "F3".into(), ts: at(6), project: "shop".into() },
            Failure { id: "B1".into(), ts: at(6), project: "blog".into() },
        ];
        let st = stats(&act, &fails, None, 10, None);
        let t = &st.totals;
        assert_eq!((t.calls, t.hook_context, t.hook_failures), (3, 1, 1));
        assert_eq!(t.tools["glint_last_error"], 1);
        assert_eq!(t.failures, 3, "blog had no agent around");
        assert_eq!((t.read, t.read_in_time, t.told), (2, 1, 1));
        // F1 read after 3 min, F2 after 25 min.
        assert_eq!(t.median_read_s, Some(840.0));
        assert_eq!(t.agents["claude-code"].sessions, 1, "one session across projects");
        assert!(st.groups.iter().all(|g| g.agents["claude-code"].sessions == 1));
        assert_eq!(st.groups.len(), 2);
        let text = render(&st);
        assert!(text.contains("Claude Code"), "{text}");
        assert!(text.contains("read by an agent 2 (1 within 10 min)"), "{text}");
        let shop = stats(&act, &fails, None, 10, Some("shop"));
        assert_eq!(shop.groups.len(), 1);
        assert_eq!(shop.totals.calls, 2);
    }

    #[test]
    fn bounded_dedupes_and_caps() {
        let mut a = Activity::new("x".repeat(500), MCP_CALL);
        a.events = vec!["a".into(), "a".into(), String::new(), "b".into()];
        a.project = Some(String::new());
        let a = a.bounded();
        assert_eq!(a.agent.len(), 80);
        assert_eq!(a.events, ["a", "b"]);
        assert_eq!(a.project, None);
    }

    #[test]
    fn empty_is_said_plainly() {
        assert!(render(&stats(&[], &[], None, 10, None)).starts_with("No agent"));
    }
}
