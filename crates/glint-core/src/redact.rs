//! Secrets redaction. Every event passes through [`Redactor::redact`] in the normaliser, before it
//! is stored, published or shown to an agent, so passwords, tokens and keys never reach the
//! database, the window or an agent's context.
//!
//! Two nets:
//! - Keys: a value whose key looks secret (`password`, `api_key`, `Authorization`, ...) is replaced
//!   at any depth of `payload`, `meta` and `origin`, including `{"name": "Cookie", "value": ...}`
//!   pairs, `["Authorization", "..."]` pairs and SQL bindings whose column looks secret.
//! - Shapes: well-known token shapes (Bearer, JWT, AWS, GitHub, Slack, Stripe, OpenAI/Anthropic,
//!   Google, PEM private keys, `user:pass@` in URLs, `password=...` in text) are replaced inside
//!   any string.
//!
//! Not secret by design: counters and sizes (`token_count`, `max_tokens`, `password_length`),
//! pagination cursors (`next_token`, `page_token`) and ids (`session_id`, `request_id`). Ids are
//! what a debugging session correlates on; add them to the extra keys to hide them anyway.
//!
//! An allowlisted key turns both nets off for its value and everything below it.

use std::sync::{Arc, LazyLock};

use glint_protocol::{Meta, Origin};
use regex::{Captures, Regex, RegexSet};
use serde_json::{Map, Value};

/// What replaces a secret. Shapes keep their prefix: `sk-ant-…[redacted]`.
pub const MARKER: &str = "[redacted]";
const SHAPE_MARKER: &str = "…[redacted]";

/// The user's part of the configuration; the built-in rules are always there while enabled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedactConfig {
    pub enabled: bool,
    /// Extra key names; a key matches when its name, lowercased without `_`, `-` and `.`, equals
    /// or ends with one.
    pub keys: Vec<String>,
    /// Extra regular expressions. A group named `s` marks the part to hide; otherwise the whole
    /// match goes.
    pub patterns: Vec<String>,
    /// Keys whose values are never redacted.
    pub allow: Vec<String>,
}

impl Default for RedactConfig {
    fn default() -> Self {
        Self { enabled: true, keys: Vec::new(), patterns: Vec::new(), allow: Vec::new() }
    }
}

struct Rule {
    re: Regex,
    marker: &'static str,
    /// The match has a group `k` (a key name) that must look secret.
    check_key: bool,
}

pub struct Redactor {
    enabled: bool,
    keys: Vec<String>,
    allow: Vec<String>,
    set: RegexSet,
    rules: Vec<Rule>,
    /// Shorter strings hold no built-in shape; extra patterns may match anything.
    min_len: usize,
}

impl std::fmt::Debug for Redactor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Redactor").field("enabled", &self.enabled).field("keys", &self.keys).field("allow", &self.allow).field("rules", &self.rules.len()).finish()
    }
}

/// Key-name fragments that make a key secret wherever they appear in it.
const CONTAINS: &[&str] = &["password", "passwd", "passphrase", "secret", "apikey", "authorization", "cookie", "privatekey", "accesskey", "credential"];
/// Fragments that make a key secret only at its end: `access_token` but not `token_count`.
const SUFFIX: &[&str] = &["token", "dsn"];
/// Whole names that are secret.
const EXACT: &[&str] = &["pass", "pwd"];
/// Endings that describe a secret rather than hold one: `password_length`, `token_type`.
const NOT_SECRET_SUFFIX: &[&str] = &["count", "length", "len", "size", "ttl", "expires", "expiresat", "expiresin", "type", "policy", "required", "enabled"];
/// Pagination cursors end in `token` but are not credentials.
const NOT_SECRET: &[&str] = &["nexttoken", "pagetoken", "nextpagetoken", "continuationtoken", "cursortoken", "synctoken"];

/// Built-in shapes: (pattern, marker, check_key). Each has a group `s`, the part replaced.
const SHAPES: &[(&str, &str, bool)] = &[
    // PEM private key blocks, also cut off ones.
    (r"-----BEGIN (?:[A-Z0-9]+ )*PRIVATE KEY-----(?P<s>[\s\S]*?)(?:-----END|\z)", MARKER, false),
    // Passwords in URLs and DSNs: scheme://user:pass@host.
    (r#"(?i)\b[a-z][a-z0-9+.\-]{1,30}://[^\s/:@'"]*:(?P<s>[^\s/@'"]+)@"#, MARKER, false),
    // Cookie and Authorization headers in text, to the end of the line.
    (r#"(?i)\b(?P<k>(?:set-)?cookie|(?:proxy-)?authorization)["']?[ \t]*:[ \t]*["']?(?P<s>[^\r\n"']+)"#, MARKER, true),
    (r"(?i)\bbearer\s+(?P<s>[A-Za-z0-9._~+/\-]{16,}=*)", MARKER, false),
    (r"\beyJ(?P<s>[A-Za-z0-9_\-]{5,}\.eyJ[A-Za-z0-9_\-]{5,}\.[A-Za-z0-9_\-]{5,})", SHAPE_MARKER, false),
    (r"\b(?:AKIA|ASIA)(?P<s>[0-9A-Z]{16})\b", SHAPE_MARKER, false),
    (r"\bgh[pousr]_(?P<s>[A-Za-z0-9]{30,})", SHAPE_MARKER, false),
    (r"\bgithub_pat_(?P<s>[A-Za-z0-9_]{20,})", SHAPE_MARKER, false),
    (r"\bxox[abposr]-(?P<s>[A-Za-z0-9\-]{10,})", SHAPE_MARKER, false),
    (r"\b(?:sk|rk)_(?:live|test)_(?P<s>[A-Za-z0-9]{10,})", SHAPE_MARKER, false),
    (r"\bwhsec_(?P<s>[A-Za-z0-9]{20,})", SHAPE_MARKER, false),
    (r"\bsk-(?:ant-(?:[a-z]+\d*-)?|proj-|svcacct-|admin-)?(?P<s>[A-Za-z0-9_\-]{20,})", SHAPE_MARKER, false),
    (r"\bAIza(?P<s>[0-9A-Za-z_\-]{35})", SHAPE_MARKER, false),
    // SQL: IDENTIFIED BY 'x', PASSWORD 'x'.
    (r"(?i)\b(?:identified\s+(?:with\s+\w+\s+)?by|password)\s+'(?P<s>[^']+)'", MARKER, false),
];

/// `password=...`, `"api_key": "..."`, `token: ...` in text; the key alternation is filled in.
const KEY_VALUE: &str = r#"(?i)(?P<k>[A-Za-z0-9_.\-]*(?:WORDS)[A-Za-z0-9_\-]*)["']?[ \t]*[:=][ \t]*["']?(?P<s>[^\s"'&,;(){}\[\]<>]+)"#;
const KEY_WORDS: &[&str] = &["password", "passwd", "passphrase", "secret", r"api[_\-]?key", "token", r"private[_\-]?key", r"access[_\-]?key", "credential", "dsn", "pass", "pwd"];

static DEFAULT: LazyLock<Arc<Redactor>> = LazyLock::new(|| Arc::new(Redactor::new(&RedactConfig::default()).expect("built-in redaction patterns compile")));

fn norm(k: &str) -> String {
    k.chars().filter(char::is_ascii_alphanumeric).map(|c| c.to_ascii_lowercase()).collect()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    Plain,
    Secret,
    Allowed,
}

impl Default for Redactor {
    fn default() -> Self {
        Self::new(&RedactConfig::default()).expect("built-in redaction patterns compile")
    }
}

impl Redactor {
    /// The built-in configuration, compiled once.
    pub fn shared_default() -> Arc<Redactor> {
        DEFAULT.clone()
    }

    /// Compiles the rules; fails on an invalid extra pattern.
    pub fn new(cfg: &RedactConfig) -> Result<Self, String> {
        let keys: Vec<String> = cfg.keys.iter().map(|k| norm(k)).filter(|k| !k.is_empty()).collect();
        let allow: Vec<String> = cfg.allow.iter().map(|k| norm(k)).filter(|k| !k.is_empty()).collect();
        let mut words: Vec<String> = KEY_WORDS.iter().map(|w| (*w).to_owned()).collect();
        words.extend(cfg.keys.iter().map(|k| k.trim()).filter(|k| !k.is_empty()).map(regex::escape));
        let mut rules = Vec::new();
        for (p, marker, check_key) in SHAPES {
            rules.push(Rule { re: Regex::new(p).map_err(|e| e.to_string())?, marker, check_key: *check_key });
        }
        rules.push(Rule { re: Regex::new(&KEY_VALUE.replace("WORDS", &words.join("|"))).map_err(|e| e.to_string())?, marker: MARKER, check_key: true });
        for p in cfg.patterns.iter().map(|p| p.trim()).filter(|p| !p.is_empty()) {
            let re = Regex::new(p).map_err(|e| format!("redact pattern `{p}`: {e}"))?;
            rules.push(Rule { re, marker: MARKER, check_key: false });
        }
        let set = RegexSet::new(rules.iter().map(|r| r.re.as_str())).map_err(|e| e.to_string())?;
        let min_len = if rules.len() > SHAPES.len() + 1 { 1 } else { 6 };
        Ok(Self { enabled: cfg.enabled, keys, allow, set, rules, min_len })
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    fn class(&self, key: &str) -> Class {
        let k = norm(key);
        if k.is_empty() {
            return Class::Plain;
        }
        if self.allow.contains(&k) {
            return Class::Allowed;
        }
        if self.keys.iter().any(|u| k.ends_with(u.as_str())) {
            return Class::Secret;
        }
        if NOT_SECRET.contains(&k.as_str()) || k.ends_with("pagetoken") || NOT_SECRET_SUFFIX.iter().any(|s| k.ends_with(s)) {
            return Class::Plain;
        }
        if EXACT.contains(&k.as_str()) || CONTAINS.iter().any(|c| k.contains(c)) || SUFFIX.iter().any(|s| k.ends_with(s)) {
            return Class::Secret;
        }
        Class::Plain
    }

    /// Redacts an event in place; returns how many values were replaced.
    pub fn redact(&self, kind: &str, payload: &mut Value, meta: &mut Meta, origin: &mut Origin) -> usize {
        if !self.enabled {
            return 0;
        }
        let mut n = 0;
        if kind.starts_with("sql.") {
            self.sql_bindings(payload, &mut n);
        }
        self.walk(payload, &mut n);
        if let Some(l) = meta.label.as_mut() {
            self.scrub(l, &mut n);
        }
        for t in &mut meta.tags {
            self.scrub(t, &mut n);
        }
        self.walk_map(&mut meta.extra, &mut n);
        self.walk_map(&mut origin.extra, &mut n);
        n
    }

    /// Redacts one JSON value (an event payload, say); returns how many values were replaced.
    pub fn redact_value(&self, v: &mut Value) -> usize {
        let mut n = 0;
        if self.enabled {
            self.walk(v, &mut n);
        }
        n
    }

    /// Redacts the shapes in one string; returns how many were replaced.
    pub fn redact_str(&self, s: &mut String) -> usize {
        let mut n = 0;
        if self.enabled {
            self.scrub(s, &mut n);
        }
        n
    }

    fn walk(&self, v: &mut Value, n: &mut usize) {
        match v {
            Value::String(s) => self.scrub(s, n),
            Value::Array(a) => {
                // ["Authorization", "Bearer ..."] header pairs.
                if a.len() == 2
                    && let Some(k) = a[0].as_str().map(str::to_owned)
                {
                    match self.class(&k) {
                        Class::Secret => return hide(&mut a[1], n, self, &k),
                        Class::Allowed => return,
                        Class::Plain => {}
                    }
                }
                for x in a {
                    self.walk(x, n);
                }
            }
            Value::Object(m) => self.walk_map(m, n),
            _ => {}
        }
    }

    fn walk_map(&self, m: &mut Map<String, Value>, n: &mut usize) {
        // {"name": "Cookie", "value": "..."}, {"key": "http.request.header.authorization", "value": ...}
        let named = ["name", "key"].iter().find_map(|f| m.get(*f).and_then(Value::as_str)).map(|k| (self.class(k), k.to_owned()));
        for (k, v) in m.iter_mut() {
            match self.class(k) {
                Class::Allowed => {}
                Class::Secret => hide(v, n, self, k),
                Class::Plain => match &named {
                    Some((Class::Secret, name)) if k == "value" => hide(v, n, self, name),
                    Some((Class::Allowed, _)) if k == "value" => {}
                    _ => self.walk(v, n),
                },
            }
        }
    }

    /// Replaces the shapes found in `s`.
    fn scrub(&self, s: &mut String, n: &mut usize) {
        if s.len() < self.min_len {
            return;
        }
        let hits = self.set.matches(s);
        if !hits.matched_any() {
            return;
        }
        for i in hits.iter() {
            let rule = &self.rules[i];
            let mut count = 0;
            let out = rule.re.replace_all(s, |c: &Captures| {
                let whole = c.get(0).map_or("", |m| m.as_str());
                let Some(secret) = c.name("s").or_else(|| c.get(0)) else { return whole.to_owned() };
                if secret.as_str().contains(MARKER) || (rule.check_key && !self.key_value_is_secret(c, secret.as_str())) {
                    return whole.to_owned();
                }
                count += 1;
                let start = c.get(0).map_or(0, |m| m.start());
                let marker = if c.name("s").is_some() { rule.marker } else { MARKER };
                format!("{}{marker}{}", &whole[..secret.start() - start], &whole[secret.end() - start..])
            });
            if count > 0 {
                *s = out.into_owned();
                *n += count;
            }
        }
    }

    fn key_value_is_secret(&self, c: &Captures, value: &str) -> bool {
        let key = c.name("k").map_or("", |m| m.as_str());
        if self.class(key) != Class::Secret {
            return false;
        }
        // Placeholders and empty-ish values say nothing: `password = ?`, `token: null`.
        let v = value.trim_end_matches(['.', ':']);
        !(v.is_empty()
            || v == "?"
            || (v.starts_with('$') && v[1..].chars().all(|c| c.is_ascii_digit()))
            || (v.starts_with(':') && v.len() > 1)
            || ["null", "none", "nil", "undefined", "true", "false", "required", "missing", "invalid", "empty"].iter().any(|w| v.eq_ignore_ascii_case(w)))
    }

    /// `sql.query` bindings whose column looks secret: `WHERE password = ?`, `INSERT INTO users
    /// (email, password) VALUES (?, ?)`.
    fn sql_bindings(&self, payload: &mut Value, n: &mut usize) {
        let Some(sql) = payload.get("sql").and_then(Value::as_str) else { return };
        let cols = placeholder_columns(sql);
        let Some(Value::Array(b)) = payload.get_mut("bindings") else { return };
        for (i, col) in cols {
            if self.class(&col) == Class::Secret
                && let Some(v) = b.get_mut(i)
            {
                hide(v, n, self, &col);
            }
        }
    }
}

/// Replaces a value held by a secret key. A URL keeps all but its password; a DSN without one
/// stays, it names a host and a database.
fn hide(v: &mut Value, n: &mut usize, r: &Redactor, key: &str) {
    match v {
        Value::String(s) if s.is_empty() || s.contains(MARKER) => {}
        Value::String(s) if s.contains("://") => {
            let before = *n;
            r.scrub(s, n);
            if *n == before && !norm(key).ends_with("dsn") && !norm(key).ends_with("url") {
                *s = MARKER.to_owned();
                *n += 1;
            }
        }
        Value::String(_) | Value::Number(_) => {
            *v = Value::String(MARKER.to_owned());
            *n += 1;
        }
        Value::Array(a) => a.iter_mut().for_each(|x| hide(x, n, r, key)),
        Value::Object(m) => m.values_mut().for_each(|x| hide(x, n, r, key)),
        Value::Bool(_) | Value::Null => {}
    }
}

/// For each positional placeholder (`?`, `$1`) in `sql`, its binding index and the column it is
/// compared with or inserted into, where that can be told.
fn placeholder_columns(sql: &str) -> Vec<(usize, String)> {
    static INSERT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?is)^\s*(?:insert|replace)\s+(?:ignore\s+)?into\s+[^\s(]+\s*\(([^)]*)\)\s*values\s*").expect("valid"));
    let (cols, values_at): (Vec<String>, usize) = match INSERT.captures(sql) {
        Some(c) => (
            c[1].split(',').map(|x| x.trim().trim_matches(['`', '"', '[', ']']).to_owned()).collect(),
            c.get(0).map_or(usize::MAX, |m| m.end()),
        ),
        None => (Vec::new(), usize::MAX),
    };
    let bytes = sql.as_bytes();
    let mut out = Vec::new();
    let (mut seq, mut depth, mut item) = (0usize, 0i32, 0usize);
    let mut quote: Option<u8> = None;
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if let Some(q) = quote {
            if c == q {
                quote = None;
            }
            i += 1;
            continue;
        }
        let in_values = i >= values_at;
        match c {
            b'\'' | b'"' | b'`' => quote = Some(c),
            b'(' if in_values => {
                depth += 1;
                if depth == 1 {
                    item = 0;
                }
            }
            b')' if in_values => depth -= 1,
            b',' if in_values && depth == 1 => item += 1,
            b'?' | b'$' => {
                let (idx, len) = if c == b'?' {
                    if bytes.get(i + 1).is_some_and(|x| matches!(x, b'?' | b'|' | b'&')) {
                        i += 2;
                        continue;
                    }
                    seq += 1;
                    (seq - 1, 1)
                } else {
                    let digits = bytes[i + 1..].iter().take_while(|x| x.is_ascii_digit()).count();
                    match sql[i + 1..i + 1 + digits].parse::<usize>() {
                        Ok(k) if k > 0 => (k - 1, 1 + digits),
                        _ => {
                            i += 1;
                            continue;
                        }
                    }
                };
                let col = if in_values && depth == 1 { cols.get(item).cloned() } else { compared_column(&sql[..i]) };
                if let Some(col) = col {
                    out.push((idx, col));
                }
                i += len;
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    out
}

/// `... password = ` → `password`.
fn compared_column(before: &str) -> Option<String> {
    let t = before.trim_end();
    let t = t.strip_suffix("!=").or_else(|| t.strip_suffix("<>")).or_else(|| t.strip_suffix('='))?.trim_end();
    let start = t.rfind(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '`' | '"' | '[' | ']'))).map_or(0, |p| p + 1);
    let name = t[start..].rsplit('.').next()?.trim_matches(['`', '"', '[', ']']);
    (!name.is_empty()).then(|| name.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn r() -> Redactor {
        Redactor::default()
    }

    fn text(s: &str) -> (String, usize) {
        let mut s = s.to_owned();
        let n = r().redact_str(&mut s);
        (s, n)
    }

    fn value(v: Value) -> (Value, usize) {
        let mut v = v;
        let n = r().redact_value(&mut v);
        (v, n)
    }

    #[test]
    fn secret_keys_at_any_depth() {
        let (v, n) = value(json!({
            "user": {"email": "a@b.c", "Password": "hunter2", "profile": {"API_KEY": "abc123", "apiKey": 7}},
            "headers": {"Authorization": "Basic dXNlcjpwYXNz", "Set-Cookie": "sid=1", "x-api-key": "k1"},
            "client_secret": "s", "private_key": "p", "aws_access_key_id": "id", "credentials": {"user": "u", "pass": "p"},
            "db_passwd": "x", "access_token": "t", "refresh_token": "t2", "pass": "p"
        }));
        assert_eq!(v["user"]["email"], "a@b.c");
        for p in ["/user/Password", "/user/profile/API_KEY", "/user/profile/apiKey", "/headers/Authorization", "/headers/Set-Cookie", "/headers/x-api-key",
            "/client_secret", "/private_key", "/aws_access_key_id", "/credentials/user", "/credentials/pass", "/db_passwd", "/access_token", "/refresh_token", "/pass"] {
            assert_eq!(v.pointer(p), Some(&json!(MARKER)), "{p}");
        }
        assert_eq!(n, 15);
    }

    #[test]
    fn non_secrets_stay() {
        let v = json!({
            "token_count": 42, "max_tokens": 1024, "usage": {"input_tokens": 10}, "tokens": [1, 2],
            "session_id": "abc", "request_id": "r1", "password_length": 8, "token_type": "Bearer",
            "next_token": "cursor", "nextPageToken": "c2", "has_password": true, "password_hint": null, "key": "cache:users:1",
            "password": "", "tokenizer": "cl100k"
        });
        let (w, n) = value(v.clone());
        assert_eq!((w, n), (v, 0));
    }

    #[test]
    fn header_and_attribute_pairs() {
        let (v, n) = value(json!({
            "headers": [["Authorization", "Bearer abc.def"], ["Accept", "*/*"]],
            "attributes": [{"key": "http.request.header.cookie", "value": {"stringValue": "a=b"}}, {"key": "http.method", "value": "GET"}],
            "har": [{"name": "X-Auth-Token", "value": "zzz"}]
        }));
        assert_eq!(v["headers"][0][1], MARKER);
        assert_eq!(v["headers"][1][1], "*/*");
        assert_eq!(v["attributes"][0]["value"]["stringValue"], MARKER);
        assert_eq!(v["attributes"][1]["value"], "GET");
        assert_eq!(v["har"][0]["value"], MARKER);
        assert_eq!(n, 3);
    }

    #[test]
    fn each_shape() {
        let cases = [
            ("Authorization: Bearer abcdefghijklmnop1234", "Authorization: [redacted]"),
            ("curl -H 'Bearer abcdefghijklmnop1234'", "curl -H 'Bearer [redacted]'"),
            ("jwt eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0In0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U ok", "jwt eyJ…[redacted] ok"),
            ("key AKIAIOSFODNN7EXAMPLE end", "key AKIA…[redacted] end"),
            ("ghp_abcdefghijklmnopqrstuvwxyz0123456789", "ghp_…[redacted]"),
            ("gho_abcdefghijklmnopqrstuvwxyz0123456789", "gho_…[redacted]"),
            ("github_pat_11ABCDEFG0123456789_abcdefghijklmnop", "github_pat_…[redacted]"),
            ("xoxb-123456789012-abcdefghij", "xoxb-…[redacted]"),
            ("sk_live_51Habcdefghijklmn", "sk_live_…[redacted]"),
            ("sk_test_51Habcdefghijklmn", "sk_test_…[redacted]"),
            ("rk_live_51Habcdefghijklmn", "rk_live_…[redacted]"),
            ("sk-proj-abcdefghijklmnopqrstuvwxyz", "sk-proj-…[redacted]"),
            ("sk-ant-api03-abcdefghijklmnopqrstuvwxyz_-01", "sk-ant-api03-…[redacted]"),
            ("sk-abcdefghijklmnopqrstuvwxyz012345", "sk-…[redacted]"),
            ("AIzaSyA1234567890abcdefghijklmnopqrstuv", "AIza…[redacted]"),
            ("postgres://app:s3cr3t@db.local:5432/shop", "postgres://app:[redacted]@db.local:5432/shop"),
            ("redis://:pw@cache:6379", "redis://:[redacted]@cache:6379"),
            ("-----BEGIN RSA PRIVATE KEY-----\nMIIEow\nabc\n-----END RSA PRIVATE KEY-----", "-----BEGIN RSA PRIVATE KEY-----[redacted]-----END RSA PRIVATE KEY-----"),
            ("-----BEGIN PRIVATE KEY-----\nMIIE", "-----BEGIN PRIVATE KEY-----[redacted]"),
        ];
        for (input, want) in cases {
            let (got, n) = text(input);
            assert_eq!(got, want, "{input}");
            assert_eq!(n, 1, "{input}");
        }
    }

    #[test]
    fn log_lines() {
        assert_eq!(text("login ok user=bob password=hunter2 ip=1.2.3.4").0, "login ok user=bob password=[redacted] ip=1.2.3.4");
        assert_eq!(text("GET /cb?code=1&access_token=abc123&state=x").0, "GET /cb?code=1&access_token=[redacted]&state=x");
        assert_eq!(text(r#"body {"email":"a@b","password":"hunter2"}"#).0, r#"body {"email":"a@b","password":"[redacted]"}"#);
        assert_eq!(text("Cookie: sid=abc; theme=dark").0, "Cookie: [redacted]");
        assert_eq!(text("DB_PASSWORD: s3cret").0, "DB_PASSWORD: [redacted]");
        // Not secret: counters, prose, placeholders.
        for s in ["used token_count=42 max_tokens=100", "password reset email sent", "password: null", "the password field is required", "next_token=abc"] {
            assert_eq!(text(s), (s.to_owned(), 0), "{s}");
        }
    }

    #[test]
    fn exception_messages() {
        let (m, n) = text("SQLSTATE[08006] could not connect to postgres://shop:Pa55w0rd@10.0.0.5/shop");
        assert_eq!((m.as_str(), n), ("SQLSTATE[08006] could not connect to postgres://shop:[redacted]@10.0.0.5/shop", 1));
        let (m, _) = text("Incorrect API key provided: sk-proj-abcdefghijklmnopqrstuvwx. Find yours at ...");
        assert_eq!(m, "Incorrect API key provided: sk-proj-…[redacted]. Find yours at ...");
    }

    #[test]
    fn sql_text_and_bindings() {
        let mut p = json!({"sql": "UPDATE users SET password = '$2y$10$abc' WHERE id = 1"});
        let mut meta = Meta::default();
        let n = r().redact("sql.query", &mut p, &mut meta, &mut Origin::default());
        assert_eq!(p["sql"], "UPDATE users SET password = '[redacted]' WHERE id = 1");
        assert_eq!(n, 1);
        assert_eq!(text("CREATE USER app IDENTIFIED BY 'pw1'").0, "CREATE USER app IDENTIFIED BY '[redacted]'");
        assert_eq!(text("ALTER ROLE app WITH PASSWORD 'pw1'").0, "ALTER ROLE app WITH PASSWORD '[redacted]'");

        let mut p = json!({"sql": "INSERT INTO `users` (`email`, `password`, created_at) VALUES (?, ?, NOW()), (?, ?, NOW())", "bindings": ["a@b", "h1", "c@d", "h2"]});
        assert_eq!(r().redact("sql.query", &mut p, &mut Meta::default(), &mut Origin::default()), 2);
        assert_eq!(p["bindings"], json!(["a@b", MARKER, "c@d", MARKER]));

        let mut p = json!({"sql": "select * from users where email = ? and users.password_hash = ? and api_token=?", "bindings": ["a@b", "h", "t"]});
        r().redact("sql.query", &mut p, &mut Meta::default(), &mut Origin::default());
        assert_eq!(p["bindings"], json!(["a@b", MARKER, MARKER]));
        // The SQL text itself keeps its placeholders.
        assert_eq!(p["sql"], "select * from users where email = ? and users.password_hash = ? and api_token=?");
        let mut p = json!({"sql": "select 1 from t where \"password\" <> $2 and name = $1 and note = 'a ? b'", "bindings": ["n", "pw"]});
        r().redact("sql.query", &mut p, &mut Meta::default(), &mut Origin::default());
        assert_eq!(p["bindings"], json!(["n", MARKER]));
    }

    #[test]
    fn urls_under_secret_keys_keep_their_host() {
        let (v, n) = value(json!({"dsn": "mysql://root:pw@localhost/app", "sentry_dsn": "https://pub@o1.ingest.sentry.io/1", "database_password": "https://x"}));
        assert_eq!(v["dsn"], "mysql://root:[redacted]@localhost/app");
        assert_eq!(v["sentry_dsn"], "https://pub@o1.ingest.sentry.io/1");
        assert_eq!(v["database_password"], MARKER);
        assert_eq!(n, 2);
    }

    #[test]
    fn meta_and_origin() {
        let mut meta: Meta = serde_json::from_value(json!({"label": "token=abcdef", "tags": ["x"], "auth": {"api_key": "k"}})).unwrap();
        let mut origin: Origin = serde_json::from_value(json!({"argv": ["app", "--password=hunter2"]})).unwrap();
        let n = r().redact("core.value", &mut json!(1), &mut meta, &mut origin);
        assert_eq!(meta.label.as_deref(), Some("token=[redacted]"));
        assert_eq!(meta.extra["auth"]["api_key"], MARKER);
        assert_eq!(origin.extra["argv"][1], "--password=[redacted]");
        assert_eq!(n, 3);
    }

    #[test]
    fn configuration() {
        let cfg = RedactConfig { enabled: true, keys: vec!["ssn".into(), "pin".into()], patterns: vec![r"\bACME-\d{6}\b".into(), r"card (?P<s>\d{4})".into()], allow: vec!["csrf_token".into(), "Cookie".into()] };
        let r = Redactor::new(&cfg).unwrap();
        let mut v = json!({"user_ssn": "123", "PIN": 1234, "spinner": "x", "csrf_token": "keep", "cookie": "keep",
            "note": "order ACME-123456 paid with card 4242 ssn=999", "auth": {"cookie": {"password": "inside allowlisted is kept"}}});
        let n = r.redact_value(&mut v);
        assert_eq!(v["user_ssn"], MARKER);
        assert_eq!(v["PIN"], MARKER);
        assert_eq!(v["spinner"], "x");
        assert_eq!(v["csrf_token"], "keep");
        assert_eq!(v["cookie"], "keep");
        assert_eq!(v["note"], "order [redacted] paid with card [redacted] ssn=[redacted]");
        assert_eq!(v["auth"]["cookie"]["password"], "inside allowlisted is kept");
        assert_eq!(n, 5);
        assert!(Redactor::new(&RedactConfig { patterns: vec!["(".into()], ..RedactConfig::default() }).is_err());
        // Short strings too, once there are extra patterns.
        let short = Redactor::new(&RedactConfig { patterns: vec!["bob(?P<s>by)".into()], ..RedactConfig::default() }).unwrap();
        let mut v = json!("bobby");
        assert_eq!((short.redact_value(&mut v), v), (1, json!("bob[redacted]")));
        let off = Redactor::new(&RedactConfig { enabled: false, ..RedactConfig::default() }).unwrap();
        let mut v = json!({"password": "x"});
        assert_eq!(off.redact_value(&mut v), 0);
        assert_eq!(v["password"], "x");
    }

    #[test]
    fn idempotent() {
        let (once, _) = value(json!({"password": "x", "log": "password=y sk-abcdefghijklmnopqrstuvwxyz012345 postgres://u:p@h/d"}));
        let (twice, n) = value(once.clone());
        assert_eq!((twice, n), (once, 0));
    }

    #[test]
    fn a_large_payload_takes_about_a_millisecond() {
        let items: Vec<Value> = (0..60)
            .map(|i| json!({
                "id": i, "name": format!("item {i}"), "price": 9.99, "tags": ["a", "b", "c"],
                "description": "A fairly long description of the product, with a few sentences of text in it so the scanner has work to do on every item.",
                "owner": {"email": "x@y.z", "password": "pw", "token_count": 3},
                "log": format!("GET /api/items/{i}?page=2 200 12ms user=bob"),
            }))
            .collect();
        let payload = json!({"items": items});
        let r = Redactor::default();
        let mut best = std::time::Duration::MAX;
        for _ in 0..20 {
            let mut v = payload.clone();
            let t = std::time::Instant::now();
            let n = r.redact_value(&mut v);
            best = best.min(t.elapsed());
            assert_eq!(n, 60);
        }
        eprintln!("60-item payload redacted in {best:?}");
        // Generous for debug builds; a release build takes well under a millisecond.
        assert!(best < std::time::Duration::from_millis(if cfg!(debug_assertions) { 30 } else { 3 }), "{best:?}");
    }
}
