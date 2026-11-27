//! Glint's settings: `~/.glint/config.toml` (or the file `GLINT_CONFIG` names), each value
//! overridable by an environment variable. glintd reads them at start and changes them live from
//! the Settings page (`PUT /v1/settings`); `glint hook` reads the agent switches from the file.
//!
//! The file is plain `key = value` lines (a TOML subset: numbers, booleans, "strings"; `#`
//! comments), so it stays easy to edit by hand too.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    /// Events older than this many days are deleted; 0 keeps them.
    pub keep_days: u32,
    /// At most this many events are kept, the oldest go first; 0 means no cap.
    pub max_events: u64,
    /// Where files open; empty: the first installed editor.
    pub editor: String,
    /// Coding agents hear about new failures with each prompt.
    pub agent_context: bool,
    /// A coding agent that wants to stop while programs failed is sent back once.
    pub stop_guard: bool,
    pub addr: String,
    pub ray_addr: String,
    pub otlp_addr: String,
    pub db: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            keep_days: 7,
            max_events: 100_000,
            editor: String::new(),
            agent_context: true,
            stop_guard: true,
            addr: "127.0.0.1:23600".into(),
            ray_addr: "127.0.0.1:23517".into(),
            otlp_addr: "127.0.0.1:4318".into(),
            db: "~/.glint/glint.db".into(),
        }
    }
}

/// Key, environment variable, and whether a change needs a restart of glintd.
pub const KEYS: &[(&str, &str, bool)] = &[
    ("keep_days", "GLINT_KEEP_DAYS", false),
    ("max_events", "GLINT_MAX_EVENTS", false),
    ("editor", "GLINT_EDITOR", false),
    ("agent_context", "GLINT_AGENT_CONTEXT", false),
    ("stop_guard", "GLINT_STOP_GUARD", false),
    ("addr", "GLINT_ADDR", true),
    ("ray_addr", "GLINT_RAY_ADDR", true),
    ("otlp_addr", "GLINT_OTLP_ADDR", true),
    ("db", "GLINT_DB", true),
];

pub fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from)
}

pub fn path() -> Option<PathBuf> {
    std::env::var_os("GLINT_CONFIG").map(PathBuf::from).or_else(|| home().map(|h| h.join(".glint/config.toml")))
}

/// `~/x` → the home directory joined with `x`.
pub fn expand(p: &str) -> PathBuf {
    match (p.strip_prefix("~/"), home()) {
        (Some(rest), Some(h)) => h.join(rest),
        _ => PathBuf::from(p),
    }
}

pub fn parse(text: &str) -> HashMap<String, String> {
    text.lines()
        .filter_map(|l| {
            let l = l.split_once('#').map_or(l, |(a, _)| if a.matches('"').count() % 2 == 0 { a } else { l }).trim();
            let (k, v) = l.split_once('=')?;
            let v = v.trim();
            let v = v.strip_prefix('"').and_then(|x| x.strip_suffix('"')).unwrap_or(v);
            Some((k.trim().to_owned(), v.to_owned())).filter(|(k, _)| !k.is_empty() && !k.starts_with('['))
        })
        .collect()
}

/// The settings as the file says, ignoring the environment.
pub fn load_file() -> Settings {
    let mut s = Settings::default();
    if let Some(text) = path().and_then(|p| std::fs::read_to_string(p).ok()) {
        for (k, v) in parse(&text) {
            s.set(&k, &v);
        }
    }
    s
}

/// The settings in effect, and which keys an environment variable sets (and so locks).
pub fn load() -> (Settings, Vec<String>) {
    let file = path().and_then(|p| std::fs::read_to_string(p).ok()).map(|t| parse(&t)).unwrap_or_default();
    let mut s = Settings::default();
    let mut from_env = Vec::new();
    for (key, env, _) in KEYS {
        let value = match std::env::var(env).ok().filter(|v| !v.is_empty()) {
            Some(v) => {
                from_env.push((*key).to_owned());
                v
            }
            None => match file.get(*key) {
                Some(v) => v.clone(),
                None => continue,
            },
        };
        s.set(key, &value);
    }
    (s, from_env)
}

fn truthy(v: &str) -> Option<bool> {
    match v.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

impl Settings {
    /// Sets one key from text; unknown keys and unparsable values are ignored.
    pub fn set(&mut self, key: &str, v: &str) {
        match key {
            "keep_days" => self.keep_days = v.trim().parse().unwrap_or(self.keep_days),
            "max_events" => self.max_events = v.trim().parse().unwrap_or(self.max_events),
            "editor" => self.editor = v.trim().to_owned(),
            "agent_context" => self.agent_context = truthy(v).unwrap_or(self.agent_context),
            "stop_guard" => self.stop_guard = truthy(v).unwrap_or(self.stop_guard),
            "addr" => self.addr = v.trim().to_owned(),
            "ray_addr" => self.ray_addr = v.trim().to_owned(),
            "otlp_addr" => self.otlp_addr = v.trim().to_owned(),
            "db" => self.db = v.trim().to_owned(),
            _ => {}
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.keep_days > 3650 {
            return Err("keep_days: at most 3650".into());
        }
        if self.max_events != 0 && self.max_events < 1000 {
            return Err("max_events: at least 1000, or 0 for no cap".into());
        }
        for (k, a) in [("addr", &self.addr), ("ray_addr", &self.ray_addr), ("otlp_addr", &self.otlp_addr)] {
            if !(k != "addr" && a == "off") && a.parse::<std::net::SocketAddr>().is_err() {
                return Err(format!("{k}: expected host:port{}", if k == "addr" { "" } else { " or off" }));
            }
        }
        Ok(())
    }

    /// The file, with a comment on each key.
    pub fn to_file(&self) -> String {
        format!(
            "# Glint's settings. The Settings page in the Glint window edits this file; you can too.\n\
             # Each key can also be set by the environment variable named next to it, which wins.\n\
             \n\
             # Events older than this many days are deleted (0 keeps everything).          GLINT_KEEP_DAYS\n\
             keep_days = {}\n\
             # At most this many events are kept; the oldest go first (0: no cap).         GLINT_MAX_EVENTS\n\
             max_events = {}\n\
             # Where files open: vscode, cursor, windsurf, zed, phpstorm, idea, webstorm, pycharm,\n\
             # sublime. Empty: the first one installed.                                    GLINT_EDITOR\n\
             editor = \"{}\"\n\
             # Coding agents hear about new failures with each prompt.                     GLINT_AGENT_CONTEXT\n\
             agent_context = {}\n\
             # An agent that wants to finish while programs failed is sent back once.      GLINT_STOP_GUARD\n\
             stop_guard = {}\n\
             \n\
             # These take effect when glintd restarts (glint install --only service).\n\
             addr = \"{}\"            # GLINT_ADDR\n\
             ray_addr = \"{}\"        # GLINT_RAY_ADDR   (\"off\" to disable)\n\
             otlp_addr = \"{}\"        # GLINT_OTLP_ADDR  (\"off\" to disable)\n\
             db = \"{}\"         # GLINT_DB\n",
            self.keep_days, self.max_events, self.editor, self.agent_context, self.stop_guard, self.addr, self.ray_addr, self.otlp_addr, self.db
        )
    }

    pub fn save(&self) -> Result<PathBuf, String> {
        self.validate()?;
        let p = path().ok_or("no home directory")?;
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
        }
        let tmp = p.with_extension("toml.new");
        std::fs::write(&tmp, self.to_file()).and_then(|()| std::fs::rename(&tmp, &p)).map_err(|e| format!("writing {}: {e}", p.display()))?;
        Ok(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_file_round_trips() {
        let mut s = Settings { keep_days: 14, editor: "cursor".into(), stop_guard: false, ..Settings::default() };
        s.validate().unwrap();
        let back = parse(&s.to_file());
        let mut t = Settings::default();
        for (k, v) in &back {
            t.set(k, v);
        }
        assert_eq!(s, t);
        s.max_events = 10;
        assert!(s.validate().is_err());
    }

    #[test]
    fn hand_written_lines() {
        let c = parse("# Glint\nkeep_days = 14\neditor = \"cursor\" # mine\naddr=127.0.0.1:23700\n[section]\n\nbad line\n");
        assert_eq!(c.get("keep_days").map(String::as_str), Some("14"));
        assert_eq!(c.get("editor").map(String::as_str), Some("cursor"));
        assert_eq!(c.len(), 3);
    }
}
