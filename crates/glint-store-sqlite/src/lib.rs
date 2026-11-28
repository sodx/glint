//! SQLite implementation of [`glint_core::Store`]: one file, WAL mode, FTS5 over the string
//! values of each event.
//!
//! Each envelope is stored whole as JSON (`body`) plus the columns filters need. Rows are
//! ordered by `seq`, the insertion order, which is what "oldest first" means everywhere.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::{DateTime, SecondsFormat, Utc};
use glint_core::agents::{Activity, EventRead, Failure};
use glint_core::{ProjectSummary, Query, RunSummary, Store, StoreError};
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

-- What coding agents did with Glint (glint_core::agents). Pruned with the events.
CREATE TABLE IF NOT EXISTS agent_activity (
    seq          INTEGER PRIMARY KEY,
    ts           TEXT    NOT NULL,
    agent        TEXT    NOT NULL,
    session      TEXT,
    project      TEXT,
    action       TEXT    NOT NULL,
    tool         TEXT,
    duration_ms  INTEGER,
    result_bytes INTEGER,
    is_error     INTEGER NOT NULL DEFAULT 0,
    error_events INTEGER NOT NULL DEFAULT 0,
    detail       TEXT
);
CREATE INDEX IF NOT EXISTS agent_activity_ts ON agent_activity (ts);
-- The events each activity put in front of the agent.
CREATE TABLE IF NOT EXISTS agent_reads (
    activity INTEGER NOT NULL,
    event_id TEXT    NOT NULL
);
CREATE INDEX IF NOT EXISTS agent_reads_event    ON agent_reads (event_id);
CREATE INDEX IF NOT EXISTS agent_reads_activity ON agent_reads (activity);
"#;

/// A program failure: error level or worse, not bookkeeping, not an agent's own tool failure.
const FAILURE: &str = "e.level >= 4 AND e.kind NOT IN ('core.update', 'ctl.resume')
    AND COALESCE(json_extract(e.body, '$.origin.lang'), '') <> 'agent'";

/// Most text indexed per event. Each distinct key and string is indexed once, so a big value of
/// repeated records (a 1 MB export of 2,000 orders) fits with every distinct value in it.
const FTS_TEXT_MAX: usize = 256 * 1024;

pub struct SqliteStore {
    writer: Mutex<Connection>,
    /// A second connection so reads never wait for a write transaction. Absent in memory,
    /// where connections cannot share a database.
    reader: Option<Mutex<Connection>>,
}

impl SqliteStore {
    /// Opens (or creates) the database at `path`. A file that is not a SQLite database, or one
    /// SQLite reports as damaged, gives [`StoreError::Corrupt`].
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        Self::open_inner(path.as_ref()).map_err(|e| match e {
            rusqlite::Error::SqliteFailure(f, msg)
                if matches!(f.code, rusqlite::ErrorCode::NotADatabase | rusqlite::ErrorCode::DatabaseCorrupt) =>
            {
                StoreError::Corrupt(msg.unwrap_or_else(|| f.to_string()))
            }
            other => backend(other),
        })
    }

    /// [`open`](Self::open), and when the file is corrupt: move it (and its `-wal` / `-shm`
    /// files) aside as `<name>.corrupt-<unix seconds>` and start with an empty database. Returns
    /// where the damaged file went, so the caller can say so. A daemon that cannot open its
    /// store would otherwise fail at every start, and a login service would restart it forever.
    pub fn open_or_recover(path: impl AsRef<Path>) -> Result<(Self, Option<PathBuf>), StoreError> {
        let path = path.as_ref();
        match Self::open(path) {
            Err(StoreError::Corrupt(_)) => {
                let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "glint.db".into());
                let aside = path.with_file_name(format!("{name}.corrupt-{ts}"));
                std::fs::rename(path, &aside).map_err(|e| StoreError::Backend(format!("moving {} aside: {e}", path.display())))?;
                for ext in ["-wal", "-shm"] {
                    let side = path.with_file_name(format!("{name}{ext}"));
                    if side.exists() {
                        let _ = std::fs::rename(&side, aside.with_file_name(format!("{name}.corrupt-{ts}{ext}")));
                    }
                }
                Ok((Self::open(path)?, Some(aside)))
            }
            other => other.map(|s| (s, None)),
        }
    }

    fn open_inner(path: &Path) -> Result<Self, rusqlite::Error> {
        let writer = Connection::open(path)?;
        // Incremental auto-vacuum lets pruning give space back to the disk. It must be set before
        // the first table exists; a database created without it is converted once (a VACUUM).
        let vacuum_mode: i64 = writer.query_row("PRAGMA auto_vacuum", [], |r| r.get(0))?;
        if vacuum_mode != 2 {
            writer.execute_batch("PRAGMA auto_vacuum = INCREMENTAL; VACUUM;")?;
        }
        writer.execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL; PRAGMA busy_timeout = 5000;")?;
        writer.execute_batch(SCHEMA)?;
        let reader = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
        reader.execute_batch("PRAGMA busy_timeout = 5000;")?;
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

    /// Deletes the oldest events so that none is older than `keep_days` and at most `max_events`
    /// remain, with their search-index rows, in batches (writers are never held up for long).
    /// Then gives the space back to the disk. Returns how many events were removed.
    pub fn prune(&self, keep_days: Option<u32>, max_events: Option<u64>) -> Result<u64, StoreError> {
        let mut boundary: i64 = 0; // events with seq <= boundary go
        {
            let conn = self.writer.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(days) = keep_days.filter(|d| *d > 0) {
                let cutoff = (chrono::Utc::now() - chrono::Duration::days(i64::from(days))).to_rfc3339_opts(SecondsFormat::Micros, true);
                // ts is close to insertion order; the newest old-enough event bounds the cut.
                let b: Option<i64> = conn
                    .query_row("SELECT max(seq) FROM events WHERE ts < ?1", [&cutoff], |r| r.get(0))
                    .map_err(backend)?;
                boundary = boundary.max(b.unwrap_or(0));
            }
            if let Some(max) = max_events.filter(|m| *m > 0) {
                let b: Option<i64> = conn
                    .query_row("SELECT seq FROM events ORDER BY seq DESC LIMIT 1 OFFSET ?1", [i64::try_from(max).unwrap_or(i64::MAX)], |r| r.get(0))
                    .ok();
                boundary = boundary.max(b.unwrap_or(0));
            }
        }
        if boundary == 0 {
            return Ok(0);
        }
        self.delete_upto(boundary)
    }

    /// Bytes the database holds in use: its pages minus the free ones.
    pub fn used_bytes(&self) -> Result<u64, StoreError> {
        let conn = self.writer.lock().unwrap_or_else(|p| p.into_inner());
        let pages: i64 = conn.query_row("PRAGMA page_count", [], |r| r.get(0)).map_err(backend)?;
        let free: i64 = conn.query_row("PRAGMA freelist_count", [], |r| r.get(0)).map_err(backend)?;
        let size: i64 = conn.query_row("PRAGMA page_size", [], |r| r.get(0)).map_err(backend)?;
        Ok(u64::try_from((pages - free) * size).unwrap_or(0))
    }

    /// Deletes the oldest events until the database holds at most `max_bytes` (0: no cap). It
    /// goes down to 90% of the cap, so a busy program does not set it off after every write.
    /// Returns how many events were removed.
    pub fn prune_bytes(&self, max_bytes: u64) -> Result<u64, StoreError> {
        if max_bytes == 0 {
            return Ok(0);
        }
        let target = max_bytes - max_bytes / 10;
        let mut removed = 0;
        // Each round removes the share of events the excess suggests; the index and free pages
        // make that a guess, so a few rounds close in on the target.
        for _ in 0..8 {
            let used = self.used_bytes()?;
            if (used <= max_bytes && removed == 0) || used <= target {
                break;
            }
            let boundary: Option<i64> = {
                let conn = self.writer.lock().unwrap_or_else(|p| p.into_inner());
                let n: i64 = conn.query_row("SELECT count(*) FROM events", [], |r| r.get(0)).map_err(backend)?;
                if n == 0 {
                    break;
                }
                #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
                let k = ((n as f64) * (used - target) as f64 / used as f64).ceil() as i64;
                conn.query_row("SELECT seq FROM events ORDER BY seq LIMIT 1 OFFSET ?1", [k.clamp(1, n) - 1], |r| r.get(0)).ok()
            };
            let Some(b) = boundary else { break };
            removed += self.delete_upto(b)?;
        }
        Ok(removed)
    }

    /// Deletes every event.
    pub fn clear(&self) -> Result<u64, StoreError> {
        self.delete_upto(i64::MAX)
    }

    fn delete_upto(&self, boundary: i64) -> Result<u64, StoreError> {
        let mut removed = 0u64;
        loop {
            let conn = self.writer.lock().unwrap_or_else(|p| p.into_inner());
            let tx = conn.unchecked_transaction().map_err(backend)?;
            let ids: Vec<i64> = {
                let mut st = tx.prepare_cached("SELECT seq FROM events WHERE seq <= ?1 ORDER BY seq LIMIT 2000").map_err(backend)?;
                st.query_map([boundary], |r| r.get(0)).map_err(backend)?.collect::<Result<_, _>>().map_err(backend)?
            };
            if ids.is_empty() {
                break;
            }
            {
                let mut fts = tx.prepare_cached("DELETE FROM events_fts WHERE rowid = ?1").map_err(backend)?;
                let mut ev = tx.prepare_cached("DELETE FROM events WHERE seq = ?1").map_err(backend)?;
                for id in &ids {
                    fts.execute([id]).map_err(backend)?;
                    ev.execute([id]).map_err(backend)?;
                }
            }
            tx.commit().map_err(backend)?;
            removed += ids.len() as u64;
        }
        let conn = self.writer.lock().unwrap_or_else(|p| p.into_inner());
        // Agent activity never outlives the events it is about.
        if removed > 0 {
            conn.execute_batch(
                "DELETE FROM agent_reads WHERE activity IN (SELECT seq FROM agent_activity
                     WHERE ts < COALESCE((SELECT min(ts) FROM events), '9999'));
                 DELETE FROM agent_activity WHERE ts < COALESCE((SELECT min(ts) FROM events), '9999');",
            )
            .map_err(backend)?;
        }
        // incremental_vacuum frees one page per step: it must be stepped to the end.
        {
            let mut st = conn.prepare("PRAGMA incremental_vacuum").map_err(backend)?;
            let mut rows = st.query([]).map_err(backend)?;
            while rows.next().map_err(backend)?.is_some() {}
        }
        let mut st = conn.prepare("PRAGMA wal_checkpoint(TRUNCATE)").map_err(backend)?;
        let mut rows = st.query([]).map_err(backend)?;
        while rows.next().map_err(backend)?.is_some() {}
        Ok(removed)
    }

    /// How many events are stored, and how big the database file is.
    pub fn stats(&self) -> Result<(u64, u64), StoreError> {
        self.read(|c| {
            let n: i64 = c.query_row("SELECT count(*) FROM events", [], |r| r.get(0)).map_err(backend)?;
            let pages: i64 = c.query_row("PRAGMA page_count", [], |r| r.get(0)).map_err(backend)?;
            let size: i64 = c.query_row("PRAGMA page_size", [], |r| r.get(0)).map_err(backend)?;
            Ok((u64::try_from(n).unwrap_or(0), u64::try_from(pages * size).unwrap_or(0)))
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
        // Bookkeeping events (updates, resumes) don't make a run "active". Newest first by the
        // time of the run's last event (ts is fixed-width RFC 3339, so text order is time order),
        // not by when glintd received it: a late batch of an old run stays where it happened.
        // Runs that end at the same instant keep ingest order.
        let sql = "SELECT g.run, g.project, g.first, g.last, g.n, g.errors, e.id,
                          json_extract(f.body, '$.origin.file'),
                          CASE WHEN f.kind = 'core.log' THEN json_extract(f.body, '$.payload.message') END
                   FROM (SELECT run, MAX(project) AS project, MIN(ts) AS first, MAX(ts) AS last,
                                COUNT(*) AS n, SUM(level >= 4) AS errors, MAX(seq) AS last_seq,
                                MIN(seq) AS first_seq
                         FROM events
                         WHERE kind NOT IN ('core.update', 'ctl.resume') AND (?1 IS NULL OR project = ?1)
                         GROUP BY run
                         ORDER BY last DESC, last_seq DESC
                         LIMIT ?2) g
                   JOIN events e ON e.seq = g.last_seq
                   JOIN events f ON f.seq = g.first_seq
                   ORDER BY g.last DESC, g.last_seq DESC";
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
                        file: r.get(7)?,
                        head: r.get::<_, Option<String>>(8)?.map(|h| h.chars().take(120).collect()),
                    })
                })
                .map_err(backend)?;
            rows.collect::<Result<Vec<_>, _>>().map_err(backend)
        })
    }

    fn projects(&self) -> Result<Vec<ProjectSummary>, StoreError> {
        // A run belongs to one project, as in `runs`. Grouping by run walks the run index; grouping by
        // (project, run) sorted every row and took ten times as long on 100,000 events.
        let sql = "SELECT project, COUNT(*), SUM(errors), MAX(last)
                   FROM (SELECT MAX(project) AS project, MAX(ts) AS last, MAX(level >= 4) AS errors
                         FROM events
                         WHERE kind NOT IN ('core.update', 'ctl.resume')
                         GROUP BY run)
                   GROUP BY project
                   ORDER BY MAX(last) DESC";
        self.read(|conn| {
            let mut stmt = conn.prepare_cached(sql).map_err(backend)?;
            let rows = stmt
                .query_map([], |r| {
                    Ok(ProjectSummary {
                        project: r.get(0)?,
                        runs: r.get::<_, i64>(1)?.try_into().unwrap_or(0),
                        failing: r.get::<_, Option<i64>>(2)?.unwrap_or(0).try_into().unwrap_or(0),
                        last: r.get(3)?,
                    })
                })
                .map_err(backend)?;
            rows.collect::<Result<Vec<_>, _>>().map_err(backend)
        })
    }

    fn record_activity(&self, activity: &[Activity]) -> Result<(), StoreError> {
        let mut conn = self.writer.lock().unwrap_or_else(|p| p.into_inner());
        let tx = conn.transaction().map_err(backend)?;
        {
            let mut ins = tx
                .prepare_cached(
                    "INSERT INTO agent_activity (ts, agent, session, project, action, tool, duration_ms, result_bytes, is_error, detail)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                )
                .map_err(backend)?;
            let mut read = tx.prepare_cached("INSERT INTO agent_reads (activity, event_id) VALUES (?1, ?2)").map_err(backend)?;
            let mut count = tx
                .prepare_cached(&format!(
                    "UPDATE agent_activity SET error_events = (SELECT count(*) FROM agent_reads r JOIN events e ON e.id = r.event_id
                     WHERE r.activity = ?1 AND {FAILURE}) WHERE seq = ?1"
                ))
                .map_err(backend)?;
            for a in activity {
                ins.execute(rusqlite::params![
                    a.ts.to_rfc3339_opts(SecondsFormat::Micros, true),
                    a.agent,
                    a.session,
                    a.project,
                    a.action,
                    a.tool,
                    a.duration_ms.and_then(|v| i64::try_from(v).ok()),
                    a.result_bytes.and_then(|v| i64::try_from(v).ok()),
                    a.is_error,
                    (!a.detail.is_null()).then(|| a.detail.to_string()),
                ])
                .map_err(backend)?;
                let seq = tx.last_insert_rowid();
                for id in &a.events {
                    read.execute(rusqlite::params![seq, id]).map_err(backend)?;
                }
                if !a.events.is_empty() {
                    count.execute([seq]).map_err(backend)?;
                }
            }
        }
        tx.commit().map_err(backend)
    }

    fn activity_since(&self, since: DateTime<Utc>) -> Result<Vec<Activity>, StoreError> {
        let since = since.to_rfc3339_opts(SecondsFormat::Micros, true);
        self.read(|conn| {
            let mut st = conn
                .prepare_cached(
                    "SELECT a.seq, a.ts, a.agent, a.session,
                            COALESCE(a.project, (SELECT e.project FROM agent_reads r JOIN events e ON e.id = r.event_id
                                                 WHERE r.activity = a.seq ORDER BY r.rowid LIMIT 1)),
                            a.action, a.tool, a.duration_ms, a.result_bytes, a.is_error, a.error_events, a.detail
                     FROM agent_activity a WHERE a.ts >= ?1 ORDER BY a.seq",
                )
                .map_err(backend)?;
            let rows = st
                .query_map([&since], |r| {
                    let ts: String = r.get(1)?;
                    let detail: Option<String> = r.get(11)?;
                    Ok((
                        r.get::<_, i64>(0)?,
                        Activity {
                            ts: DateTime::parse_from_rfc3339(&ts).map(|t| t.with_timezone(&Utc)).unwrap_or_default(),
                            agent: r.get(2)?,
                            session: r.get(3)?,
                            project: r.get(4)?,
                            action: r.get(5)?,
                            tool: r.get(6)?,
                            duration_ms: r.get::<_, Option<i64>>(7)?.and_then(|v| u64::try_from(v).ok()),
                            result_bytes: r.get::<_, Option<i64>>(8)?.and_then(|v| u64::try_from(v).ok()),
                            is_error: r.get(9)?,
                            events: Vec::new(),
                            error_events: r.get::<_, i64>(10)?.try_into().unwrap_or(0),
                            detail: detail.and_then(|d| serde_json::from_str(&d).ok()).unwrap_or(Value::Null),
                        },
                    ))
                })
                .map_err(backend)?;
            let mut out: Vec<(i64, Activity)> = rows.collect::<Result<_, _>>().map_err(backend)?;
            let Some(first) = out.first().map(|(s, _)| *s) else { return Ok(Vec::new()) };
            let mut st = conn
                .prepare_cached("SELECT activity, event_id FROM agent_reads WHERE activity >= ?1 ORDER BY rowid")
                .map_err(backend)?;
            let reads = st.query_map([first], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))).map_err(backend)?;
            let index: std::collections::HashMap<i64, usize> = out.iter().enumerate().map(|(i, (s, _))| (*s, i)).collect();
            for row in reads {
                let (seq, id) = row.map_err(backend)?;
                if let Some(i) = index.get(&seq) {
                    out[*i].1.events.push(id);
                }
            }
            Ok(out.into_iter().map(|(_, a)| a).collect())
        })
    }

    fn failures_since(&self, since: DateTime<Utc>) -> Result<Vec<Failure>, StoreError> {
        let since = since.to_rfc3339_opts(SecondsFormat::Micros, true);
        self.read(|conn| {
            let mut st = conn
                .prepare_cached(&format!("SELECT e.id, e.ts, e.project, e.run FROM events e WHERE e.ts >= ?1 AND {FAILURE} ORDER BY e.seq"))
                .map_err(backend)?;
            let rows = st
                .query_map([&since], |r| {
                    let ts: String = r.get(1)?;
                    Ok(Failure {
                        id: r.get(0)?,
                        ts: DateTime::parse_from_rfc3339(&ts).map(|t| t.with_timezone(&Utc)).unwrap_or_default(),
                        project: r.get(2)?,
                        run: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                    })
                })
                .map_err(backend)?;
            rows.collect::<Result<_, _>>().map_err(backend)
        })
    }

    fn reads_in_run(&self, run: &str) -> Result<Vec<EventRead>, StoreError> {
        self.read(|conn| {
            let mut st = conn
                .prepare_cached(
                    "SELECT r.event_id, a.agent, min(a.ts),
                            CASE WHEN a.action = 'mcp.call' THEN 'mcp' ELSE 'hook' END AS via
                     FROM events e JOIN agent_reads r ON r.event_id = e.id JOIN agent_activity a ON a.seq = r.activity
                     WHERE e.run = ?1 AND a.action IN ('mcp.call', 'hook.context', 'hook.stop_guard')
                     GROUP BY r.event_id, a.agent, via",
                )
                .map_err(backend)?;
            let rows = st
                .query_map([run], |r| {
                    let ts: String = r.get(2)?;
                    Ok(EventRead {
                        event: r.get(0)?,
                        agent: r.get(1)?,
                        ts: DateTime::parse_from_rfc3339(&ts).map(|t| t.with_timezone(&Utc)).unwrap_or_default(),
                        via: r.get(3)?,
                    })
                })
                .map_err(backend)?;
            rows.collect::<Result<_, _>>().map_err(backend)
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
    collect_strings(&e.payload, &mut out, &mut std::collections::HashSet::new());
    if out.len() > FTS_TEXT_MAX {
        let mut cut = FTS_TEXT_MAX;
        while !out.is_char_boundary(cut) {
            cut -= 1;
        }
        out.truncate(cut);
    }
    out
}

fn collect_strings<'a>(v: &'a Value, out: &mut String, seen: &mut std::collections::HashSet<&'a str>) {
    if out.len() > FTS_TEXT_MAX {
        return;
    }
    fn push<'a>(s: &'a str, out: &mut String, seen: &mut std::collections::HashSet<&'a str>) {
        if seen.insert(s) {
            out.push(' ');
            out.push_str(s);
        }
    }
    match v {
        Value::String(s) => push(s, out, seen),
        Value::Number(n) => {
            out.push(' ');
            out.push_str(&n.to_string());
        }
        Value::Array(items) => items.iter().for_each(|i| collect_strings(i, out, seen)),
        Value::Object(map) => map.iter().for_each(|(k, v)| {
            push(k, out, seen);
            collect_strings(v, out, seen);
        }),
        Value::Bool(_) | Value::Null => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_corrupt_file_is_moved_aside_and_a_fresh_store_opens() {
        let dir = std::env::temp_dir().join(format!("glint-corrupt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("glint.db");
        std::fs::write(&db, b"this is not a sqlite database, just some bytes that are long enough to look like a header").unwrap();
        assert!(matches!(SqliteStore::open(&db), Err(StoreError::Corrupt(_))));
        let (store, aside) = SqliteStore::open_or_recover(&db).unwrap();
        let aside = aside.expect("moved aside");
        assert!(aside.file_name().unwrap().to_string_lossy().starts_with("glint.db.corrupt-"));
        assert!(std::fs::read(&aside).unwrap().starts_with(b"this is not"));
        assert_eq!(store.stats().unwrap().0, 0);
        drop(store);
        // A healthy database is opened as it is.
        let (_, again) = SqliteStore::open_or_recover(&db).unwrap();
        assert!(again.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
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
    fn search_finds_a_value_deep_in_a_big_payload() {
        // About 1 MB of repeated records: keys repeat, values differ. The last record's SKU sits
        // far past the first 64 KB of the payload's text.
        let orders: Vec<_> = (0..2000)
            .map(|i| serde_json::json!({ "id": i, "sku": format!("SKU-{i:05}"), "customer": { "name": format!("Customer {i}"), "note": "x".repeat(400) } }))
            .collect();
        let batch = serde_json::json!([{ "kind": "core.value", "payload": { "value": { "orders": orders } } }]).to_string();
        let s = SqliteStore::open_in_memory().unwrap();
        s.insert(&events(&batch)).unwrap();
        assert_eq!(s.query(&q(r#"{"q":"SKU-01999"}"#)).unwrap().len(), 1);
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
    fn runs_order_by_last_event_time_not_arrival() {
        let s = SqliteStore::open_in_memory().unwrap();
        // "late" arrives last but happened first; "a" and "b" end at the same instant.
        s.insert(&events(
            r#"[
              {"kind":"core.log","payload":{"message":"x"},"run":"a","ts":"2026-01-02T10:00:00Z"},
              {"kind":"core.log","payload":{"message":"x"},"run":"b","ts":"2026-01-02T10:00:00Z"},
              {"kind":"core.log","payload":{"message":"x"},"run":"new","ts":"2026-01-02T11:00:00.5Z"},
              {"kind":"core.log","payload":{"message":"x"},"run":"late","ts":"2026-01-01T09:00:00Z"}
            ]"#,
        ))
        .unwrap();
        let order = |n| s.runs(None, n).unwrap().into_iter().map(|r| r.run).collect::<Vec<_>>();
        assert_eq!(order(10), ["new", "b", "a", "late"]);
        // The limit keeps the newest by time, not the last to arrive.
        assert_eq!(order(1), ["new"]);
    }

    #[test]
    fn projects_list_every_project_newest_first() {
        let s = sample();
        let p = s.projects().unwrap();
        assert_eq!(p.iter().map(|x| (x.project.as_str(), x.runs, x.failing)).collect::<Vec<_>>(), [("blog", 1, 1), ("shop", 1, 0)]);
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

    #[test]
    fn agent_activity_round_trips_counts_failures_and_goes_with_the_events() {
        let s = sample();
        let all = s.query(&Query::default()).unwrap();
        let (log, exc) = (all[0].id.clone(), all[2].id.clone());
        let mut a = Activity::new("claude-code", glint_core::agents::MCP_CALL);
        a.tool = Some("glint_events".into());
        a.events = vec![log.clone(), exc.clone()];
        a.duration_ms = Some(12);
        let mut b = Activity::new("claude-code", glint_core::agents::HOOK_CONTEXT);
        b.events = vec![exc.clone()];
        b.detail = serde_json::json!({ "failures": 1 });
        s.record_activity(&[a, b]).unwrap();

        let since = chrono::Utc::now() - chrono::Duration::hours(1);
        let got = s.activity_since(since).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].events, [log.clone(), exc.clone()]);
        assert_eq!(got[0].error_events, 1);
        assert_eq!(got[0].project.as_deref(), Some("shop"), "from the first event read");
        assert_eq!(got[1].detail["failures"], 1);
        let fails = s.failures_since(since).unwrap();
        assert_eq!(fails.iter().map(|f| f.id.as_str()).collect::<Vec<_>>(), [exc.as_str()]);
        let reads = s.reads_in_run("r2").unwrap();
        assert_eq!(reads.len(), 2, "once over MCP, once by a hook");
        assert!(reads.iter().all(|r| r.event == exc));

        s.clear().unwrap();
        assert!(s.activity_since(since).unwrap().is_empty());
    }

    #[test]
    fn prune_bytes_keeps_the_database_under_its_cap_and_the_newest_events() {
        let s = SqliteStore::open_in_memory().unwrap();
        let text = "lorem ipsum dolor sit amet ".repeat(400); // ~10 KB, indexed too
        for i in 0..300 {
            s.insert(&events(&format!(r#"{{"id":"e{i:03}","kind":"core.text","payload":{{"text":"{text}"}}}}"#))).unwrap();
        }
        let before = s.used_bytes().unwrap();
        assert!(before > 4_000_000, "{before}");
        assert_eq!(s.prune_bytes(0).unwrap(), 0, "0 means no cap");
        assert_eq!(s.prune_bytes(before * 2).unwrap(), 0, "under the cap");
        let cap = 2_000_000;
        let removed = s.prune_bytes(cap).unwrap();
        assert!(removed > 100 && removed < 300, "{removed}");
        assert!(s.used_bytes().unwrap() <= cap, "{}", s.used_bytes().unwrap());
        assert!(s.get("e299").unwrap().is_some(), "the newest stay");
        assert!(s.get("e000").unwrap().is_none(), "the oldest go");
        let fts: i64 = s.writer.lock().unwrap().query_row("SELECT count(*) FROM events_fts", [], |r| r.get(0)).unwrap();
        assert_eq!(u64::try_from(fts).unwrap(), 300 - removed);
    }

    #[test]
    fn prune_keeps_the_newest_and_the_search_index_in_step() {
        let s = SqliteStore::open_in_memory().unwrap();
        let mut n = glint_core::Normaliser::new();
        let old = chrono::Utc::now() - chrono::Duration::days(30);
        let mk = |i: usize, ts: chrono::DateTime<chrono::Utc>, n: &mut glint_core::Normaliser| {
            let inc = glint_protocol::parse_body(format!(r#"{{"kind":"core.log","payload":{{"message":"needle {i}"}}}}"#).as_bytes(), glint_protocol::BodyFormat::Json).unwrap().remove(0).unwrap();
            let mut e = n.normalise(inc, ts);
            e.ts = ts;
            e
        };
        let events: Vec<_> = (0..10).map(|i| mk(i, if i < 4 { old } else { chrono::Utc::now() }, &mut n)).collect();
        s.insert(&events).unwrap();
        assert_eq!(s.prune(Some(7), None).unwrap(), 4, "older than a week");
        assert_eq!(s.prune(None, Some(5)).unwrap(), 1, "beyond the cap");
        assert_eq!(s.stats().unwrap().0, 5);
        let hits = s.query(&Query { q: Some("needle".into()), ..Query::default() }).unwrap();
        assert_eq!(hits.len(), 5, "search sees only what is left");
        assert_eq!(s.prune(Some(7), Some(5)).unwrap(), 0);
    }
}
