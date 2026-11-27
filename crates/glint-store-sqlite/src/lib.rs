//! SQLite implementation of [`glint_core::Store`]: one file, WAL mode, FTS5 over the string
//! values of each event.
//!
//! Each envelope is stored whole as JSON (`body`) plus the columns filters need. Rows are
//! ordered by `seq`, the insertion order, which is what "oldest first" means everywhere.

use std::path::Path;
use std::sync::Mutex;

use chrono::SecondsFormat;
use glint_core::{Query, RunSummary, Store, StoreError};
use glint_protocol::Envelope;
use rusqlite::{Connection, OpenFlags, params_from_iter, types::Value as Sql};
use serde_json::Value;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS events (
    seq     INTEGER PRIMARY KEY,
    id      TEXT    NOT NULL UNIQUE,
    ts      TEXT    NOT NULL,
    run     TEXT    NOT NULL,
    project TEXT    NOT NULL,
    kind    TEXT    NOT NULL,
    level   INTEGER NOT NULL,
    parent  TEXT,
    body    TEXT    NOT NULL
);
CREATE INDEX IF NOT EXISTS events_project ON events (project, seq);
CREATE INDEX IF NOT EXISTS events_run     ON events (run, seq);
CREATE INDEX IF NOT EXISTS events_kind    ON events (kind, seq);
CREATE VIRTUAL TABLE IF NOT EXISTS events_fts
    USING fts5 (text, content = '', contentless_delete = 1, tokenize = 'unicode61');
"#;

/// Most text indexed per event.
const FTS_TEXT_MAX: usize = 64 * 1024;

pub struct SqliteStore {
    writer: Mutex<Connection>,
    /// A second connection so reads never wait for a write transaction. Absent in memory,
    /// where connections cannot share a database.
    reader: Option<Mutex<Connection>>,
}

impl SqliteStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let path = path.as_ref();
        let writer = Connection::open(path).map_err(backend)?;
        writer
            .execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL; PRAGMA busy_timeout = 5000;")
            .map_err(backend)?;
        writer.execute_batch(SCHEMA).map_err(backend)?;
        let reader = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(backend)?;
        reader.execute_batch("PRAGMA busy_timeout = 5000;").map_err(backend)?;
        Ok(Self {
            writer: Mutex::new(writer),
            reader: Some(Mutex::new(reader)),
        })
    }

    pub fn open_in_memory() -> Result<Self, StoreError> {
        let conn = Connection::open_in_memory().map_err(backend)?;
        conn.execute_batch(SCHEMA).map_err(backend)?;
        Ok(Self {
            writer: Mutex::new(conn),
            reader: None,
        })
    }

    fn read<T>(&self, f: impl FnOnce(&Connection) -> Result<T, StoreError>) -> Result<T, StoreError> {
        let m = self.reader.as_ref().unwrap_or(&self.writer);
        let conn = m.lock().unwrap_or_else(|p| p.into_inner());
        f(&conn)
    }
}

impl Store for SqliteStore {
    fn insert(&self, events: &[Envelope]) -> Result<(), StoreError> {
        let mut conn = self.writer.lock().unwrap_or_else(|p| p.into_inner());
        let tx = conn.transaction().map_err(backend)?;
        {
            // A repeated id is a client retry: the first copy wins.
            let mut ins = tx
                .prepare_cached(
                    "INSERT OR IGNORE INTO events (id, ts, run, project, kind, level, parent, body)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                )
                .map_err(backend)?;
            let mut fts = tx
                .prepare_cached("INSERT INTO events_fts (rowid, text) VALUES (?1, ?2)")
                .map_err(backend)?;
            for e in events {
                let body = serde_json::to_string(e).map_err(|err| StoreError::Backend(err.to_string()))?;
                let inserted = ins
                    .execute(rusqlite::params![
                        e.id,
                        e.ts.to_rfc3339_opts(SecondsFormat::Micros, true),
                        e.run,
                        e.project,
                        e.kind,
                        e.level().rank(),
                        e.parent,
                        body,
                    ])
                    .map_err(backend)?;
                if inserted == 1 {
                    let seq = tx.last_insert_rowid();
                    fts.execute(rusqlite::params![seq, searchable_text(e)]).map_err(backend)?;
                }
            }
        }
        tx.commit().map_err(backend)
    }

    fn query(&self, q: &Query) -> Result<Vec<Envelope>, StoreError> {
        let mut sql = String::from("SELECT body FROM events WHERE 1 = 1");
        let mut args: Vec<Sql> = Vec::new();
        if let Some(p) = &q.project {
            sql.push_str(" AND project = ?");
            args.push(Sql::Text(p.clone()));
        }
        if let Some(r) = &q.run {
            sql.push_str(" AND run = ?");
            args.push(Sql::Text(r.clone()));
        }
        if let Some(k) = &q.kind {
            match k.strip_suffix('*') {
                Some(prefix) => {
                    sql.push_str(" AND kind LIKE ? ESCAPE '\\'");
                    args.push(Sql::Text(format!("{}%", escape_like(prefix))));
                }
                None => {
                    sql.push_str(" AND kind = ?");
                    args.push(Sql::Text(k.clone()));
                }
            }
        }
        if let Some(l) = q.level {
            sql.push_str(" AND level >= ?");
            args.push(Sql::Integer(i64::from(l.rank())));
        }
        if let Some(text) = q.q.as_deref().and_then(fts_query) {
            sql.push_str(" AND seq IN (SELECT rowid FROM events_fts WHERE events_fts MATCH ?)");
            args.push(Sql::Text(text));
        }
        if let Some(after) = &q.after {
            sql.push_str(" AND seq > COALESCE((SELECT seq FROM events WHERE id = ?), 0)");
            args.push(Sql::Text(after.clone()));
        }
        if let Some(before) = &q.before {
            sql.push_str(" AND seq < COALESCE((SELECT seq FROM events WHERE id = ?), 0)");
            args.push(Sql::Text(before.clone()));
        }
        sql.push_str(" ORDER BY seq DESC LIMIT ?");
        args.push(Sql::Integer(i64::try_from(q.limit()).unwrap_or(i64::MAX)));

        self.read(|conn| {
            let mut stmt = conn.prepare_cached(&sql).map_err(backend)?;
            let rows = stmt
                .query_map(params_from_iter(args), |r| r.get::<_, String>(0))
                .map_err(backend)?;
            let mut out = Vec::new();
            for body in rows {
                out.push(decode(&body.map_err(backend)?)?);
            }
            out.reverse();
            Ok(out)
        })
    }

    fn runs(&self, project: Option<&str>, limit: usize) -> Result<Vec<RunSummary>, StoreError> {
        // Bookkeeping events (updates, resumes) don't make a run "active".
        let sql = "SELECT g.run, g.project, g.first, g.last, g.n, g.errors, e.id
                   FROM (SELECT run, MAX(project) AS project, MIN(ts) AS first, MAX(ts) AS last,
                                COUNT(*) AS n, SUM(level >= 4) AS errors, MAX(seq) AS last_seq
                         FROM events
                         WHERE kind NOT IN ('core.update', 'ctl.resume') AND (?1 IS NULL OR project = ?1)
                         GROUP BY run
                         ORDER BY last_seq DESC
                         LIMIT ?2) g
                   JOIN events e ON e.seq = g.last_seq
                   ORDER BY g.last_seq DESC";
        self.read(|conn| {
            let mut stmt = conn.prepare_cached(sql).map_err(backend)?;
            let rows = stmt
                .query_map(rusqlite::params![project, i64::try_from(limit).unwrap_or(i64::MAX)], |r| {
                    Ok(RunSummary {
                        run: r.get(0)?,
                        project: r.get(1)?,
                        first: r.get(2)?,
                        last: r.get(3)?,
                        events: r.get::<_, i64>(4)?.try_into().unwrap_or(0),
                        errors: r.get::<_, Option<i64>>(5)?.unwrap_or(0).try_into().unwrap_or(0),
                        last_id: r.get(6)?,
                    })
                })
                .map_err(backend)?;
            rows.collect::<Result<Vec<_>, _>>().map_err(backend)
        })
    }

    fn get(&self, id: &str) -> Result<Option<Envelope>, StoreError> {
        self.read(|conn| {
            let mut stmt = conn.prepare_cached("SELECT body FROM events WHERE id = ?1").map_err(backend)?;
            let mut rows = stmt.query([id]).map_err(backend)?;
            match rows.next().map_err(backend)? {
                Some(row) => decode(&row.get::<_, String>(0).map_err(backend)?).map(Some),
                None => Ok(None),
            }
        })
    }
}

fn backend(e: rusqlite::Error) -> StoreError {
    StoreError::Backend(e.to_string())
}

fn decode(body: &str) -> Result<Envelope, StoreError> {
    serde_json::from_str(body).map_err(|e| StoreError::Corrupt(e.to_string()))
}

fn escape_like(s: &str) -> String {
    s.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
}

/// Turns free text into an FTS5 query that cannot be a syntax error: every word is quoted,
/// all must match, the last one as a prefix so results follow typing.
fn fts_query(text: &str) -> Option<String> {
    let words: Vec<String> = text
        .split_whitespace()
        .map(|w| format!("\"{}\"", w.replace('"', "\"\"")))
        .collect();
    if words.is_empty() {
        return None;
    }
    Some(format!("{}*", words.join(" ")))
}

/// Kind, label, source file and every string, number and key in the payload, capped.
fn searchable_text(e: &Envelope) -> String {
    let mut out = String::new();
    out.push_str(&e.kind);
    for s in [&e.meta.label, &e.origin.file].into_iter().flatten() {
        out.push(' ');
        out.push_str(s);
    }
    collect_strings(&e.payload, &mut out);
    if out.len() > FTS_TEXT_MAX {
        let mut cut = FTS_TEXT_MAX;
        while !out.is_char_boundary(cut) {
            cut -= 1;
        }
        out.truncate(cut);
    }
    out
}

fn collect_strings(v: &Value, out: &mut String) {
    if out.len() > FTS_TEXT_MAX {
        return;
    }
    match v {
        Value::String(s) => {
            out.push(' ');
            out.push_str(s);
        }
        Value::Number(n) => {
            out.push(' ');
            out.push_str(&n.to_string());
        }
        Value::Array(items) => items.iter().for_each(|i| collect_strings(i, out)),
        Value::Object(map) => map.iter().for_each(|(k, v)| {
            out.push(' ');
            out.push_str(k);
            collect_strings(v, out);
        }),
        Value::Bool(_) | Value::Null => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glint_core::Normaliser;
    use glint_protocol::{BodyFormat, Level, parse_body};

    fn events(json: &str) -> Vec<Envelope> {
        let mut n = Normaliser::new();
        parse_body(json.as_bytes(), BodyFormat::Json)
            .unwrap()
            .into_iter()
            .map(|r| n.normalise(r.unwrap(), chrono::Utc::now()))
            .collect()
    }

    fn sample() -> SqliteStore {
        let s = SqliteStore::open_in_memory().unwrap();
        s.insert(&events(
            r#"[
              {"kind":"core.log","payload":{"message":"user signed in"},"project":"shop","run":"r1"},
              {"kind":"sql.query","payload":{"sql":"select * from orders"},"project":"shop","run":"r1","meta":{"level":"warn"}},
              {"kind":"core.exception","payload":{"class":"TypeError","message":"undefined is not a function"},"project":"blog","run":"r2"}
            ]"#,
        ))
        .unwrap();
        s
    }

    fn q(json: &str) -> Query {
        serde_json::from_str(json).unwrap()
    }

    fn kinds(v: &[Envelope]) -> Vec<&str> {
        v.iter().map(|e| e.kind.as_str()).collect()
    }

    #[test]
    fn round_trips_whole_envelopes_in_insertion_order() {
        let s = sample();
        let all = s.query(&Query::default()).unwrap();
        assert_eq!(kinds(&all), ["core.log", "sql.query", "core.exception"]);
        assert_eq!(s.get(&all[1].id).unwrap().as_ref(), Some(&all[1]));
        assert!(s.get("missing").unwrap().is_none());
    }

    #[test]
    fn filters_combine() {
        let s = sample();
        assert_eq!(kinds(&s.query(&q(r#"{"project":"shop"}"#)).unwrap()), ["core.log", "sql.query"]);
        assert_eq!(kinds(&s.query(&q(r#"{"kind":"sql.*"}"#)).unwrap()), ["sql.query"]);
        assert_eq!(kinds(&s.query(&q(r#"{"kind":"core.log"}"#)).unwrap()), ["core.log"]);
        assert_eq!(kinds(&s.query(&q(r#"{"level":"warn"}"#)).unwrap()), ["sql.query", "core.exception"]);
        assert_eq!(kinds(&s.query(&q(r#"{"run":"r2"}"#)).unwrap()), ["core.exception"]);
    }

    #[test]
    fn full_text_search_is_safe_and_prefix_matches() {
        let s = sample();
        assert_eq!(kinds(&s.query(&q(r#"{"q":"orde"}"#)).unwrap()), ["sql.query"]);
        assert_eq!(kinds(&s.query(&q(r#"{"q":"undefined func"}"#)).unwrap()), ["core.exception"]);
        // FTS5 syntax characters must not cause errors.
        assert!(s.query(&q(r#"{"q":"\"AND( * NEAR"}"#)).unwrap().is_empty());
    }

    #[test]
    fn limit_keeps_the_newest_and_after_pages_forward() {
        let s = sample();
        let all = s.query(&Query::default()).unwrap();
        assert_eq!(kinds(&s.query(&q(r#"{"limit":2}"#)).unwrap()), ["sql.query", "core.exception"]);
        let after = Query { after: Some(all[0].id.clone()), ..Default::default() };
        assert_eq!(kinds(&s.query(&after).unwrap()), ["sql.query", "core.exception"]);
    }

    #[test]
    fn runs_summarise_newest_first() {
        let s = sample();
        let runs = s.runs(None, 10).unwrap();
        assert_eq!(runs.iter().map(|r| r.run.as_str()).collect::<Vec<_>>(), ["r2", "r1"]);
        assert_eq!((runs[0].events, runs[0].errors), (1, 1));
        assert_eq!((runs[1].events, runs[1].errors, runs[1].project.as_str()), (2, 0, "shop"));
        assert_eq!(s.runs(Some("shop"), 10).unwrap().len(), 1);
        let all = s.query(&Query::default()).unwrap();
        assert_eq!(runs[0].last_id, all[2].id);
    }

    #[test]
    fn before_pages_backward() {
        let s = sample();
        let all = s.query(&Query::default()).unwrap();
        let q = Query { before: Some(all[2].id.clone()), ..Default::default() };
        assert_eq!(kinds(&s.query(&q).unwrap()), ["core.log", "sql.query"]);
    }

    #[test]
    fn duplicate_ids_are_ignored() {
        let s = SqliteStore::open_in_memory().unwrap();
        let e = events(r#"[{"id":"same","kind":"core.value","payload":1},{"id":"same","kind":"core.value","payload":2}]"#);
        s.insert(&e).unwrap();
        let all = s.query(&Query::default()).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].payload, 1);
        assert_eq!(all[0].level(), Level::Debug);
    }

    #[test]
    fn file_store_reads_through_second_connection() {
        let dir = std::env::temp_dir().join(format!("glint-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.db");
        let _ = std::fs::remove_file(&path);
        let s = SqliteStore::open(&path).unwrap();
        s.insert(&events(r#"{"kind":"core.value","payload":"hi"}"#)).unwrap();
        assert_eq!(s.query(&Query::default()).unwrap().len(), 1);
        drop(s);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
