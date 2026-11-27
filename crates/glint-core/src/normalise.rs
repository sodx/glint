use std::sync::Arc;

use chrono::{DateTime, Utc};
use glint_protocol::{Envelope, IncomingEnvelope, PROTOCOL_VERSION};
use serde_json::Value;
use ulid::Generator;

use crate::project::{ProjectResolver, folder_name};
use crate::redact::Redactor;

/// Fills in what a client left out. Server-assigned ids are monotonic ULIDs, so they sort
/// in arrival order. Secrets are redacted here, so nothing downstream ever sees them.
pub struct Normaliser {
    ids: Generator,
    projects: ProjectResolver,
    redactor: Arc<Redactor>,
}

impl Default for Normaliser {
    fn default() -> Self {
        Self { ids: Generator::default(), projects: ProjectResolver::default(), redactor: Redactor::shared_default() }
    }
}

impl Normaliser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replaces the redaction rules for the events that follow.
    pub fn set_redactor(&mut self, r: Arc<Redactor>) {
        self.redactor = r;
    }

    pub fn normalise(&mut self, mut e: IncomingEnvelope, now: DateTime<Utc>) -> Envelope {
        // Code in a container reports container paths; the client says which host folders they
        // are (`origin.path_map`), so files open in the editor and the project is found.
        if let Some(Value::String(map)) = e.origin.extra.remove("path_map") {
            apply_path_map(&mut e, &map);
        }
        let id = e.id.filter(|s| !s.is_empty()).unwrap_or_else(|| {
            // The generator only fails after 2^80 ids in one millisecond.
            self.ids.generate().map(|u| u.to_string()).unwrap_or_else(|_| ulid::Ulid::new().to_string())
        });
        let run = e.run.filter(|s| !s.is_empty()).unwrap_or_else(|| {
            match (&e.origin.host, e.origin.pid) {
                (Some(h), Some(p)) => format!("{h}:{p}"),
                (None, Some(p)) => format!("pid:{p}"),
                _ => "default".to_owned(),
            }
        });
        // An exception reported by Glint's own global handlers (`glint install`) or with no call
        // site is located where it was thrown: its first app frame.
        let from_glint = |f: &str| f.contains("/.glint/sdk/") || f.contains("\\.glint\\sdk\\");
        if e.kind == "core.exception" && e.origin.file.as_deref().is_none_or(from_glint) {
            let frame = e.payload.get("frames").and_then(Value::as_array).and_then(|fs| {
                fs.iter().find(|f| f.get("vendor") != Some(&Value::Bool(true)) && f.get("file").and_then(Value::as_str).is_some_and(|x| !from_glint(x)))
            });
            if let Some(f) = frame {
                e.origin.file = f.get("file").and_then(Value::as_str).map(str::to_owned);
                e.origin.line = f.get("line").and_then(Value::as_u64).and_then(|l| u32::try_from(l).ok());
                e.origin.function = f.get("fn").and_then(Value::as_str).map(str::to_owned);
            }
        }
        // The project is the git repository, so all worktrees of one repository share it (see
        // project.rs): looked up from the calling file's folder, then the cwd, else the cwd's name.
        let file_dir = e.origin.file.as_deref().filter(|f| f.starts_with('/') || f.contains(":\\")).and_then(parent);
        let resolved = file_dir
            .and_then(|d| self.projects.resolve(d))
            .or_else(|| e.origin.cwd.as_deref().and_then(|d| self.projects.resolve(d)));
        if let Some(r) = &resolved {
            for (k, v) in [("worktree", &r.worktree), ("branch", &r.branch)] {
                if let Some(v) = v {
                    e.origin.extra.entry(k.to_owned()).or_insert_with(|| Value::String(v.clone()));
                }
            }
        }
        let project = e
            .project
            .take()
            .filter(|s| !s.is_empty())
            .or_else(|| resolved.map(|r| r.project))
            .or_else(|| e.origin.cwd.as_deref().and_then(folder_name))
            .unwrap_or_else(|| "default".to_owned());
        // `meta.redacted` tells the window how many values were hidden.
        let hidden = self.redactor.redact(&e.kind, &mut e.payload, &mut e.meta, &mut e.origin);
        if hidden > 0 {
            e.meta.extra.insert("redacted".to_owned(), Value::from(hidden));
        }
        Envelope {
            v: PROTOCOL_VERSION,
            id,
            ts: e.ts.unwrap_or(now),
            run,
            parent: e.parent.filter(|s| !s.is_empty()),
            project,
            origin: e.origin,
            kind: e.kind,
            payload: e.payload,
            meta: e.meta,
        }
    }
}

/// `"/var/www/html:/Users/me/shop,/opt/lib:/Users/me/lib"` → pairs, longest container path
/// first. Split at the first colon: container paths are POSIX, a host path may be `C:\\…`.
fn parse_path_map(map: &str) -> Vec<(&str, &str)> {
    let mut pairs: Vec<(&str, &str)> = map
        .split(',')
        .filter_map(|p| p.trim().split_once(':'))
        .map(|(c, h)| (c.trim().trim_end_matches('/'), h.trim().trim_end_matches(['/', '\\'])))
        .filter(|(c, h)| !c.is_empty() && !h.is_empty())
        .collect();
    pairs.sort_by_key(|(c, _)| std::cmp::Reverse(c.len()));
    pairs
}

fn map_path(pairs: &[(&str, &str)], path: &str) -> Option<String> {
    pairs.iter().find_map(|(c, h)| {
        let rest = path.strip_prefix(c).filter(|r| r.is_empty() || r.starts_with('/'))?;
        let rest = if h.contains('\\') { rest.replace('/', "\\") } else { rest.to_owned() };
        Some(format!("{h}{rest}"))
    })
}

fn apply_path_map(e: &mut IncomingEnvelope, map: &str) {
    let pairs = parse_path_map(map);
    if pairs.is_empty() {
        return;
    }
    for p in [&mut e.origin.file, &mut e.origin.cwd] {
        if let Some(mapped) = p.as_deref().and_then(|f| map_path(&pairs, f)) {
            *p = Some(mapped);
        }
    }
    if let Some(frames) = e.payload.get_mut("frames").and_then(Value::as_array_mut) {
        for f in frames {
            if let Some(mapped) = f.get("file").and_then(Value::as_str).and_then(|x| map_path(&pairs, x)) {
                f["file"] = Value::String(mapped);
            }
        }
    }
}

fn parent(file: &str) -> Option<&str> {
    file.rsplit_once(['/', '\\']).map(|(d, _)| d).filter(|d| !d.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use glint_protocol::{BodyFormat, parse_body};

    fn incoming(json: &str) -> IncomingEnvelope {
        parse_body(json.as_bytes(), BodyFormat::Json).unwrap().remove(0).unwrap()
    }

    #[test]
    fn redacts_secrets_and_counts_them() {
        let mut n = Normaliser::new();
        let e = n.normalise(incoming(r#"{"kind":"core.value","payload":{"password":"hunter2","key":"sk-ant-api03-abcdefghijklmnopqrstuvwxyz","n":1}}"#), Utc::now());
        assert_eq!(e.payload["password"], "[redacted]");
        assert_eq!(e.payload["key"], "sk-ant-api03-…[redacted]");
        assert_eq!(e.meta.extra["redacted"], 2);
        n.set_redactor(Arc::new(Redactor::new(&crate::RedactConfig { enabled: false, ..Default::default() }).unwrap()));
        let e = n.normalise(incoming(r#"{"kind":"core.value","payload":{"password":"hunter2"}}"#), Utc::now());
        assert_eq!(e.payload["password"], "hunter2");
        assert!(!e.meta.extra.contains_key("redacted"));
    }

    #[test]
    fn fills_defaults() {
        let now = Utc::now();
        let e = Normaliser::new().normalise(incoming(r#"{"kind":"core.value","payload":1}"#), now);
        assert_eq!(e.v, 1);
        assert_eq!(e.id.len(), 26);
        assert_eq!(e.ts, now);
        assert_eq!(e.run, "default");
        assert_eq!(e.project, "default");
    }

    #[test]
    fn derives_run_and_project_from_origin() {
        let e = Normaliser::new().normalise(
            incoming(r#"{"kind":"core.value","payload":1,"origin":{"host":"mbp","pid":7,"cwd":"/src/shop-api/"}}"#),
            Utc::now(),
        );
        assert_eq!(e.run, "mbp:7");
        assert_eq!(e.project, "shop-api");
    }

    #[test]
    fn keeps_client_values_and_orders_server_ids() {
        let mut n = Normaliser::new();
        let e = n.normalise(incoming(r#"{"id":"mine","run":"r1","project":"p","kind":"core.value","payload":1}"#), Utc::now());
        assert_eq!((e.id.as_str(), e.run.as_str(), e.project.as_str()), ("mine", "r1", "p"));
        let a = n.normalise(incoming(r#"{"kind":"core.value","payload":1}"#), Utc::now()).id;
        let b = n.normalise(incoming(r#"{"kind":"core.value","payload":1}"#), Utc::now()).id;
        assert!(a < b);
    }

    #[test]
    fn worktrees_share_their_repository_project() {
        let e = Normaliser::new().normalise(
            incoming(r#"{"kind":"core.value","payload":1,"origin":{"cwd":"/home/u","file":"/srv/shop/.claude/worktrees/feat-x/src/cart.ts"}}"#),
            Utc::now(),
        );
        assert_eq!(e.project, "shop");
        assert_eq!(e.origin.extra.get("worktree").and_then(Value::as_str), Some("feat-x"));
    }

    #[test]
    fn path_map_turns_container_paths_into_host_paths() {
        let e = Normaliser::new().normalise(
            incoming(r#"{"kind":"core.exception","payload":{"class":"E","message":"m","frames":[
                {"file":"/opt/.glint/sdk/php/autoload.php","line":40},
                {"file":"/var/www/html/src/pay.php","line":7,"fn":"pay"},
                {"file":"/var/www/htmlx/other.php","line":1}]},
              "origin":{"cwd":"/var/www/html","file":"/opt/.glint/sdk/php/autoload.php","line":40,
                "path_map":"/var/www:/Users/me/www, /var/www/html/:/Users/me/shop/"}}"#),
            Utc::now(),
        );
        assert_eq!(e.origin.file.as_deref(), Some("/Users/me/shop/src/pay.php"));
        assert_eq!(e.origin.cwd.as_deref(), Some("/Users/me/shop"));
        assert_eq!(e.payload["frames"][2]["file"], "/Users/me/www/htmlx/other.php");
        assert_eq!(e.payload["frames"][0]["file"], "/opt/.glint/sdk/php/autoload.php");
        assert!(!e.origin.extra.contains_key("path_map"));
        assert_eq!(e.project, "shop");
    }

    #[test]
    fn path_map_to_a_windows_host() {
        let pairs = parse_path_map("/app:C:\\src\\shop");
        assert_eq!(map_path(&pairs, "/app/lib/a.py").as_deref(), Some("C:\\src\\shop\\lib\\a.py"));
        assert_eq!(map_path(&pairs, "/application/a.py"), None);
        assert!(parse_path_map("nonsense, :/x, /y:").is_empty());
    }

    #[test]
    fn exceptions_from_glints_global_handlers_take_their_throw_site() {
        let e = Normaliser::new().normalise(
            incoming(r#"{"kind":"core.exception","payload":{"class":"E","message":"m","frames":[
                {"file":"/home/u/.glint/sdk/php/autoload.php","line":24},
                {"file":"/srv/shop/vendor/lib.php","line":3,"vendor":true},
                {"file":"/srv/shop/pay.php","line":7,"fn":"pay"}]},
              "origin":{"file":"/home/u/.glint/sdk/php/autoload.php","line":24}}"#),
            Utc::now(),
        );
        assert_eq!((e.origin.file.as_deref(), e.origin.line, e.origin.function.as_deref()), (Some("/srv/shop/pay.php"), Some(7), Some("pay")));
    }
}
