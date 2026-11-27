//! `glint pipe`: forward program output to Glint, line by line, while still printing it.
//!
//! Never breaks the pipeline: if Glint is down, output keeps flowing to stdout and the lines
//! are dropped with one warning on stderr.

use std::io::{BufRead, Write};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::client::{Client, ClientError};
use crate::lines::{Base, classify, is_continuation, log_envelope};

/// A record that sees no continuation line for this long is complete.
const IDLE: Duration = Duration::from_millis(50);
/// Most envelopes per request.
const BATCH: usize = 200;
/// Longest folded record; beyond it a new record starts.
const MAX_RECORD: usize = 64 * 1024;

pub struct PipeArgs {
    pub quiet: bool,
}

/// Joins stack-trace and indented lines onto the line they belong to.
#[derive(Default)]
pub struct Folder {
    current: Option<(String, Instant)>,
}

impl Folder {
    /// Feeds one line; returns the record it completed, if any.
    pub fn push(&mut self, line: String, now: Instant) -> Option<String> {
        if let Some((cur, at)) = &mut self.current
            && is_continuation(&line)
            && cur.len() + line.len() < MAX_RECORD
        {
            cur.push('\n');
            cur.push_str(&line);
            *at = now;
            return None;
        }
        self.current.replace((line, now)).map(|(s, _)| s)
    }

    /// Completes the pending record if nothing was added to it for [`IDLE`].
    pub fn tick(&mut self, now: Instant) -> Option<String> {
        match &self.current {
            Some((_, at)) if now.duration_since(*at) >= IDLE => self.current.take().map(|(s, _)| s),
            _ => None,
        }
    }

    pub fn finish(&mut self) -> Option<String> {
        self.current.take().map(|(s, _)| s)
    }
}

pub fn run(args: &PipeArgs, base: &Base, client: &Client) -> Result<(), String> {
    let (tx, rx) = mpsc::sync_channel::<String>(10_000);
    let quiet = args.quiet;

    // Reading and echoing happen on their own thread so output is never held back by
    // batching or by a slow daemon.
    let reader = std::thread::spawn(move || {
        let stdin = std::io::stdin();
        let mut input = stdin.lock();
        let stdout = std::io::stdout();
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match input.read_until(b'\n', &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            if !quiet {
                let mut out = stdout.lock();
                // A closed stdout (`| head`) must not stop forwarding.
                let _ = out.write_all(&buf).and_then(|()| out.flush());
            }
            let line = String::from_utf8_lossy(&buf);
            let line = line.strip_suffix('\n').unwrap_or(&line);
            let line = line.strip_suffix('\r').unwrap_or(line);
            if tx.send(line.to_owned()).is_err() {
                break;
            }
        }
    });

    let mut sink = Sink::new(client, base);
    let mut folder = Folder::default();
    loop {
        match rx.recv_timeout(IDLE) {
            Ok(line) => {
                if let Some(rec) = folder.push(line, Instant::now()) {
                    sink.add(rec);
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                if let Some(rec) = folder.tick(Instant::now()) {
                    sink.add(rec);
                }
                sink.flush();
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    if let Some(rec) = folder.finish() {
        sink.add(rec);
    }
    sink.flush();
    let _ = reader.join();
    sink.report();
    Ok(())
}

struct Sink<'a> {
    client: &'a Client,
    base: &'a Base,
    batch: Vec<Value>,
    down: bool,
    dropped: u64,
}

impl<'a> Sink<'a> {
    fn new(client: &'a Client, base: &'a Base) -> Self {
        Self { client, base, batch: Vec::new(), down: false, dropped: 0 }
    }

    fn add(&mut self, record: String) {
        if record.trim().is_empty() {
            return;
        }
        self.batch.push(log_envelope(classify(&record), self.base));
        if self.batch.len() >= BATCH {
            self.flush();
        }
    }

    fn flush(&mut self) {
        if self.batch.is_empty() {
            return;
        }
        let batch = std::mem::take(&mut self.batch);
        match self.client.post(&batch) {
            Ok(ack) => {
                if self.down {
                    eprintln!("glint: reconnected to {}", self.client.base());
                    self.down = false;
                }
                for (i, err) in ack.rejected {
                    eprintln!("glint: line rejected ({i}): {err}");
                }
            }
            Err(e) => {
                if !self.down {
                    let hint = matches!(e, ClientError::Unreachable(_)).then_some(" Is glintd running?").unwrap_or("");
                    eprintln!("glint: cannot send to {} ({e}).{hint} Output continues; lines are dropped until it is back.", self.client.base());
                    self.down = true;
                }
                self.dropped += batch.len() as u64;
            }
        }
    }

    fn report(&self) {
        if self.dropped > 0 {
            eprintln!("glint: {} lines were not sent", self.dropped);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folds_stack_traces_and_splits_on_new_records() {
        let t0 = Instant::now();
        let mut f = Folder::default();
        assert_eq!(f.push("Error: boom".into(), t0), None);
        assert_eq!(f.push("    at a (x.js:1:1)".into(), t0), None);
        assert_eq!(f.push("    at b (x.js:2:1)".into(), t0), None);
        assert_eq!(f.push("next line".into(), t0), Some("Error: boom\n    at a (x.js:1:1)\n    at b (x.js:2:1)".into()));
        assert_eq!(f.tick(t0), None);
        assert_eq!(f.tick(t0 + IDLE), Some("next line".into()));
        assert_eq!(f.finish(), None);
    }

    #[test]
    fn a_leading_indented_line_starts_its_own_record() {
        let mut f = Folder::default();
        assert_eq!(f.push("  indented first".into(), Instant::now()), None);
        assert_eq!(f.finish(), Some("  indented first".into()));
    }

    #[test]
    fn records_have_a_size_cap() {
        let t0 = Instant::now();
        let mut f = Folder::default();
        f.push("head".into(), t0);
        let long = format!("  {}", "x".repeat(MAX_RECORD));
        assert_eq!(f.push(long.clone(), t0), Some("head".into()));
        assert_eq!(f.finish(), Some(long));
    }
}
