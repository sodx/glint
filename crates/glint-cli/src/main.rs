//! `glint` — send things to a running Glint daemon from the shell.
//!
//! ```text
//! glint send 42 '{"a":1}' hello          # three core.value events
//! glint send --kind core.log "deployed"   # a log line
//! echo '{"sql":"select 1"}' | glint send --kind sql.query
//! npm run dev 2>&1 | glint pipe --label web
//! glint conformance -- node sdks/js/conformance.mjs
//! ```

mod client;
mod conformance;
mod lines;
mod pipe;
mod send;

use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};
use glint_protocol::Level;
use serde_json::json;

use client::Client;
use lines::Base;

#[derive(Parser)]
#[command(name = "glint", version, about = "Send values, logs and program output to Glint")]
struct Cli {
    /// Where glintd listens.
    #[arg(long, global = true, env = "GLINT_URL", default_value = "http://127.0.0.1:23600")]
    url: String,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Send one event per VALUE (or stdin when there is none, or for `-`).
    ///
    /// Values that parse as JSON are sent as JSON; anything else as a string.
    Send {
        /// Values to send.
        values: Vec<String>,
        /// Event kind. core.value, core.log, core.text, core.json, core.html and core.markdown
        /// wrap the value; any other kind takes the JSON value as its payload.
        #[arg(short, long, default_value = "core.value")]
        kind: String,
        /// Level: trace, debug, info, warn, error, fatal.
        #[arg(long, value_parser = parse_level)]
        level: Option<Level>,
        /// Send values as strings, even if they look like JSON.
        #[arg(long)]
        raw: bool,
        /// Input is complete Glint envelopes (JSON object, array or NDJSON).
        #[arg(long, conflicts_with_all = ["kind", "raw", "level"])]
        envelope: bool,
        /// Don't print the ids of stored events.
        #[arg(short, long)]
        quiet: bool,
        #[command(flatten)]
        common: Common,
    },
    /// Forward stdin to Glint line by line, echoing it to stdout.
    ///
    /// Levels are detected from plain text and from JSON log lines (pino, zap, structlog, …);
    /// stack traces are kept together; lines that are Glint envelopes pass through.
    Pipe {
        /// Don't echo input to stdout.
        #[arg(short, long)]
        quiet: bool,
        #[command(flatten)]
        common: Common,
    },
    /// Check a language SDK against the conformance suite (spec/conformance/).
    ///
    /// Runs the SDK's conformance program three times against an in-memory server: normally,
    /// with Glint unreachable, and with GLINT_ENABLED=0.
    Conformance {
        /// Use this scenarios.json instead of the built-in one.
        #[arg(long)]
        scenarios: Option<String>,
        /// The command that runs the SDK's conformance program.
        #[arg(last = true, required = true)]
        command: Vec<String>,
    },
}

#[derive(Args)]
struct Common {
    /// Label shown next to each event (e.g. the service name).
    #[arg(short, long)]
    label: Option<String>,
    /// gray, green, orange, red, blue or purple.
    #[arg(short, long, value_parser = ["gray", "green", "orange", "red", "blue", "purple"])]
    color: Option<String>,
    /// Project name; defaults to the current directory's name.
    #[arg(short, long)]
    project: Option<String>,
    /// Tag, repeatable.
    #[arg(short, long = "tag")]
    tags: Vec<String>,
}

fn parse_level(s: &str) -> Result<Level, String> {
    Level::parse(&s.to_ascii_lowercase()).ok_or_else(|| "expected trace, debug, info, warn, error or fatal".into())
}

fn base(common: Common, via: &str) -> Base {
    let cwd = std::env::current_dir().ok().map(|p| p.display().to_string());
    let mut origin = json!({
        "sdk": concat!("glint-cli/", env!("CARGO_PKG_VERSION")),
        "pid": std::process::id(),
        "via": via,
    });
    if let Some(cwd) = cwd {
        origin["cwd"] = json!(cwd);
    }
    if let Ok(host) = std::env::var("HOSTNAME") {
        origin["host"] = json!(host);
    }
    Base {
        run: format!("{via}-{}", ulid::Ulid::new()),
        project: common.project,
        label: common.label,
        color: common.color,
        tags: common.tags,
        origin,
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let client = Client::new(&cli.url);
    let result = match cli.command {
        Command::Send { values, kind, level, raw, envelope, quiet, common } => {
            let args = send::SendArgs { values, kind, level, raw, envelope, quiet };
            send::run(&args, &base(common, "send"), &client)
        }
        Command::Pipe { quiet, common } => pipe::run(&pipe::PipeArgs { quiet }, &base(common, "pipe"), &client),
        Command::Conformance { scenarios, command } => conformance::run(&conformance::Args { command, scenarios }),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("glint: {e}");
            ExitCode::FAILURE
        }
    }
}
