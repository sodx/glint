//! Expectations over what programs did: the check behind `glint_expect` (MCP), `POST /v1/expect`
//! and `glint expect` (CLI). Markers placed with glint() become a test: "these events arrive, with
//! these values, and nothing fails".
//!
//! An expectation is one line of space-separated terms; all must hold for one event:
//!
//! ```text
//! label:"order saved"         meta.label equals
//! kind:sql.query  kind:sql.*  kind equals, or starts with
//! text:refund                 the payload contains this text (case-insensitive)
//! payload.value.total>0       a dot path into the event, compared: = != > >= < <=
//! payload.value.coupon=null   (values are JSON when they parse: numbers, true, null, "x")
//! has:payload.value.email     the path exists and is not null
//! count>=2                    how many matching events are needed (default 1)
//! ```
//!
//! Labels set afterwards (`glint(x)->label('…')` sends a `core.update`) count: updates are applied
//! before matching.

use std::collections::HashMap;

use glint_protocol::{Envelope, Level};
use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
enum Term {
    Kind(String),
    Label(String),
    Text(String),
    Has(String),
    Cmp(String, Op, Value),
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Op {
    Eq,
    Ne,
    Gt,
    Ge,
    Lt,
    Le,
}

#[derive(Debug, Clone)]
pub struct Step {
    pub source: String,
    terms: Vec<Term>,
    min: usize,
}

impl Step {
    pub fn parse(src: &str) -> Result<Step, String> {
        let mut terms = Vec::new();
        let mut min = 1;
        for tok in split(src)? {
            if let Some(v) = tok.strip_prefix("kind:") {
                terms.push(Term::Kind(v.to_owned()));
            } else if let Some(v) = tok.strip_prefix("label:") {
                terms.push(Term::Label(v.to_owned()));
            } else if let Some(v) = tok.strip_prefix("text:") {
                terms.push(Term::Text(v.to_lowercase()));
            } else if let Some(v) = tok.strip_prefix("has:") {
                terms.push(Term::Has(v.to_owned()));
            } else if let Some(n) = tok.strip_prefix("count>=").or_else(|| tok.strip_prefix("count=")) {
                min = n.parse().map_err(|_| format!("`{tok}`: count needs a number"))?;
            } else if let Some((path, op, val)) = comparison(&tok) {
                let v = serde_json::from_str(val).unwrap_or_else(|_| Value::String(val.to_owned()));
                terms.push(Term::Cmp(path.to_owned(), op, v));
            } else {
                return Err(format!("`{tok}`: expected kind:, label:, text:, has:, count>=N or a comparison like payload.value.total>0"));
            }
        }
        if terms.is_empty() {
            return Err(format!("`{src}` says nothing to match"));
        }
        Ok(Step { source: src.trim().to_owned(), terms, min })
    }

    fn matches(&self, e: &Envelope, as_json: &Value) -> bool {
        self.terms.iter().all(|t| match t {
            Term::Kind(k) => k.strip_suffix('*').map_or(e.kind == *k, |p| e.kind.starts_with(p)),
            Term::Label(l) => e.meta.label.as_deref() == Some(l.as_str()),
            Term::Text(s) => serde_json::to_string(&e.payload).is_ok_and(|p| p.to_lowercase().contains(s)),
            Term::Has(p) => path(as_json, p).is_some_and(|v| !v.is_null()),
            Term::Cmp(p, op, want) => compare(path(as_json, p).unwrap_or(&Value::Null), *op, want),
        })
    }
}

fn comparison(tok: &str) -> Option<(&str, Op, &str)> {
    for (s, op) in [(">=", Op::Ge), ("<=", Op::Le), ("!=", Op::Ne), ("=", Op::Eq), (">", Op::Gt), ("<", Op::Lt)] {
        if let Some(i) = tok.find(s) {
            let (path, val) = (&tok[..i], &tok[i + s.len()..]);
            if !path.is_empty() && !path.contains(':') {
                return Some((path, op, val));
            }
        }
    }
    None
}

fn path<'a>(v: &'a Value, p: &str) -> Option<&'a Value> {
    p.split('.').try_fold(v, |cur, k| match cur {
        Value::Object(m) => m.get(k),
        Value::Array(a) => k.parse::<usize>().ok().and_then(|i| a.get(i)),
        _ => None,
    })
}

fn compare(got: &Value, op: Op, want: &Value) -> bool {
    match (op, got.as_f64(), want.as_f64()) {
        (Op::Eq, ..) => got == want || (got.as_str().is_some_and(|g| want.as_str() == Some(g))),
        (Op::Ne, ..) => got != want,
        (Op::Gt, Some(a), Some(b)) => a > b,
        (Op::Ge, Some(a), Some(b)) => a >= b,
        (Op::Lt, Some(a), Some(b)) => a < b,
        (Op::Le, Some(a), Some(b)) => a <= b,
        _ => false,
    }
}

/// Shell-like split on spaces, honouring "double quotes".
fn split(s: &str) -> Result<Vec<String>, String> {
    let (mut out, mut cur, mut quoted, mut any) = (Vec::new(), String::new(), false, false);
    for c in s.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                any = true;
            }
            c if c.is_whitespace() && !quoted => {
                if any {
                    out.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            c => {
                cur.push(c);
                any = true;
            }
        }
    }
    if quoted {
        return Err(format!("`{s}`: unclosed quote"));
    }
    if any {
        out.push(cur);
    }
    Ok(out)
}

#[derive(Debug, Clone)]
pub struct Expectation {
    pub steps: Vec<Step>,
    /// Steps must be met in the order given.
    pub ordered: bool,
    /// Any program error (level ≥ error, not an agent's own tool) fails the expectation at once.
    pub no_errors: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct StepResult {
    pub expect: String,
    pub met: bool,
    pub matched: usize,
    pub needed: usize,
    /// Ids of the matching events (the evidence), oldest first.
    pub events: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Verdict {
    pub pass: bool,
    pub steps: Vec<StepResult>,
    /// The program error that failed it, if one did.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
    pub events_seen: usize,
}

/// Feeds events one by one; says when the outcome is known.
pub struct Evaluator {
    x: Expectation,
    seen: Vec<Envelope>,
    index: HashMap<String, usize>,
    failure: Option<String>,
}

impl Evaluator {
    pub fn new(x: Expectation) -> Self {
        Self { x, seen: Vec::new(), index: HashMap::new(), failure: None }
    }

    /// Takes one event; returns the verdict once it is decided (all met, or a failure).
    pub fn feed(&mut self, e: &Envelope) -> Option<Verdict> {
        match e.kind.as_str() {
            "core.update" => {
                if let Some(i) = e.payload.get("target").and_then(Value::as_str).and_then(|t| self.index.get(t))
                    && let Some(label) = e.payload.pointer("/meta/label").and_then(Value::as_str)
                {
                    self.seen[*i].meta.label = Some(label.to_owned());
                }
            }
            "ctl.resume" => return None,
            _ => {
                if self.x.no_errors && e.level() >= Level::Error && e.origin.lang.as_deref() != Some("agent") && self.failure.is_none() {
                    let p = &e.payload;
                    let what = [p.get("class"), p.get("message")].into_iter().flatten().filter_map(Value::as_str).collect::<Vec<_>>().join(": ");
                    let at = e.origin.file.as_deref().map(|f| format!(" at {f}{}", e.origin.line.map(|l| format!(":{l}")).unwrap_or_default())).unwrap_or_default();
                    self.failure = Some(format!("{} {}{at} (id {})", e.kind, if what.is_empty() { "failed".into() } else { what }, e.id));
                }
                self.index.insert(e.id.clone(), self.seen.len());
                self.seen.push(e.clone());
            }
        }
        let v = self.verdict();
        (v.pass || self.failure.is_some()).then_some(v)
    }

    /// The outcome so far (at a timeout: the final one).
    pub fn verdict(&self) -> Verdict {
        let json: Vec<Value> = self.seen.iter().map(|e| serde_json::to_value(e).unwrap_or_default()).collect();
        let mut from = 0; // ordered: the next step looks after the previous step's last match
        let steps: Vec<StepResult> = self
            .x
            .steps
            .iter()
            .map(|s| {
                let start = if self.x.ordered { from } else { 0 };
                let hits: Vec<usize> = (start..self.seen.len()).filter(|&i| s.matches(&self.seen[i], &json[i])).collect();
                let met = hits.len() >= s.min;
                if self.x.ordered && met {
                    from = hits[s.min - 1] + 1;
                }
                StepResult { expect: s.source.clone(), met, matched: hits.len(), needed: s.min, events: hits.iter().map(|&i| self.seen[i].id.clone()).collect() }
            })
            .collect();
        let pass = self.failure.is_none() && steps.iter().all(|s| s.met);
        Verdict { pass, steps, failure: self.failure.clone(), events_seen: self.seen.len() }
    }
}

impl Verdict {
    /// For people and agents: one line per expectation, then the failure if any.
    pub fn report(&self) -> String {
        let mut out = format!("{}\n", if self.pass { "PASS" } else { "FAIL" });
        for s in &self.steps {
            let ev = if s.events.is_empty() { String::new() } else { format!(" — {}", s.events.iter().take(3).cloned().collect::<Vec<_>>().join(", ")) };
            out.push_str(&format!("  {} {} ({} of {}){ev}\n", if s.met { "✓" } else { "✗" }, s.expect, s.matched.min(9999), s.needed));
        }
        if let Some(f) = &self.failure {
            out.push_str(&format!("  ✗ a program failed: {f}\n"));
        }
        out.push_str(&format!("  ({} events looked at)\n", self.events_seen));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glint_protocol::{BodyFormat, parse_body};

    fn ev(json: &str) -> Envelope {
        let i = parse_body(json.as_bytes(), BodyFormat::Json).unwrap().remove(0).unwrap();
        crate::Normaliser::new().normalise(i, chrono::Utc::now())
    }

    fn x(steps: &[&str], ordered: bool) -> Evaluator {
        Evaluator::new(Expectation { steps: steps.iter().map(|s| Step::parse(s).unwrap()).collect(), ordered, no_errors: true })
    }

    #[test]
    fn markers_with_values_pass_in_order() {
        let mut x = x(&[r#"label:cart payload.value.total>0"#, r#"kind:sql.* text:"insert into orders""#, r#"label:"order saved" payload.value.paid=true"#], true);
        assert!(x.feed(&ev(r#"{"id":"a","kind":"core.value","payload":{"value":{"total":129.5}}}"#)).is_none());
        assert!(x.feed(&ev(r#"{"id":"u","kind":"core.update","payload":{"target":"a","meta":{"label":"cart"}}}"#)).is_none(), "label set afterwards counts");
        assert!(x.feed(&ev(r#"{"id":"b","kind":"sql.query","payload":{"sql":"INSERT INTO orders values (?)"}}"#)).is_none());
        let v = x.feed(&ev(r#"{"id":"c","kind":"core.value","payload":{"value":{"paid":true}},"meta":{"label":"order saved"}}"#)).unwrap();
        assert!(v.pass, "{}", v.report());
        assert_eq!(v.steps[0].events, ["a"]);
    }

    #[test]
    fn order_matters_when_asked() {
        let mut x = x(&["label:second", "label:first"], true);
        x.feed(&ev(r#"{"id":"1","kind":"core.value","payload":{"value":1},"meta":{"label":"first"}}"#));
        x.feed(&ev(r#"{"id":"2","kind":"core.value","payload":{"value":2},"meta":{"label":"second"}}"#));
        assert!(!x.verdict().pass);
    }

    #[test]
    fn a_program_error_fails_at_once_but_an_agents_does_not() {
        let mut x = x(&["label:done"], false);
        assert!(x.feed(&ev(r#"{"kind":"agent.tool_result","payload":{"ok":false},"meta":{"level":"error"},"origin":{"lang":"agent"}}"#)).is_none());
        let v = x.feed(&ev(r#"{"id":"boom","kind":"core.exception","payload":{"class":"TypeError","message":"x is undefined","frames":[]}}"#)).unwrap();
        assert!(!v.pass && v.failure.as_deref().is_some_and(|f| f.contains("TypeError: x is undefined")));
    }

    #[test]
    fn counts_and_comparisons() {
        let mut x = x(&["kind:core.log count>=2 payload.message!=skip", "has:payload.value.email"], false);
        for (i, m) in ["a", "skip", "b"].iter().enumerate() {
            x.feed(&ev(&format!(r#"{{"id":"l{i}","kind":"core.log","payload":{{"message":"{m}"}}}}"#)));
        }
        assert!(x.verdict().steps[0].met);
        assert!(!x.verdict().steps[1].met);
        x.feed(&ev(r#"{"id":"e","kind":"core.value","payload":{"value":{"email":"a@b.c"}}}"#));
        assert!(x.verdict().pass);
    }

    #[test]
    fn bad_expectations_say_why() {
        assert!(Step::parse("").is_err());
        assert!(Step::parse("label:\"open").unwrap_err().contains("unclosed"));
        assert!(Step::parse("whatever").unwrap_err().contains("expected"));
        assert!(Step::parse("count>=x kind:a").unwrap_err().contains("number"));
    }
}

/// Where and how long to look.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct Scope {
    pub project: Option<String>,
    pub run: Option<String>,
    /// Look at stored events after this event id (e.g. the newest event before a command ran).
    pub after: Option<String>,
    /// Without `after`: also look at events stored in the last N seconds (default 30), so that a
    /// program run just before asking counts.
    pub lookback_s: Option<u64>,
    /// How long to wait for events still to come (default 30, at most 300).
    pub timeout_s: Option<u64>,
}

/// Checks `x` against stored events (see `Scope`), then live ones, until it is decided or the
/// timeout passes.
pub async fn run(hub: &crate::Hub, x: Expectation, s: Scope) -> Result<Verdict, crate::StoreError> {
    use std::collections::HashSet;
    use tokio::sync::broadcast::error::RecvError;
    let mut rx = hub.subscribe(); // first, so nothing falls between the query and the stream
    let in_scope = |e: &Envelope| s.project.as_ref().is_none_or(|p| *p == e.project) && s.run.as_ref().is_none_or(|r| *r == e.run);
    let mut ev = Evaluator::new(x);
    let mut done = HashSet::new();
    let stored = hub
        .query(crate::Query {
            project: s.project.clone(),
            run: s.run.clone(),
            kind: None,
            level: None,
            q: None,
            after: s.after.clone(),
            before: None,
            limit: Some(crate::Query::MAX_LIMIT),
        })
        .await?;
    let cutoff = chrono::Utc::now() - chrono::Duration::seconds(i64::try_from(s.lookback_s.unwrap_or(30)).unwrap_or(30));
    for e in stored {
        if s.after.is_none() && e.ts < cutoff {
            continue;
        }
        done.insert(e.id.clone());
        if let Some(v) = ev.feed(&e) {
            return Ok(v);
        }
    }
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(s.timeout_s.unwrap_or(30).clamp(1, 300));
    loop {
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Err(_) | Ok(Err(RecvError::Closed)) => return Ok(ev.verdict()),
            Ok(Err(RecvError::Lagged(_))) => continue,
            Ok(Ok(e)) => {
                if !in_scope(&e) || done.contains(&e.id) {
                    continue;
                }
                if let Some(v) = ev.feed(&e) {
                    return Ok(v);
                }
            }
        }
    }
}

fn yes() -> bool {
    true
}

/// The arguments of `glint_expect` and `POST /v1/expect`.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct Request {
    /// One expectation per entry, in the term syntax of this module.
    pub expect: Vec<String>,
    #[serde(default = "yes")]
    pub ordered: bool,
    #[serde(default = "yes")]
    pub no_errors: bool,
    #[serde(flatten)]
    pub scope: Scope,
}

impl Request {
    pub fn expectation(&self) -> Result<Expectation, String> {
        if self.expect.is_empty() {
            return Err("give at least one expectation, e.g. label:\"order saved\"".into());
        }
        let steps = self.expect.iter().map(|s| Step::parse(s)).collect::<Result<Vec<_>, _>>()?;
        Ok(Expectation { steps, ordered: self.ordered, no_errors: self.no_errors })
    }
}
