//! `glint` — send things to a running Glint daemon from the shell.
//!
//! ```text
//! glint send 42 '{"a":1}' hello          # three core.value events
//! glint send --kind core.log "deployed"   # a log line
//! echo '{"sql":"select 1"}' | glint send --kind sql.query
//! npm run dev 2>&1 | glint pipe --label web
//! glint conformance -- node sdks/js/conformance.mjs
//! glint install                          # glintd at login + glint() in PHP, Python, Node
//! ```

mod agents;
mod client;
mod conformance;
mod expect;
mod hook;
mod install;
mod lines;
mod mcp;
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
    /// Forward a coding-agent hook (JSON on stdin) to Glint; always exits 0. On SessionStart and
    /// UserPromptSubmit it replies with Glint's news for the agent (recent or new failures).
    ///
    /// `glint hook --print-config` prints the Claude Code settings that install it.
    Hook {
        /// Which agent sent the hook.
        #[arg(long, default_value = "claude-code", value_parser = ["claude-code"])]
        agent: String,
        /// Print the hook settings to paste into .claude/settings.json, then exit.
        #[arg(long)]
        print_config: bool,
    },
    /// Serve MCP on stdio for coding agents, backed by the running glintd.
    ///
    /// Claude Code: `claude mcp add glint -- glint mcp`, or connect over HTTP directly:
    /// `claude mcp add --transport http glint http://127.0.0.1:23600/mcp`.
    Mcp,
    /// Set Glint up on this machine: glintd starts at login, glint() works in every PHP, Python
    /// and Node program with no setup (SDKs stay off when NODE_ENV/APP_ENV=production), and the
    /// coding agents found here (Claude Code, Cursor, VS Code, Windsurf, Gemini CLI, Codex) get
    /// Glint's MCP server; Claude Code also gets the hooks that show its session in Glint.
    Install {
        #[command(flatten)]
        setup: Setup,
    },
    /// Undo `glint install`. Keeps ~/.glint (events and SDK copies).
    Uninstall {
        #[command(flatten)]
        setup: Setup,
    },
    /// Check that glintd is reachable and glint() is available in each language.
    Doctor,
    /// A test from what programs did: wait for events matching each expectation, fail on errors.
    ///
    /// `glint expect 'label:cart payload.value.total>0' 'label:"order saved"' -- ./checkout.sh`.
    /// Terms (all must hold for one event): label:X, kind:X or kind:sql.*, text:X, has:PATH,
    /// PATH=VALUE (also != > >= < <=, e.g. payload.value.paid=true), count>=N.
    /// Exit 0 PASS, 1 FAIL, 2 could not check.
    Expect {
        /// The expectations, in the order they must be met (see --unordered).
        #[arg(required = true)]
        expect: Vec<String>,
        /// Project to look at; default: this directory's (the git repository's name).
        #[arg(short, long)]
        project: Option<String>,
        /// Look at every project.
        #[arg(long, conflicts_with = "project")]
        all_projects: bool,
        /// The expectations may be met in any order.
        #[arg(long)]
        unordered: bool,
        /// Don't fail when a program error arrives.
        #[arg(long)]
        allow_errors: bool,
        /// Seconds to wait for events still to come.
        #[arg(long, default_value_t = 30)]
        timeout: u64,
        /// Without a command: events of the last N seconds count too.
        #[arg(long, default_value_t = 30)]
        lookback: u64,
        /// A command to run first; only what happens from then on counts.
        #[arg(last = true)]
        command: Vec<String>,
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

#[derive(Args)]
struct Setup {
    /// Only these parts: service, php, python, node, agents (comma-separated).
    #[arg(long, value_delimiter = ',', value_parser = install::Target::parse)]
    only: Vec<install::Target>,
    /// Show what would be done without changing anything.
    #[arg(long)]
    dry_run: bool,
    /// PHP's ini scan directory (default: from `php --ini`).
    #[arg(long)]
    php_ini_dir: Option<std::path::PathBuf>,
    /// Shell profile for PATH and NODE_OPTIONS (default: from $SHELL).
    #[arg(long)]
    shell_profile: Option<std::path::PathBuf>,
}

impl Setup {
    fn args(self, url: &str) -> Result<install::Args, String> {
        let home = std::env::var_os("HOME").map(std::path::PathBuf::from).ok_or("HOME is not set")?;
        let targets = if self.only.is_empty() { install::Target::ALL.to_vec() } else { self.only };
        Ok(install::Args { targets, dry_run: self.dry_run, home, php_ini_dir: self.php_ini_dir, shell_profile: self.shell_profile, url: url.to_owned() })
    }
}

fn setup(steps: &[install::Step], dry_run: bool, then: &str) -> Result<(), String> {
    if dry_run {
        println!("(dry run: nothing was changed)");
    }
    if install::print(steps) {
        if !then.is_empty() {
            println!("\n{then}");
        }
        Ok(())
    } else {
        Err("some steps failed (see above)".into())
    }
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
        Command::Mcp => mcp::run(&cli.url),
        Command::Hook { agent, print_config } => {
            if print_config {
                println!("{}", hook::claude_code_config("glint"));
                Ok(())
            } else {
                hook::run(&cli.url, &agent)
            }
        }
        Command::Install { setup: s } => {
            let dry = s.dry_run;
            s.args(&cli.url).and_then(|a| setup(&install::install(&a), dry, "Done. Open a new shell, then try glint(\"hello\") in any PHP, Python or Node program."))
        }
        Command::Uninstall { setup: s } => {
            let dry = s.dry_run;
            s.args(&cli.url).and_then(|a| setup(&install::uninstall(&a), dry, ""))
        }
        Command::Doctor => Setup { only: vec![], dry_run: false, php_ini_dir: None, shell_profile: None }
            .args(&cli.url)
            .and_then(|a| setup(&install::doctor(&a, &cli.url), false, "")),
        Command::Expect { expect, project, all_projects, unordered, allow_errors, timeout, lookback, command } => {
            let a = expect::Args { expect, project, all_projects, unordered, allow_errors, timeout, lookback, command };
            return match expect::run(&cli.url, &a) {
                Ok(true) => ExitCode::SUCCESS,
                Ok(false) => ExitCode::from(1),
                Err(e) => {
                    eprintln!("glint: {e}");
                    ExitCode::from(2)
                }
            };
        }
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
