//! `glint install` / `glint uninstall` / `glint doctor`: make Glint work with zero
//! configuration on a developer machine.
//!
//! - **service**: copies `glintd` and `glint` to `~/.glint/bin` and starts glintd at login
//!   (launchd on macOS, a systemd user unit on Linux).
//! - **php** (8.0 or newer): `auto_prepend_file` in PHP's ini scan directory points at a small
//!   stub next to the ini file, which includes `~/.glint/sdk/php/autoload.php` only when it exists
//!   and PHP is 8.0+, so a deleted `~/.glint` or an older PHP never stops a script. An existing
//!   prepend file is chained.
//! - **python**: a `.pth` file in the user site-packages that puts `glint` into builtins.
//! - **node** (18.19+ / 20.6+, which have `--import`): `NODE_OPTIONS=--import …/global.mjs` in the
//!   shell profile, set only while that file exists, so `glint` is a global.
//!
//! The SDKs are embedded in this binary and written to `~/.glint/sdk`. Every file and profile
//! block written outside `~/.glint/sdk` is recorded in `~/.glint/installed.json`, which
//! `glint uninstall` and `glint doctor` read, so a tool that left PATH since is still cleaned up.
//! Event data in `~/.glint` is kept. SDKs stay off in production (`NODE_ENV` /
//! `APP_ENV=production`) unless `GLINT_ENABLED=1`.
//!
//! The service step stops whatever glintd holds the port before the login service starts (a
//! LaunchAgent with `KeepAlive {SuccessfulExit=false}`, so a glintd that cannot start is not
//! restarted in a tight loop), then checks that the new one answers with this version.
//!
//! `GLINT_SKIP_SERVICE_START=1` writes the login service file but leaves launchd and systemd alone:
//! a check in a sandbox HOME (scripts/clean-mac-check.sh) must not replace the real service.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub(crate) const MARK_BEGIN: &str = "# >>> glint >>>";
pub(crate) const MARK_END: &str = "# <<< glint <<<";
const SERVICE_LABEL: &str = "local.glint.glintd";

/// Leave launchd and systemd alone (see the module docs).
fn skip_service_start() -> bool {
    std::env::var("GLINT_SKIP_SERVICE_START").is_ok_and(|v| !v.is_empty() && v != "0")
}

macro_rules! sdk_files {
    ($($path:literal),* $(,)?) => { &[$(($path, include_str!(concat!("../../../sdks/", $path)))),*] };
}
pub(crate) const JS: &[(&str, &str)] = sdk_files!["js/src/index.js", "js/src/encode.js", "js/src/frameworks.js", "js/src/vite.js", "js/src/index.d.ts", "js/package.json"];
pub(crate) const PYTHON: &[(&str, &str)] = sdk_files![
    "python/glint/__init__.py", "python/glint/_config.py", "python/glint/_encode.py",
    "python/glint/_ids.py", "python/glint/_origin.py", "python/glint/_transport.py", "python/glint/_frameworks.py", "python/glint/py.typed",
];
pub(crate) const PHP: &[(&str, &str)] = sdk_files![
    "php/src/Transport.php", "php/src/Encoder.php", "php/src/Entry.php", "php/src/Glint.php", "php/src/GlintStop.php", "php/src/WordPress.php",
    "php/src/Laravel.php", "php/src/LaravelServiceProvider.php", "php/src/LaravelServeCommand.php",
    "php/src/laravel-env.php", "php/src/laravel-server.php", "php/src/helpers.php",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Service,
    Php,
    Python,
    Node,
    Agents,
}

impl Target {
    pub const ALL: [Target; 5] = [Target::Service, Target::Php, Target::Python, Target::Node, Target::Agents];
    pub fn parse(s: &str) -> Result<Target, String> {
        match s {
            "service" => Ok(Target::Service),
            "php" => Ok(Target::Php),
            "python" => Ok(Target::Python),
            "node" | "js" => Ok(Target::Node),
            "agents" | "mcp" => Ok(Target::Agents),
            other => Err(format!("unknown target `{other}` (service, php, python, node, agents)")),
        }
    }
}

pub struct Args {
    pub targets: Vec<Target>,
    pub dry_run: bool,
    pub home: PathBuf,
    pub php_ini_dir: Option<PathBuf>,
    pub shell_profile: Option<PathBuf>,
    /// Where glintd listens; agents get `<url>/mcp`.
    pub url: String,
    /// Also find agents by their app and command, not only their settings folder (off in tests).
    pub probe: bool,
}

impl Args {
    fn glint_dir(&self) -> PathBuf {
        self.home.join(".glint")
    }

    /// True when HOME is the account's own home folder (from the user database), not a sandbox
    /// a test or scripts/clean-mac-check.sh made. Only then are files outside HOME that this
    /// command did not record (other PHPs' ini folders) looked for and removed.
    fn real_home(&self) -> bool {
        let Some(home) = account_home() else { return false };
        let canon = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
        canon(&home) == canon(&self.home)
    }
}

fn account_home() -> Option<PathBuf> {
    let user = Command::new("id").arg("-un").output().ok().filter(|o| o.status.success())?;
    let user = String::from_utf8_lossy(&user.stdout).trim().to_owned();
    if cfg!(target_os = "macos") {
        let out = Command::new("dscl").args([".", "-read", &format!("/Users/{user}"), "NFSHomeDirectory"]).output().ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        Some(PathBuf::from(text.split_once(':')?.1.trim()))
    } else {
        let out = Command::new("getent").args(["passwd", &user]).output().ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        text.trim().split(':').nth(5).map(PathBuf::from)
    }
}

// ------------------------------------------------------------------ what install wrote

/// One file or profile block `glint install` wrote outside `~/.glint/sdk`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Written {
    pub target: &'static str,
    pub path: PathBuf,
    /// A marked block in a file the user owns (a shell profile), not a whole file of ours.
    pub block: bool,
}

fn file(target: &'static str, path: &Path) -> Written {
    Written { target, path: path.to_path_buf(), block: false }
}

fn manifest_path(a: &Args) -> PathBuf {
    a.glint_dir().join("installed.json")
}

fn target_name(t: &str) -> Option<&'static str> {
    ["service", "php", "python", "node"].into_iter().find(|n| *n == t)
}

/// `~/.glint/installed.json`, or nothing when it is missing or unreadable.
pub(crate) fn load_manifest(a: &Args) -> Vec<Written> {
    let Ok(text) = fs::read_to_string(manifest_path(a)) else { return Vec::new() };
    let v: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
    v["written"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|e| Some(Written { target: target_name(e["target"].as_str()?)?, path: PathBuf::from(e["path"].as_str()?), block: e["how"] == "block" }))
        .collect()
}

fn save_manifest(a: &Args, list: &[Written]) -> Result<(), String> {
    let path = manifest_path(a);
    if list.is_empty() {
        if !a.dry_run {
            let _ = fs::remove_file(&path);
        }
        return Ok(());
    }
    let json = serde_json::json!({
        "about": "Written by `glint install`: what it changed outside ~/.glint/sdk. `glint uninstall` removes these, `glint doctor` checks them.",
        "version": env!("CARGO_PKG_VERSION"),
        "written": list.iter().map(|w| serde_json::json!({ "target": w.target, "path": w.path, "how": if w.block { "block" } else { "file" } })).collect::<Vec<_>>(),
    });
    write(a, &path, &(serde_json::to_string_pretty(&json).unwrap_or_default() + "\n"))
}

fn recorded(a: &Args, target: &str) -> Vec<Written> {
    load_manifest(a).into_iter().filter(|w| w.target == target).collect()
}

/// What one step did, for the checklist the command prints.
pub struct Step {
    pub name: &'static str,
    pub result: Result<String, String>,
    pub skipped: bool,
}

pub(crate) fn done(name: &'static str, detail: String) -> Step {
    Step { name, result: Ok(detail), skipped: false }
}
pub(crate) fn skip(name: &'static str, why: String) -> Step {
    Step { name, result: Ok(why), skipped: true }
}
pub(crate) fn fail(name: &'static str, err: String) -> Step {
    Step { name, result: Err(err), skipped: false }
}

/// `[{name, status: ok|skip|fail, detail}]`, as `--json` prints it and the window reads it.
pub fn to_json(steps: &[Step]) -> serde_json::Value {
    let list = steps
        .iter()
        .map(|s| {
            let (status, detail) = match &s.result {
                Ok(d) if s.skipped => ("skip", d),
                Ok(d) => ("ok", d),
                Err(e) => ("fail", e),
            };
            serde_json::json!({ "name": s.name, "status": status, "detail": detail })
        })
        .collect();
    serde_json::Value::Array(list)
}

pub fn print(steps: &[Step]) -> bool {
    let mut ok = true;
    for s in steps {
        match &s.result {
            Ok(d) if s.skipped => println!("  –  {:<8} {d}", s.name),
            Ok(d) => println!("  ✓  {:<8} {d}", s.name),
            Err(e) => {
                ok = false;
                println!("  ✗  {:<8} {e}", s.name);
            }
        }
    }
    ok
}

// ------------------------------------------------------------------ install

pub fn install(a: &Args) -> Vec<Step> {
    let mut wrote = Vec::new();
    let steps = a
        .targets
        .iter()
        .flat_map(|t| match t {
            Target::Service => vec![install_service(a, &mut wrote)],
            Target::Php => vec![install_php(a, &mut wrote)],
            Target::Python => vec![install_python(a, &mut wrote)],
            Target::Node => vec![install_node(a, &mut wrote)],
            Target::Agents => crate::agents::install(a),
        })
        .collect();
    // Earlier entries stay: a PHP or profile set up before is still ours to remove.
    let mut all = load_manifest(a);
    for w in wrote {
        if !all.contains(&w) {
            all.push(w);
        }
    }
    let _ = save_manifest(a, &all);
    steps
}

pub(crate) fn write(a: &Args, path: &Path, contents: &str) -> Result<(), String> {
    if a.dry_run {
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    }
    // Replaced in one step (temp file + rename), so a reader never sees half a file and a full
    // disk leaves the old one intact. A symlinked config (dotfiles) is written where it points,
    // and keeps its permissions.
    let target = fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
    let name = target.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let tmp = target.with_file_name(format!(".{name}.glint-tmp-{}", std::process::id()));
    let result = fs::write(&tmp, contents)
        .and_then(|()| match fs::metadata(&target) {
            Ok(m) => fs::set_permissions(&tmp, m.permissions()),
            Err(_) => Ok(()),
        })
        .and_then(|()| fs::rename(&tmp, &target));
    result.map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("writing {}: {e}", path.display())
    })
}

fn write_sdk(a: &Args, files: &[(&str, &str)], strip: &str, dest: &Path) -> Result<(), String> {
    for (rel, body) in files {
        let rel = rel.strip_prefix(strip).unwrap_or(rel);
        write(a, &dest.join(rel), body)?;
    }
    Ok(())
}

fn run(cmd: impl AsRef<std::ffi::OsStr>, args: &[&str], home: &Path) -> Option<String> {
    let out = Command::new(cmd).args(args).env("HOME", home).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

fn install_service(a: &Args, wrote: &mut Vec<Written>) -> Step {
    let bin = a.glint_dir().join("bin");
    let here = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => return fail("service", format!("cannot find this executable: {e}")),
    };
    let name = if cfg!(windows) { "glintd.exe" } else { "glintd" };
    // The glintd that ran this setup (GLINT_DAEMON, set by glintd's "Set up this Mac", which may
    // run this binary from a copy in ~/.glint/cache), next to this binary (a build, ~/.glint/bin),
    // or inside Glint.app, where this binary is Contents/Resources/bin/glint and glintd is
    // Contents/MacOS/glintd.
    let from_env = std::env::var_os("GLINT_DAEMON").map(PathBuf::from).filter(|p| p.file_name().is_some_and(|n| n == name));
    let daemon = from_env.into_iter().chain([here.with_file_name(name), here.parent().and_then(|p| p.parent()).and_then(|p| p.parent()).map(|c| c.join("MacOS").join(name)).unwrap_or_default()])
        .find(|p| p.is_file())
        .unwrap_or_else(|| here.with_file_name(name));
    if !daemon.exists() {
        return fail("service", format!("glintd not found next to {}: build or install it first (cargo install --path crates/glintd)", here.display()));
    }
    let target_daemon = bin.join("glintd");
    if !a.dry_run
        && let Err(e) = fs::create_dir_all(&bin).and_then(|()| replace_file(&daemon, &target_daemon)).and_then(|()| replace_file(&here, &bin.join("glint"))) {
            return fail("service", format!("copying binaries to {}: {e}", bin.display()));
        }
    let config = a.glint_dir().join("config.toml");
    if !config.exists()
        && let Err(e) = write(a, &config, &glint_core::config::Settings::default().to_file())
    {
        return fail("service", e);
    }
    let log = a.glint_dir().join("glintd.log");
    let version = env!("CARGO_PKG_VERSION");
    if cfg!(target_os = "macos") {
        let plist = plist_path(&a.home);
        if let Err(e) = write(a, &plist, &plist_body(&target_daemon, &log)) {
            return fail("service", e);
        }
        wrote.push(file("service", &plist));
        if a.dry_run || skip_service_start() {
            let note = if skip_service_start() { " (launchd left alone: GLINT_SKIP_SERVICE_START)" } else { "" };
            return done("service", format!("glintd starts at login ({}) · binaries in {}{note}", plist.display(), bin.display()));
        }
        let target = launchd_target(&a.home);
        let domain = target.rsplit_once('/').map_or(String::new(), |(d, _)| d.to_owned());
        let plist_s = plist.to_string_lossy().into_owned();
        let holder = daemon_info(&a.url);
        if let Some(setup) = setup_daemon(&holder) {
            // This setup runs inside that glintd (the window's "Set up this Mac"): stopping it now
            // would cut off its answer. A detached script swaps it for the service a few seconds
            // after this command ends; the service waits for the port (GLINT_WAIT_FOR_PORT).
            let script = format!(
                "sleep 3; launchctl bootout {t} 2>/dev/null; ps -p {setup} -o comm= | grep -q glintd && kill -TERM {setup}; launchctl bootstrap {domain} {p}",
                t = sh_quote(&target),
                domain = sh_quote(&domain),
                p = sh_quote(&plist_s)
            );
            if let Err(e) = detached(&script) {
                return fail("service", format!("wrote {} but could not start it: {e}", plist.display()));
            }
            return done("service", format!("glintd starts at login ({}) · binaries in {} · the glintd running this setup {HANDS_OVER}", plist.display(), bin.display()));
        }
        // The service's own glintd first, and wait until launchd has really let it go.
        let _ = Command::new("launchctl").args(["bootout", &target]).output();
        wait_for(50, || !launchd_loaded(&target));
        // Then any other glintd on the port (one the app or a terminal started).
        if let Err(e) = stop_glintd(&a.url, &holder) {
            return fail("service", e);
        }
        let ok = Command::new("launchctl").args(["bootstrap", &domain, &plist_s]).status().is_ok_and(|s| s.success());
        if !ok {
            return fail("service", format!("wrote {} but launchctl bootstrap failed; run it yourself or log out and in", plist.display()));
        }
        if let Err(e) = expect_version(&a.url, version, &log) {
            return fail("service", e);
        }
        done("service", format!("glintd {version} runs now and starts at login ({}) · binaries in {}", plist.display(), bin.display()))
    } else if cfg!(target_os = "linux") {
        let unit = a.home.join(".config/systemd/user/glintd.service");
        let body = format!(
            "# Written by `glint install`; removed by `glint uninstall`.\n[Unit]\nDescription=Glint debug desk\n\n[Service]\nExecStart={}\nEnvironment=GLINT_WAIT_FOR_PORT=1\nRestart=on-failure\nRestartSec=10\n\n[Install]\nWantedBy=default.target\n",
            target_daemon.display()
        );
        if let Err(e) = write(a, &unit, &body) {
            return fail("service", e);
        }
        wrote.push(file("service", &unit));
        if a.dry_run || skip_service_start() {
            return done("service", format!("glintd starts at login ({}) · binaries in {}", unit.display(), bin.display()));
        }
        let holder = daemon_info(&a.url);
        let _ = Command::new("systemctl").args(["--user", "daemon-reload"]).status();
        if let Some(setup) = setup_daemon(&holder) {
            let script = format!("sleep 3; systemctl --user stop glintd.service; ps -p {setup} -o comm= | grep -q glintd && kill -TERM {setup}; systemctl --user enable --now glintd.service");
            if let Err(e) = detached(&script) {
                return fail("service", format!("wrote {} but could not start it: {e}", unit.display()));
            }
            return done("service", format!("glintd starts at login ({}) · binaries in {} · the glintd running this setup {HANDS_OVER}", unit.display(), bin.display()));
        }
        let _ = Command::new("systemctl").args(["--user", "stop", "glintd.service"]).status();
        if let Err(e) = stop_glintd(&a.url, &holder) {
            return fail("service", e);
        }
        let ok = Command::new("systemctl").args(["--user", "enable", "--now", "glintd.service"]).status().is_ok_and(|s| s.success());
        if !ok {
            return fail("service", format!("wrote {} but systemctl --user enable failed", unit.display()));
        }
        if let Err(e) = expect_version(&a.url, version, &log) {
            return fail("service", e);
        }
        done("service", format!("glintd {version} runs now and starts at login ({}) · binaries in {}", unit.display(), bin.display()))
    } else {
        skip("service", "automatic start is only set up on macOS and Linux; run glintd yourself".into())
    }
}

/// Said by the service step when the glintd running setup is replaced after this command ends.
const HANDS_OVER: &str = "hands over to the service in a few seconds";

fn plist_path(home: &Path) -> PathBuf {
    home.join("Library/LaunchAgents").join(format!("{SERVICE_LABEL}.plist"))
}

/// The LaunchAgent. `KeepAlive {SuccessfulExit=false}`: restarted when it fails, not when it
/// stops cleanly; `ThrottleInterval` spaces out restarts. `GLINT_WAIT_FOR_PORT`: a taken port is
/// waited for rather than an error, so the service never fights another glintd for it.
fn plist_body(daemon: &Path, log: &Path) -> String {
    let x = |p: &Path| p.display().to_string().replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<!-- Written by `glint install`; removed by `glint uninstall`. -->
<plist version="1.0"><dict>
  <key>Label</key><string>{SERVICE_LABEL}</string>
  <key>ProgramArguments</key><array><string>{}</string></array>
  <key>EnvironmentVariables</key><dict><key>GLINT_WAIT_FOR_PORT</key><string>1</string></dict>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>
  <key>ThrottleInterval</key><integer>10</integer>
  <key>StandardOutPath</key><string>{}</string>
  <key>StandardErrorPath</key><string>{}</string>
</dict></plist>
"#,
        x(daemon),
        x(log),
        x(log)
    )
}

fn launchd_target(home: &Path) -> String {
    let uid = run("id", &["-u"], home).unwrap_or_default();
    format!("gui/{uid}/{SERVICE_LABEL}")
}

fn launchd_loaded(target: &str) -> bool {
    Command::new("launchctl").args(["print", target]).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status().is_ok_and(|s| s.success())
}

/// `(running, pid)` of the LaunchAgent, or None when launchd has not loaded it.
fn launchd_state(target: &str) -> Option<(bool, Option<u32>)> {
    let out = Command::new("launchctl").args(["print", target]).output().ok().filter(|o| o.status.success())?;
    let text = String::from_utf8_lossy(&out.stdout);
    let field = |k: &str| text.lines().map(str::trim).find_map(|l| l.strip_prefix(k).map(|v| v.trim().to_owned()));
    Some((field("state = ").is_some_and(|v| v == "running"), field("pid = ").and_then(|v| v.parse().ok())))
}

fn wait_for(tenths: u32, mut ok: impl FnMut() -> bool) -> bool {
    for _ in 0..tenths {
        if ok() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    ok()
}

/// `GET /v1/info` of whatever answers at `url`, when it is a glintd.
fn daemon_info(url: &str) -> Option<serde_json::Value> {
    let r = crate::client::builder().build().get(&format!("{}/v1/info", url.trim_end_matches('/'))).timeout(std::time::Duration::from_secs(1)).call().ok()?;
    let v: serde_json::Value = r.into_json().ok()?;
    (v["name"] == "glint").then_some(v)
}

/// The pid of the glintd running this setup (it sets GLINT_SETUP_PID), when that is the one on
/// the port.
fn setup_daemon(holder: &Option<serde_json::Value>) -> Option<u32> {
    let setup: u32 = std::env::var("GLINT_SETUP_PID").ok()?.parse().ok()?;
    let pid = holder.as_ref()?["pid"].as_u64()?;
    (u64::from(setup) == pid).then_some(setup)
}

/// Stops the glintd answering at `url` (SIGTERM, so it finishes its writes) and waits for the
/// port. A glintd older than 0.1.5 does not say its pid; then the listener on the port is found
/// with lsof, and stopped only when it is a glintd.
fn stop_glintd(url: &str, holder: &Option<serde_json::Value>) -> Result<(), String> {
    let Some(info) = holder else { return Ok(()) };
    let pids: Vec<String> = match info["pid"].as_u64() {
        Some(p) => vec![p.to_string()],
        None => {
            let port = url.trim_end_matches('/').rsplit(':').next().unwrap_or("23600");
            let out = Command::new("lsof").args(["-nP", &format!("-iTCP:{port}"), "-sTCP:LISTEN", "-t"]).output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
            out.split_whitespace()
                .filter(|pid| Command::new("ps").args(["-o", "comm=", "-p", pid]).output().is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim().ends_with("glintd")))
                .map(str::to_owned)
                .collect()
        }
    };
    for pid in &pids {
        let _ = Command::new("kill").args(["-TERM", pid]).status();
    }
    if wait_for(100, || daemon_info(url).is_none()) {
        Ok(())
    } else {
        Err(format!("glintd {} (pid {}) at {url} did not stop; quit it (or the Glint app) and run setup again", info["version"].as_str().unwrap_or("?"), pids.join(", ")))
    }
}

/// Waits until a glintd answers at `url` with `version`; else says what the log says.
fn expect_version(url: &str, version: &str, log: &Path) -> Result<(), String> {
    let mut seen = None;
    if wait_for(150, || {
        seen = daemon_info(url).and_then(|v| v["version"].as_str().map(str::to_owned));
        seen.as_deref() == Some(version)
    }) {
        return Ok(());
    }
    let last = last_log_line(log).map(|l| format!("; glintd.log says: {l}")).unwrap_or_default();
    Err(match seen {
        Some(v) => format!("started the service, but glintd {v} answers at {url}, not {version}{last}"),
        None => format!("started the service, but nothing answers at {url}{last}"),
    })
}

pub(crate) fn last_log_line(log: &Path) -> Option<String> {
    let text = fs::read_to_string(log).ok()?;
    text.lines().rev().find(|l| !l.trim().is_empty()).map(|l| l.chars().take(300).collect())
}

/// Runs `script` with sh in its own process group, so it outlives this command and the shell
/// that ran it.
fn detached(script: &str) -> Result<(), String> {
    let mut cmd = Command::new("/bin/sh");
    cmd.args(["-c", script]).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
    cmd.spawn().map(drop).map_err(|e| e.to_string())
}

/// A word for sh: single-quoted, with `'` written as `'\''`.
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Copy `from` over `to` through a new file and a rename. Overwriting a binary in place keeps its
/// inode, and macOS then kills the next launch with OS_REASON_CODESIGNING (stale signature cache);
/// a rename also leaves a running copy (this very `glint`, or glintd) untouched.
pub(crate) fn replace_file(from: &Path, to: &Path) -> std::io::Result<()> {
    if from == to {
        return Ok(());
    }
    let tmp = to.with_extension("new");
    fs::copy(from, &tmp)?;
    // The copy keeps a downloaded Glint.app's quarantine flag, and macOS kills a quarantined
    // glintd or glint at launch with no message. ~/.glint is ours to change, so drop it there.
    if cfg!(target_os = "macos") {
        let _ = std::process::Command::new("xattr")
            .args(["-d", "com.apple.quarantine"])
            .arg(&tmp)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
    fs::rename(&tmp, to)
}

/// What one PHP binary says about itself.
#[derive(Debug)]
struct Php {
    version: String,
    version_id: u32,
    /// Its ini scan directories (`php --ini`), first one first.
    scan_dirs: Vec<PathBuf>,
    prepend: String,
}

fn php_probe(bin: &Path, home: &Path) -> Option<Php> {
    let out = run(bin, &["-r", r#"echo PHP_VERSION_ID, "\n", PHP_VERSION, "\n", ini_get('auto_prepend_file');"#], home)?;
    let mut lines = out.lines();
    let version_id = lines.next()?.trim().parse().ok()?;
    let version = lines.next().unwrap_or("").trim().to_owned();
    let prepend = lines.next().unwrap_or("").trim().to_owned();
    let ini = run(bin, &["--ini"], home).unwrap_or_default();
    Some(Php { version, version_id, scan_dirs: scan_dirs(&ini), prepend })
}

fn scan_dirs(php_ini: &str) -> Vec<PathBuf> {
    let Some(line) = php_ini.lines().find(|l| l.starts_with("Scan for additional .ini files in:")) else { return Vec::new() };
    let dirs = line.split_once(':').map_or("", |x| x.1).trim();
    if dirs == "(none)" {
        return Vec::new();
    }
    dirs.split(':').filter(|d| !d.is_empty()).map(PathBuf::from).collect()
}

/// `name` on PATH.
fn which(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?).map(|d| d.join(name)).find(|p| p.is_file())
}

/// Entries of `dir` whose name starts with `prefix`, each joined with `rest`, that exist.
fn glob(dir: &Path, prefix: &str, rest: &str) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with(prefix))
        .map(|e| if rest.is_empty() { e.path() } else { e.path().join(rest) })
        .filter(|p| p.exists())
        .collect();
    found.sort();
    found
}

/// Every PHP this machine has, as far as we can tell: the one on PATH, Homebrew's versions,
/// Laravel Herd's, the system's. Deduplicated by their real path.
fn php_binaries(home: &Path) -> Vec<PathBuf> {
    let mut all: Vec<PathBuf> = which("php").into_iter().collect();
    for opt in ["/opt/homebrew/opt", "/usr/local/opt"] {
        all.extend(glob(Path::new(opt), "php", "bin/php"));
    }
    all.extend(glob(&home.join("Library/Application Support/Herd/bin"), "php", ""));
    for dir in ["/usr/bin", "/usr/local/bin", "/opt/homebrew/bin"] {
        all.extend(glob(Path::new(dir), "php", "").into_iter().filter(|p| {
            let n = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            n == "php" || n.strip_prefix("php").is_some_and(|v| v.chars().all(|c| c.is_ascii_digit() || c == '.') && !v.is_empty())
        }));
    }
    let mut seen = Vec::new();
    all.retain(|p| {
        let real = p.canonicalize().unwrap_or_else(|_| p.clone());
        let fresh = !seen.contains(&real);
        seen.push(real);
        fresh
    });
    all
}

/// A PHP string literal (single quotes) for `s`.
fn php_str(s: &str) -> String {
    format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"))
}

/// The file PHP's `auto_prepend_file` names. It lives next to 99-glint.ini, not in ~/.glint, and
/// is plain PHP 5 syntax: when ~/.glint is deleted or a PHP older than 8.0 reads the same ini
/// folder, it does nothing instead of stopping every script with a fatal error.
fn php_stub(autoload: &Path, chained: &str) -> String {
    let mut out = format!(
        "<?php\n// Written by `glint install`; removed by `glint uninstall`. PHP's auto_prepend_file points here.\nif (PHP_VERSION_ID >= 80000 && is_file({0})) {{\n    include {0};\n}}\n",
        php_str(&autoload.to_string_lossy())
    );
    if !chained.is_empty() {
        out.push_str(&format!("// The prepend file configured before Glint, kept working:\nif (is_file({0})) {{\n    require_once {0};\n}}\n", php_str(chained)));
    }
    out
}

const PHP_STUB: &str = "glint-prepend.php";
const PHP_INI: &str = "99-glint.ini";

fn install_php(a: &Args, wrote: &mut Vec<Written>) -> Step {
    let Some(php) = which("php").and_then(|b| php_probe(&b, &a.home)) else {
        return skip("php", "php not found on PATH".into());
    };
    if php.version_id < 80000 {
        return skip("php", format!("PHP {} is older than 8.0, which Glint's PHP SDK needs; nothing changed", php.version));
    }
    let ini_dir = match a.php_ini_dir.clone().or_else(|| php.scan_dirs.first().cloned()) {
        Some(d) => d,
        None => return skip("php", "PHP has no ini scan directory (php --ini); add auto_prepend_file by hand".into()),
    };
    let sdk = a.glint_dir().join("sdk/php");
    let autoload = sdk.join("autoload.php");
    let stub = ini_dir.join(PHP_STUB);
    // 0.1.4 and older pointed PHP at ~/.glint/sdk/php/autoload.php directly.
    let ours = |p: &str| p == stub.to_string_lossy() || p == autoload.to_string_lossy() || p.ends_with(".glint/sdk/php/autoload.php") || p.ends_with(PHP_STUB);
    let chained = if php.prepend.is_empty() || ours(&php.prepend) { String::new() } else { php.prepend.clone() };
    if let Err(e) = write_sdk(a, PHP, "php/", &sdk) {
        return fail("php", e);
    }
    // Nothing is loaded up front: the classes come from an autoloader registered last, so a
    // project's own glint/glint wins (Composer registers its autoloader first, with prepend).
    if let Err(e) = write(a, &autoload, include_str!("../../../sdks/global/autoload.php")) {
        return fail("php", e);
    }
    let ini = ini_dir.join(PHP_INI);
    let sudo = " (PHP's ini directory may need sudo; or pass --php-ini-dir)";
    if let Err(e) = write(a, &stub, &php_stub(&autoload, &chained)) {
        return fail("php", format!("{e}{sudo}"));
    }
    wrote.push(file("php", &stub));
    if let Err(e) = write(a, &ini, &format!("; Written by `glint install`; removed by `glint uninstall`.\nauto_prepend_file=\"{}\"\n", stub.display())) {
        return fail("php", format!("{e}{sudo}"));
    }
    wrote.push(file("php", &ini));
    done(
        "php",
        format!(
            "glint() in every script of PHP {} via {}{}",
            php.version,
            ini.display(),
            if chained.is_empty() { String::new() } else { format!(" (chains your previous prepend {chained})") }
        ),
    )
}

fn install_python(a: &Args, wrote: &mut Vec<Written>) -> Step {
    let site = match run("python3", &["-c", "import site; print(site.getusersitepackages() if site.ENABLE_USER_SITE is not False else '')"], &a.home) {
        None => return skip("python", "python3 not found on PATH".into()),
        Some(s) if s.is_empty() => return skip("python", "this python3 has user site-packages disabled".into()),
        Some(s) => PathBuf::from(s),
    };
    let sdk = a.glint_dir().join("sdk/python");
    if let Err(e) = write_sdk(a, PYTHON, "python/", &sdk) {
        return fail("python", e);
    }
    // Only the shim is on sys.path; the SDK is imported on first use, and a project's own
    // `glint` package wins over this copy.
    let shim_dir = a.glint_dir().join("sdk/python-shim");
    let shim = include_str!("../../../sdks/global/glint_builtin.py").replace("__GLINT_SDK__", &sdk.display().to_string().replace('\\', "\\\\"));
    if let Err(e) = write(a, &shim_dir.join("glint_builtin.py"), &shim) {
        return fail("python", e);
    }
    let pth = site.join("glint.pth");
    if let Err(e) = write(a, &pth, &pth_body(&shim_dir)) {
        return fail("python", e);
    }
    wrote.push(file("python", &pth));
    done("python", format!("glint() is a builtin via {}", pth.display()))
}

fn pth_body(shim_dir: &Path) -> String {
    format!("{}\nimport glint_builtin\n", shim_dir.display())
}

/// Node versions with `--import` in NODE_OPTIONS: 18.19+, 20.6+, 21 and later. An older Node
/// refuses to start at all when NODE_OPTIONS names an option it does not know.
pub(crate) fn node_has_import(version: &str) -> bool {
    let mut parts = version.trim().trim_start_matches('v').split('.').map(|p| p.parse::<u32>().unwrap_or(0));
    let (major, minor) = (parts.next().unwrap_or(0), parts.next().unwrap_or(0));
    match major {
        18 => minor >= 19,
        20 => minor >= 6,
        m => m >= 21,
    }
}

/// `file://` URL of `p`, percent-encoded, so a HOME with spaces or quotes stays one word in
/// NODE_OPTIONS (which Node splits on spaces).
pub(crate) fn file_url(p: &Path) -> String {
    let mut out = String::from("file://");
    for b in p.to_string_lossy().bytes() {
        if b.is_ascii_alphanumeric() || b"/-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The profile block: glint on PATH, and (when `global` is given) NODE_OPTIONS, set only while
/// that file exists and only once, however often the profile is read.
pub(crate) fn node_block(fish: bool, bin: &Path, global: Option<&Path>) -> String {
    let bin = bin.to_string_lossy();
    let mut out = format!("{MARK_BEGIN}\n");
    if fish {
        let q = |s: &str| format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"));
        out.push_str(&format!("set -gx PATH {} $PATH\n", q(&bin)));
        if let Some(g) = global {
            let url = file_url(g);
            out.push_str(&format!(
                "if test -f {}; and not string match -q -- '*{url}*' \"$NODE_OPTIONS\"\n    set -gx NODE_OPTIONS (string trim -- \"--import={url} $NODE_OPTIONS\")\nend\n",
                q(&g.to_string_lossy())
            ));
        }
    } else {
        out.push_str(&format!("export PATH={}:\"$PATH\"\n", sh_quote(&bin)));
        if let Some(g) = global {
            let url = file_url(g);
            out.push_str(&format!(
                "if [ -f {} ]; then\n  case \" ${{NODE_OPTIONS:-}} \" in\n    *'{url}'*) ;;\n    *) export NODE_OPTIONS=\"--import={url}${{NODE_OPTIONS:+ $NODE_OPTIONS}}\" ;;\n  esac\nfi\n",
                sh_quote(&g.to_string_lossy())
            ));
        }
    }
    out.push_str(MARK_END);
    out.push('\n');
    out
}

fn install_node(a: &Args, wrote: &mut Vec<Written>) -> Step {
    let version = run("node", &["--version"], &a.home);
    let usable = version.as_deref().is_some_and(node_has_import);
    let sdk = a.glint_dir().join("sdk/js");
    let global = sdk.join("global.mjs");
    if usable {
        if let Err(e) = write_sdk(a, JS, "js/", &sdk) {
            return fail("node", e);
        }
        if let Err(e) = write(a, &global, include_str!("../../../sdks/global/global.mjs")) {
            return fail("node", e);
        }
    }
    let Some(profile) = a.shell_profile.clone().or_else(|| default_profile(&a.home)) else {
        return skip("node", format!("unknown shell; add NODE_OPTIONS=\"--import={}\" yourself", file_url(&global)));
    };
    let fish = profile.extension().is_some_and(|e| e == "fish");
    // The block also puts `glint` on PATH, which matters without Node too.
    let block = node_block(fish, &a.glint_dir().join("bin"), usable.then_some(global.as_path()));
    let current = fs::read_to_string(&profile).unwrap_or_default();
    if let Err(e) = write(a, &profile, &with_block(&current, &block)) {
        return fail("node", e);
    }
    wrote.push(Written { target: "node", path: profile.clone(), block: true });
    match version {
        None => skip("node", format!("node not found on PATH; glint is on PATH via {}", profile.display())),
        Some(v) if !usable => skip("node", format!("Node {v} has no --import (needs 18.19+ or 20.6+), so NODE_OPTIONS is left alone; glint is on PATH via {}", profile.display())),
        Some(v) => done("node", format!("glint() is a global in Node {v} via NODE_OPTIONS in {} (open a new shell)", profile.display())),
    }
}

/// The profile of the login shell. zsh reads `$ZDOTDIR/.zshrc` when ZDOTDIR is set; it is used
/// only when it is inside HOME, so a test with a sandbox HOME never writes to the real one.
fn default_profile(home: &Path) -> Option<PathBuf> {
    let shell = std::env::var("SHELL").unwrap_or_default();
    let name = shell.rsplit('/').next().unwrap_or("");
    match name {
        "zsh" => {
            let zdot = std::env::var_os("ZDOTDIR").map(PathBuf::from).filter(|d| !d.as_os_str().is_empty() && d.starts_with(home));
            Some(zdot.unwrap_or_else(|| home.to_path_buf()).join(".zshrc"))
        }
        "bash" => Some(home.join(if cfg!(target_os = "macos") { ".bash_profile" } else { ".bashrc" })),
        "fish" => Some(home.join(".config/fish/conf.d/glint.fish")),
        _ => None,
    }
}

/// `text` with Glint's marked block replaced (or appended).
pub fn with_block(text: &str, block: &str) -> String {
    let mut out = without_block(text);
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(block);
    out
}

/// `text` with Glint's marked block removed.
pub fn without_block(text: &str) -> String {
    let mut out = String::new();
    let mut inside = false;
    for line in text.split_inclusive('\n') {
        let l = line.trim_end();
        if l == MARK_BEGIN {
            inside = true;
            continue;
        }
        if l == MARK_END {
            inside = false;
            continue;
        }
        if !inside {
            out.push_str(line);
        }
    }
    out
}

// ------------------------------------------------------------------ uninstall

pub fn uninstall(a: &Args) -> Vec<Step> {
    let mut steps = Vec::new();
    let mut manifest = load_manifest(a);
    let rm = |p: &Path| -> bool { !a.dry_run && fs::remove_file(p).is_ok() || a.dry_run && p.exists() };
    for t in &a.targets {
        match t {
            Target::Service => {
                let mut removed = Vec::new();
                if cfg!(target_os = "macos") {
                    if !a.dry_run && !skip_service_start() {
                        let target = launchd_target(&a.home);
                        let _ = Command::new("launchctl").args(["bootout", &target]).output();
                        wait_for(50, || !launchd_loaded(&target));
                    }
                } else if cfg!(target_os = "linux") && !a.dry_run && !skip_service_start() {
                    let _ = Command::new("systemctl").args(["--user", "disable", "--now", "glintd.service"]).status();
                }
                let mut files: Vec<PathBuf> = recorded(a, "service").into_iter().map(|w| w.path).collect();
                files.push(if cfg!(target_os = "macos") { plist_path(&a.home) } else { a.home.join(".config/systemd/user/glintd.service") });
                files.dedup();
                for f in files {
                    if rm(&f) {
                        removed.push(f.display().to_string());
                    }
                }
                steps.push(if removed.is_empty() { skip("service", "not installed".into()) } else { done("service", format!("stopped and removed {}", removed.join(", "))) });
            }
            Target::Php => {
                let removed: Vec<String> = php_files_to_remove(a).into_iter().filter(|p| rm(p)).map(|p| p.display().to_string()).collect();
                steps.push(if removed.is_empty() { skip("php", "not installed".into()) } else { done("php", format!("removed {}", removed.join(", "))) });
            }
            Target::Python => {
                let mut files: Vec<PathBuf> = recorded(a, "python").into_iter().map(|w| w.path).collect();
                if let Some(site) = run("python3", &["-c", "import site; print(site.getusersitepackages())"], &a.home) {
                    files.push(PathBuf::from(site).join("glint.pth"));
                }
                files.sort();
                files.dedup();
                let removed: Vec<String> = files.into_iter().filter(|p| ours(p) && rm(p)).map(|p| p.display().to_string()).collect();
                steps.push(if removed.is_empty() { skip("python", "not installed".into()) } else { done("python", format!("removed {}", removed.join(", "))) });
            }
            Target::Node => {
                let mut profiles: Vec<PathBuf> = recorded(a, "node").into_iter().map(|w| w.path).collect();
                profiles.extend(a.shell_profile.clone().or_else(|| default_profile(&a.home)));
                profiles.sort();
                profiles.dedup();
                let mut cleaned = Vec::new();
                for p in profiles {
                    if let Ok(t) = fs::read_to_string(&p)
                        && t.contains(MARK_BEGIN)
                    {
                        match write(a, &p, &without_block(&t)) {
                            Ok(()) => cleaned.push(p.display().to_string()),
                            Err(e) => steps.push(fail("node", e)),
                        }
                    }
                }
                if !cleaned.is_empty() {
                    steps.push(done("node", format!("removed the glint block from {}", cleaned.join(", "))));
                } else if !steps.iter().any(|s| s.name == "node") {
                    steps.push(skip("node", "not installed".into()));
                }
            }
            Target::Agents => steps.extend(crate::agents::uninstall(a)),
        }
        let name = match t {
            Target::Service => "service",
            Target::Php => "php",
            Target::Python => "python",
            Target::Node => "node",
            Target::Agents => "agents",
        };
        manifest.retain(|w| w.target != name);
    }
    let _ = save_manifest(a, &manifest);
    steps.push(skip("data", format!("kept {} (events, SDK copies); delete it to remove everything", a.glint_dir().display())));
    steps
}

/// A file Glint wrote, by its first line (written by `glint install`) or, for a .pth, by what it
/// imports. Uninstall never removes a file someone else put at the same name.
fn ours(p: &Path) -> bool {
    fs::read_to_string(p).is_ok_and(|t| t.contains("glint install") || t.contains("import glint_builtin"))
}

/// 99-glint.ini and its stub in every PHP ini folder we know of: the ones install recorded, the
/// one `--php-ini-dir` names, and every installed PHP's (Homebrew versions, Herd, the system's).
/// The last only with the account's real HOME, never in a sandbox.
fn php_files_to_remove(a: &Args) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = recorded(a, "php").into_iter().filter_map(|w| w.path.parent().map(Path::to_path_buf)).collect();
    if let Some(d) = &a.php_ini_dir {
        dirs.push(d.clone());
    } else {
        // The PHP on PATH honours PHP_INI_SCAN_DIR, which is how a sandbox points it elsewhere.
        if let Some(php) = which("php").and_then(|b| php_probe(&b, &a.home)) {
            dirs.extend(php.scan_dirs);
        }
        if a.real_home() {
            for bin in php_binaries(&a.home) {
                dirs.extend(php_probe(&bin, &a.home).map(|p| p.scan_dirs).unwrap_or_default());
            }
            for etc in ["/opt/homebrew/etc/php", "/usr/local/etc/php"] {
                dirs.extend(glob(Path::new(etc), "", "conf.d"));
            }
            dirs.extend(glob(&a.home.join("Library/Application Support/Herd/config/php"), "", ""));
            dirs.extend(glob(Path::new("/etc/php"), "", "cli/conf.d"));
            dirs.extend(glob(Path::new("/etc/php"), "", "fpm/conf.d"));
            dirs.push(PathBuf::from("/etc/php.d"));
        }
    }
    let mut out = Vec::new();
    for d in dirs {
        for f in [d.join(PHP_INI), d.join(PHP_STUB)] {
            if !out.contains(&f) && ours(&f) {
                out.push(f);
            }
        }
    }
    out
}

// ------------------------------------------------------------------ doctor

pub fn doctor(a: &Args, base_url: &str) -> Vec<Step> {
    let mut steps = Vec::new();
    let info = crate::client::builder().build().get(&format!("{}/v1/info", base_url.trim_end_matches('/'))).timeout(std::time::Duration::from_secs(1)).call();
    let mut answering_pid = None;
    steps.push(match info {
        Ok(r) => {
            let v: serde_json::Value = r.into_json().unwrap_or_default();
            answering_pid = v["pid"].as_u64();
            let d = &v["data"];
            let data = if d.is_object() {
                format!(
                    " · {} events, {:.1} MB, kept {} days / at most {} events",
                    d["events"],
                    d["bytes"].as_f64().unwrap_or(0.0) / 1_048_576.0,
                    v["settings"]["keep_days"],
                    v["settings"]["max_events"]
                )
            } else {
                String::new()
            };
            done("daemon", format!("glintd {} at {base_url}{data}", v["version"].as_str().unwrap_or("?")))
        }
        Err(e) => fail("daemon", format!("not reachable at {base_url} ({e}); run `glint install` or start glintd")),
    });
    let check = |cmd: &Path, args: &[&str], stdin: &str| -> Option<bool> {
        use std::io::Write as _;
        use std::process::Stdio;
        let out = Command::new(cmd).args(args).env("HOME", &a.home).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().and_then(|mut c| {
            if let Some(mut i) = c.stdin.take() {
                let _ = i.write_all(stdin.as_bytes());
            }
            c.wait_with_output()
        });
        out.ok().map(|o| String::from_utf8_lossy(&o.stdout).trim() == "yes")
    };
    steps.push(doctor_service(a, answering_pid));
    // `php -r` skips auto_prepend_file; a script on stdin does not.
    const PHP_CHECK: &str = "<?php echo function_exists('glint') ? 'yes' : 'no';";
    const PY_CHECK: &[&str] = &["-c", "import builtins; print('yes' if hasattr(builtins, 'glint') else 'no')"];
    steps.push(match which("php") {
        None => skip("php", "php not found".into()),
        Some(bin) => match (check(&bin, &[], PHP_CHECK), php_probe(&bin, &a.home)) {
            (_, Some(p)) if p.version_id < 80000 => skip("php", format!("PHP {} is older than 8.0, which Glint's PHP SDK needs", p.version)),
            (Some(true), _) => done("php", "glint() available without setup".into()),
            _ => fail("php", "not set up yet: run `glint install --only php`".into()),
        },
    });
    steps.push(match which("python3") {
        None => skip("python", "python3 not found".into()),
        Some(bin) => match check(&bin, PY_CHECK, "") {
            Some(true) => done("python", "glint() available without setup".into()),
            _ => fail("python", "not set up yet: run `glint install --only python`".into()),
        },
    });
    steps.push(match which("node") {
        None => skip("node", "node not found".into()),
        Some(bin) => {
            let version = run(&bin, &["--version"], &a.home).unwrap_or_default();
            if !node_has_import(&version) {
                skip("node", format!("Node {version} has no --import (needs 18.19+ or 20.6+); glint() works there only with `import glint from` the SDK"))
            } else {
                match check(&bin, &["-e", "console.log(typeof glint === 'function' ? 'yes' : 'no')"], "") {
                    Some(true) => done("node", "glint() available without setup".into()),
                    _ => fail("node", "not set up yet: run `glint install --only node`, then open a new shell".into()),
                }
            }
        }
    });
    // Interpreters setup did not reach: other PHP versions, pyenv versions, a venv.
    let mut uncovered = Vec::new();
    let path_php = which("php").and_then(|p| p.canonicalize().ok());
    for bin in php_binaries(&a.home) {
        if bin.canonicalize().ok() == path_php {
            continue;
        }
        let Some(p) = php_probe(&bin, &a.home) else { continue };
        if p.version_id >= 80000 && check(&bin, &[], PHP_CHECK) == Some(false) {
            let dir = bin.parent().unwrap_or(Path::new("")).display().to_string();
            uncovered.push(format!("PHP {} ({}): run `PATH={}:$PATH glint install --only php`", p.version, bin.display(), sh_quote(&dir)));
        }
    }
    let shim = a.glint_dir().join("sdk/python-shim");
    let path_py = which("python3").and_then(|p| p.canonicalize().ok());
    for bin in python_binaries(&a.home) {
        if bin.canonicalize().ok() == path_py || check(&bin, PY_CHECK, "") != Some(false) {
            continue;
        }
        let venv = bin.parent().and_then(Path::parent).filter(|d| d.join("pyvenv.cfg").is_file());
        // A venv skips user site-packages, so the .pth goes into its own; any other Python gets
        // it in its user site-packages, which is per version.
        let code = if venv.is_some() { "import sysconfig; print(sysconfig.get_paths()['purelib'])" } else { "import site; print(site.getusersitepackages())" };
        let Some(site) = run(&bin, &["-c", code], &a.home) else { continue };
        let what = venv.map_or_else(|| bin.display().to_string(), |v| format!("the venv {}", v.display()));
        uncovered.push(format!(
            "{what}: run `mkdir -p {site} && printf '%s\\nimport glint_builtin\\n' {shim} > {pth}`",
            site = sh_quote(&site),
            shim = sh_quote(&shim.display().to_string()),
            pth = sh_quote(&Path::new(&site).join("glint.pth").display().to_string())
        ));
    }
    if !uncovered.is_empty() {
        steps.push(skip("coverage", format!("glint() is missing in {}", uncovered.join("; "))));
    }
    steps.extend(crate::agents::doctor(a));
    steps
}

/// python3 binaries besides the one on PATH: pyenv's versions, Homebrew's and python.org's
/// python3.x, and the active venv.
fn python_binaries(home: &Path) -> Vec<PathBuf> {
    let mut all = glob(&home.join(".pyenv/versions"), "", "bin/python3");
    for dir in ["/opt/homebrew/bin", "/usr/local/bin", "/Library/Frameworks/Python.framework/Versions/Current/bin"] {
        all.extend(glob(Path::new(dir), "python3.", "").into_iter().filter(|p| !p.to_string_lossy().ends_with("-config")));
    }
    if let Some(v) = std::env::var_os("VIRTUAL_ENV") {
        all.push(PathBuf::from(v).join("bin/python3"));
    }
    let mut seen = Vec::new();
    all.retain(|p| {
        let real = p.canonicalize().unwrap_or_else(|_| p.clone());
        let fresh = p.is_file() && !seen.contains(&real);
        seen.push(real);
        fresh
    });
    all
}

/// Is glintd's login service there and running, and is it the glintd that answers?
fn doctor_service(a: &Args, answering_pid: Option<u64>) -> Step {
    let log = a.glint_dir().join("glintd.log");
    let why = || last_log_line(&log).map(|l| format!("; glintd.log says: {l}")).unwrap_or_default();
    if cfg!(target_os = "macos") {
        let plist = plist_path(&a.home);
        if !plist.exists() {
            return fail("service", "glintd does not start at login yet: run `glint install`".into());
        }
        if skip_service_start() {
            return done("service", "glintd starts at login (launchd not checked: GLINT_SKIP_SERVICE_START)".into());
        }
        match launchd_state(&launchd_target(&a.home)) {
            None => fail("service", format!("{} exists but launchd has not loaded it: run `glint install --only service` or log out and in", plist.display())),
            Some((false, _)) => fail("service", format!("the login service is loaded but glintd is not running{}", why())),
            Some((true, pid)) => match (pid.map(u64::from), answering_pid) {
                (Some(p), Some(q)) if p != q => skip("service", format!("glintd starts at login; another glintd (pid {q}) holds the port now, and the service (pid {p}) takes over when it stops")),
                _ => done("service", "glintd starts at login and is running".into()),
            },
        }
    } else if cfg!(target_os = "linux") {
        let unit = a.home.join(".config/systemd/user/glintd.service");
        if !unit.exists() {
            return fail("service", "glintd does not start at login yet: run `glint install`".into());
        }
        if skip_service_start() {
            return done("service", "glintd starts at login (systemd not checked: GLINT_SKIP_SERVICE_START)".into());
        }
        match run("systemctl", &["--user", "is-active", "glintd.service"], &a.home) {
            Some(s) if s == "active" => done("service", "glintd starts at login and is running".into()),
            _ => fail("service", format!("the login service is not running (systemctl --user status glintd){}", why())),
        }
    } else {
        skip("service", "no login service on this system".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_block_is_idempotent_and_removable() {
        let block = format!("{MARK_BEGIN}\nexport X=1\n{MARK_END}\n");
        let once = with_block("alias ll='ls -l'", &block);
        assert_eq!(once, format!("alias ll='ls -l'\n{block}"));
        let twice = with_block(&once, &block.replace("X=1", "X=2"));
        assert_eq!(twice.matches(MARK_BEGIN).count(), 1);
        assert!(twice.contains("X=2") && !twice.contains("X=1"));
        assert_eq!(without_block(&twice), "alias ll='ls -l'\n");
    }

    #[test]
    fn node_import_needs_18_19_or_20_6() {
        for ok in ["v18.19.0", "v18.20.4", "v20.6.0", "v20.18.1", "v21.0.0", "v22.11.0", "v24.1.0"] {
            assert!(node_has_import(ok), "{ok}");
        }
        for old in ["v16.20.2", "v18.18.2", "v19.9.0", "v20.5.1", "garbage", ""] {
            assert!(!node_has_import(old), "{old}");
        }
    }

    #[test]
    fn file_urls_have_no_spaces_or_quotes() {
        let url = file_url(Path::new("/Users/Jane Doe/it's \"here\"/.glint/sdk/js/global.mjs"));
        assert_eq!(url, "file:///Users/Jane%20Doe/it%27s%20%22here%22/.glint/sdk/js/global.mjs");
    }

    #[test]
    fn the_profile_sets_node_options_only_when_the_file_exists_and_only_once() {
        let dir = std::env::temp_dir().join(format!("glint-profile-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let global = dir.join("my home/global.mjs");
        fs::create_dir_all(global.parent().unwrap()).unwrap();
        let profile = dir.join("profile.sh");
        fs::write(&profile, node_block(false, &dir.join("bin"), Some(&global))).unwrap();
        let run = |pre: &str| {
            let out = Command::new("/bin/sh").arg("-c").arg(format!("{pre} . {}; . {}; printf %s \"$NODE_OPTIONS\"", sh_quote(&profile.to_string_lossy()), sh_quote(&profile.to_string_lossy()))).output().unwrap();
            String::from_utf8_lossy(&out.stdout).into_owned()
        };
        // No file: NODE_OPTIONS untouched (a deleted ~/.glint must not stop Node).
        assert_eq!(run("NODE_OPTIONS='--max-old-space-size=100';"), "--max-old-space-size=100");
        fs::write(&global, "").unwrap();
        let url = file_url(&global);
        // Read twice (a nested shell), set once; what was there is kept.
        assert_eq!(run("NODE_OPTIONS='--max-old-space-size=100';"), format!("--import={url} --max-old-space-size=100"));
        assert_eq!(run("unset NODE_OPTIONS;"), format!("--import={url}"));
        // Without Node: only PATH.
        assert!(!node_block(false, &dir.join("bin"), None).contains("NODE_OPTIONS"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_php_stub_is_old_php_safe_and_survives_a_missing_sdk() {
        let stub = php_stub(Path::new("/home/o'neil/.glint/sdk/php/autoload.php"), "/etc/prepend.php");
        assert!(stub.contains("PHP_VERSION_ID >= 80000 && is_file('/home/o\\'neil/.glint/sdk/php/autoload.php')"));
        assert!(stub.contains("require_once '/etc/prepend.php'"));
        let Some(php) = which("php") else { return };
        let dir = std::env::temp_dir().join(format!("glint-stub-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let stub_file = dir.join(PHP_STUB);
        fs::write(&stub_file, php_stub(&dir.join("missing/autoload.php"), "")).unwrap();
        let out = Command::new(php).args(["-n", "-d", &format!("auto_prepend_file={}", stub_file.display()), "-r", "echo 'ran';"]).output().unwrap();
        let out2 = Command::new(which("php").unwrap()).args(["-n", "-d", &format!("auto_prepend_file={}", stub_file.display())]).stdin(std::process::Stdio::null()).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), "ran");
        assert!(out2.status.success(), "{}", String::from_utf8_lossy(&out2.stderr));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_launch_agent_restarts_only_after_a_failure_and_waits_for_the_port() {
        let body = plist_body(Path::new("/Users/a&b/.glint/bin/glintd"), Path::new("/tmp/glintd.log"));
        assert!(body.contains("<key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>"));
        assert!(body.contains("<key>ThrottleInterval</key><integer>10</integer>"));
        assert!(body.contains("<key>GLINT_WAIT_FOR_PORT</key><string>1</string>"));
        assert!(body.contains("/Users/a&amp;b/.glint/bin/glintd"));
    }

    #[test]
    fn php_scan_dirs_are_parsed() {
        let ini = "Configuration File (php.ini) Path: /opt/homebrew/etc/php/8.3\nScan for additional .ini files in: /opt/homebrew/etc/php/8.3/conf.d:/tmp/x\n";
        assert_eq!(scan_dirs(ini), vec![PathBuf::from("/opt/homebrew/etc/php/8.3/conf.d"), PathBuf::from("/tmp/x")]);
        assert!(scan_dirs("Scan for additional .ini files in: (none)\n").is_empty());
    }

    #[test]
    fn the_glintd_on_the_port_is_stopped_by_its_pid() {
        let Some(py) = which("python3") else { return };
        // A stand-in glintd: answers /v1/info with its own pid, as glintd 0.1.5+ does.
        let server = r#"
import http.server, json, os, sys
class H(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        body = json.dumps({"name": "glint", "version": "0.0.1", "pid": os.getpid()}).encode()
        self.send_response(200); self.send_header("content-type", "application/json"); self.end_headers(); self.wfile.write(body)
    def log_message(self, *a): pass
s = http.server.HTTPServer(("127.0.0.1", 0), H)
print(s.server_address[1], flush=True)
s.serve_forever()
"#;
        let mut child = Command::new(py).args(["-c", server]).stdout(std::process::Stdio::piped()).spawn().unwrap();
        let mut line = String::new();
        std::io::BufRead::read_line(&mut std::io::BufReader::new(child.stdout.take().unwrap()), &mut line).unwrap();
        let url = format!("http://127.0.0.1:{}", line.trim());
        let holder = daemon_info(&url);
        assert_eq!(holder.as_ref().unwrap()["pid"].as_u64(), Some(u64::from(child.id())));
        // Not the glintd running setup: it is stopped, and the port is free afterwards.
        assert_eq!(setup_daemon(&holder), None);
        stop_glintd(&url, &holder).unwrap();
        assert!(daemon_info(&url).is_none());
        let _ = child.wait();
    }

    #[test]
    fn embedded_sdks_are_complete() {
        assert!(JS.iter().any(|(p, b)| p.ends_with("index.js") && b.contains("export function glint")));
        assert!(PYTHON.iter().any(|(p, _)| p.ends_with("__init__.py")));
        assert!(PHP.iter().any(|(p, b)| p.ends_with("helpers.php") && b.contains("function glint")));
    }
}
