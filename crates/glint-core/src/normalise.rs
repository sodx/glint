use chrono::{DateTime, Utc};
use glint_protocol::{Envelope, IncomingEnvelope, PROTOCOL_VERSION};
use serde_json::Value;
use ulid::Generator;

use crate::project::{ProjectResolver, folder_name};

/// Fills in what a client left out. Server-assigned ids are monotonic ULIDs, so they sort
/// in arrival order.
#[derive(Default)]
pub struct Normaliser {
    ids: Generator,
    projects: ProjectResolver,
}

impl Normaliser {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn normalise(&mut self, mut e: IncomingEnvelope, now: DateTime<Utc>) -> Envelope {
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
