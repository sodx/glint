//! `glint install` / `glint uninstall` / `glint doctor`: make Glint work with zero
//! configuration on a developer machine.
//!
//! - **service**: copies `glintd` and `glint` to `~/.glint/bin` and starts glintd at login
//!   (launchd on macOS, a systemd user unit on Linux).
//! - **php**: `auto_prepend_file` in PHP's ini scan directory, so `glint()` exists in every
//!   script (the same mechanism as Spatie's global-ray). An existing prepend file is chained.
//! - **python**: a `.pth` file in the user site-packages that puts `glint` into builtins.
//! - **node**: `NODE_OPTIONS=--import …/global.mjs` in the shell profile, so `glint` is a global.
//!
//! The SDKs are embedded in this binary and written to `~/.glint/sdk`. Everything this command
//! writes is listed in its output and removed by `glint uninstall`; event data in `~/.glint` is
//! kept. SDKs stay off in production (`NODE_ENV` / `APP_ENV=production`) unless `GLINT_ENABLED=1`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub(crate) const MARK_BEGIN: &str = "# >>> glint >>>";
pub(crate) const MARK_END: &str = "# <<< glint <<<";
const SERVICE_LABEL: &str = "local.glint.glintd";

macro_rules! sdk_files {
    ($($path:literal),* $(,)?) => { &[$(($path, include_str!(concat!("../../../sdks/", $path)))),*] };
}
const JS: &[(&str, &str)] = sdk_files!["js/src/index.js", "js/src/encode.js", "js/src/index.d.ts", "js/package.json"];
const PYTHON: &[(&str, &str)] = sdk_files![
    "python/glint/__init__.py", "python/glint/_config.py", "python/glint/_encode.py",
    "python/glint/_ids.py", "python/glint/_origin.py", "python/glint/_transport.py", "python/glint/py.typed",
];
const PHP: &[(&str, &str)] = sdk_files![
    "php/src/Transport.php", "php/src/Encoder.php", "php/src/Entry.php", "php/src/Glint.php", "php/src/helpers.php",
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
}

impl Args {
    fn glint_dir(&self) -> PathBuf {
        self.home.join(".glint")
    }
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
    a.targets
        .iter()
        .flat_map(|t| match t {
            Target::Service => vec![install_service(a)],
            Target::Php => vec![install_php(a)],
            Target::Python => vec![install_python(a)],
            Target::Node => vec![install_node(a)],
            Target::Agents => crate::agents::install(a),
        })
        .collect()
}

pub(crate) fn write(a: &Args, path: &Path, contents: &str) -> Result<(), String> {
    if a.dry_run {
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    }
    fs::write(path, contents).map_err(|e| format!("writing {}: {e}", path.display()))
}

fn write_sdk(a: &Args, files: &[(&str, &str)], strip: &str, dest: &Path) -> Result<(), String> {
    for (rel, body) in files {
        let rel = rel.strip_prefix(strip).unwrap_or(rel);
        write(a, &dest.join(rel), body)?;
    }
    Ok(())
}

fn run(cmd: &str, args: &[&str], home: &Path) -> Option<String> {
    let out = Command::new(cmd).args(args).env("HOME", home).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

const DEFAULT_CONFIG: &str = "# Glint's settings (read by glintd at start; restart it, or run `glint install --only service`).\n\
# Each can also be set by an environment variable, which wins.\n\
\n\
# Events older than this many days are deleted (0 keeps everything).      GLINT_KEEP_DAYS\n\
keep_days = 7\n\
# At most this many events are kept; the oldest go first (0: no cap).       GLINT_MAX_EVENTS\n\
max_events = 100000\n\
# Where files open: vscode, cursor, windsurf, zed, phpstorm, idea, webstorm, pycharm, sublime.  GLINT_EDITOR\n\
# editor = \"vscode\"\n\
# addr = \"127.0.0.1:23600\"        GLINT_ADDR\n\
# ray_addr = \"127.0.0.1:23517\"    GLINT_RAY_ADDR   (\"off\" to disable)\n\
# otlp_addr = \"127.0.0.1:4318\"    GLINT_OTLP_ADDR  (\"off\" to disable)\n\
# db = \"~/.glint/glint.db\"         GLINT_DB\n";

fn install_service(a: &Args) -> Step {
    let bin = a.glint_dir().join("bin");
    let here = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => return fail("service", format!("cannot find this executable: {e}")),
    };
    let daemon = here.with_file_name(if cfg!(windows) { "glintd.exe" } else { "glintd" });
    if !daemon.exists() {
        return fail("service", format!("glintd not found next to {} — build or install it first (cargo install --path crates/glintd)", here.display()));
    }
    let target_daemon = bin.join("glintd");
    if !a.dry_run {
        if let Err(e) = fs::create_dir_all(&bin).and_then(|()| replace_file(&daemon, &target_daemon)).and_then(|()| replace_file(&here, &bin.join("glint"))) {
            return fail("service", format!("copying binaries to {}: {e}", bin.display()));
        }
    }
    let config = a.glint_dir().join("config.toml");
    if !config.exists()
        && let Err(e) = write(a, &config, DEFAULT_CONFIG)
    {
        return fail("service", e);
    }
    let log = a.glint_dir().join("glintd.log");
    if cfg!(target_os = "macos") {
        let plist = a.home.join("Library/LaunchAgents").join(format!("{SERVICE_LABEL}.plist"));
        let body = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<!-- Written by `glint install`; removed by `glint uninstall`. -->
<plist version="1.0"><dict>
  <key>Label</key><string>{SERVICE_LABEL}</string>
  <key>ProgramArguments</key><array><string>{}</string></array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>StandardOutPath</key><string>{}</string>
  <key>StandardErrorPath</key><string>{}</string>
</dict></plist>
"#,
            target_daemon.display(),
            log.display(),
            log.display()
        );
        if let Err(e) = write(a, &plist, &body) {
            return fail("service", e);
        }
        if !a.dry_run {
            let uid = run("id", &["-u"], &a.home).unwrap_or_default();
            let domain = format!("gui/{uid}");
            let _ = Command::new("launchctl").args(["bootout", &format!("{domain}/{SERVICE_LABEL}")]).output();
            let ok = Command::new("launchctl").args(["bootstrap", &domain, &plist.to_string_lossy()]).status().is_ok_and(|s| s.success());
            if !ok {
                return fail("service", format!("wrote {} but launchctl bootstrap failed; run it yourself or log out and in", plist.display()));
            }
        }
        done("service", format!("glintd starts at login ({}) · binaries in {}", plist.display(), bin.display()))
    } else if cfg!(target_os = "linux") {
        let unit = a.home.join(".config/systemd/user/glintd.service");
        let body = format!(
            "# Written by `glint install`; removed by `glint uninstall`.\n[Unit]\nDescription=Glint debug desk\n\n[Service]\nExecStart={}\nRestart=on-failure\n\n[Install]\nWantedBy=default.target\n",
            target_daemon.display()
        );
        if let Err(e) = write(a, &unit, &body) {
            return fail("service", e);
        }
        if !a.dry_run {
            let _ = Command::new("systemctl").args(["--user", "daemon-reload"]).status();
            let ok = Command::new("systemctl").args(["--user", "enable", "--now", "glintd.service"]).status().is_ok_and(|s| s.success());
            if !ok {
                return fail("service", format!("wrote {} but systemctl --user enable failed", unit.display()));
            }
        }
        done("service", format!("glintd starts at login ({}) · binaries in {}", unit.display(), bin.display()))
    } else {
        skip("service", "automatic start is only set up on macOS and Linux; run glintd yourself".into())
    }
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
    fs::rename(&tmp, to)
}

fn install_php(a: &Args) -> Step {
    if run("php", &["-r", "echo PHP_VERSION;"], &a.home).is_none() {
        return skip("php", "php not found on PATH".into());
    }
    let ini_dir = match &a.php_ini_dir {
        Some(d) => d.clone(),
        None => match php_scan_dir(&a.home) {
            Some(d) => d,
            None => return skip("php", "PHP has no ini scan directory (php --ini); add auto_prepend_file by hand".into()),
        },
    };
    let sdk = a.glint_dir().join("sdk/php");
    let prepend = sdk.join("autoload.php");
    let previous = run("php", &["-r", "echo ini_get('auto_prepend_file');"], &a.home).unwrap_or_default();
    let chained = if previous.is_empty() || previous == prepend.to_string_lossy() { String::new() } else { previous };
    if let Err(e) = write_sdk(a, PHP, "php/", &sdk) {
        return fail("php", e);
    }
    // Nothing is loaded up front: the classes come from an autoloader registered last, so a
    // project's own glint/glint wins (Composer registers its autoloader first, with prepend).
    // sdks/global/autoload.php, plus the prepend file that was configured before, kept working.
    let mut autoload = include_str!("../../../sdks/global/autoload.php").to_owned();
    if !chained.is_empty() {
        autoload.push_str(&format!("\n// The prepend file configured before Glint, kept working:\nif (is_file({0:?})) {{ require_once {0:?}; }}\n", chained));
    }
    if let Err(e) = write(a, &prepend, &autoload) {
        return fail("php", e);
    }
    let ini = ini_dir.join("99-glint.ini");
    if let Err(e) = write(a, &ini, &format!("; Written by `glint install`; removed by `glint uninstall`.\nauto_prepend_file=\"{}\"\n", prepend.display())) {
        return fail("php", format!("{e} (PHP's ini directory may need sudo; or pass --php-ini-dir)"));
    }
    done("php", format!("glint() in every script via {}{}", ini.display(), if chained.is_empty() { String::new() } else { format!(" (chains your previous prepend {chained})") }))
}

fn php_scan_dir(home: &Path) -> Option<PathBuf> {
    let out = run("php", &["--ini"], home)?;
    let line = out.lines().find(|l| l.starts_with("Scan for additional .ini files in:"))?;
    let dir = line.split_once(':')?.1.trim();
    (dir != "(none)" && !dir.is_empty()).then(|| PathBuf::from(dir.split(':').next().unwrap_or(dir)))
}

fn install_python(a: &Args) -> Step {
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
    if let Err(e) = write(a, &pth, &format!("{}\nimport glint_builtin\n", shim_dir.display())) {
        return fail("python", e);
    }
    done("python", format!("glint() is a builtin via {}", pth.display()))
}

fn install_node(a: &Args) -> Step {
    let version = match run("node", &["--version"], &a.home) {
        None => return skip("node", "node not found on PATH".into()),
        Some(v) => v,
    };
    let sdk = a.glint_dir().join("sdk/js");
    if let Err(e) = write_sdk(a, JS, "js/", &sdk) {
        return fail("node", e);
    }
    let global = sdk.join("global.mjs");
    if let Err(e) = write(a, &global, include_str!("../../../sdks/global/global.mjs")) {
        return fail("node", e);
    }
    let Some(profile) = a.shell_profile.clone().or_else(|| default_profile(&a.home)) else {
        return skip("node", format!("unknown shell; add NODE_OPTIONS=\"--import=file://{}\" yourself", global.display()));
    };
    let fish = profile.extension().is_some_and(|e| e == "fish");
    let block = if fish {
        format!("{MARK_BEGIN}\nset -gx PATH {0} $PATH\nset -gx NODE_OPTIONS \"--import=file://{1} $NODE_OPTIONS\"\n{MARK_END}\n", a.glint_dir().join("bin").display(), global.display())
    } else {
        format!("{MARK_BEGIN}\nexport PATH=\"{0}:$PATH\"\nexport NODE_OPTIONS=\"--import=file://{1} ${{NODE_OPTIONS:-}}\"\n{MARK_END}\n", a.glint_dir().join("bin").display(), global.display())
    };
    let current = fs::read_to_string(&profile).unwrap_or_default();
    if let Err(e) = write(a, &profile, &with_block(&current, &block)) {
        return fail("node", e);
    }
    done("node", format!("glint() is a global in Node {version} via NODE_OPTIONS in {} (open a new shell)", profile.display()))
}

fn default_profile(home: &Path) -> Option<PathBuf> {
    let shell = std::env::var("SHELL").unwrap_or_default();
    let name = shell.rsplit('/').next().unwrap_or("");
    match name {
        "zsh" => Some(home.join(".zshrc")),
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
    let rm = |p: &Path| -> bool { !a.dry_run && fs::remove_file(p).is_ok() || a.dry_run && p.exists() };
    for t in &a.targets {
        match t {
            Target::Service => {
                if cfg!(target_os = "macos") {
                    let plist = a.home.join("Library/LaunchAgents").join(format!("{SERVICE_LABEL}.plist"));
                    if !a.dry_run {
                        let uid = run("id", &["-u"], &a.home).unwrap_or_default();
                        let _ = Command::new("launchctl").args(["bootout", &format!("gui/{uid}/{SERVICE_LABEL}")]).output();
                    }
                    steps.push(if rm(&plist) { done("service", format!("stopped and removed {}", plist.display())) } else { skip("service", "not installed".into()) });
                } else if cfg!(target_os = "linux") {
                    if !a.dry_run {
                        let _ = Command::new("systemctl").args(["--user", "disable", "--now", "glintd.service"]).status();
                    }
                    let unit = a.home.join(".config/systemd/user/glintd.service");
                    steps.push(if rm(&unit) { done("service", format!("stopped and removed {}", unit.display())) } else { skip("service", "not installed".into()) });
                }
            }
            Target::Php => {
                let dir = a.php_ini_dir.clone().or_else(|| php_scan_dir(&a.home));
                let ini = dir.map(|d| d.join("99-glint.ini"));
                steps.push(match ini {
                    Some(p) if rm(&p) => done("php", format!("removed {}", p.display())),
                    _ => skip("php", "not installed".into()),
                });
            }
            Target::Python => {
                let site = run("python3", &["-c", "import site; print(site.getusersitepackages())"], &a.home).map(PathBuf::from);
                steps.push(match site.map(|s| s.join("glint.pth")) {
                    Some(p) if rm(&p) => done("python", format!("removed {}", p.display())),
                    _ => skip("python", "not installed".into()),
                });
            }
            Target::Node => {
                let profile = a.shell_profile.clone().or_else(|| default_profile(&a.home));
                steps.push(match profile {
                    Some(p) => match fs::read_to_string(&p) {
                        Ok(t) if t.contains(MARK_BEGIN) => match write(a, &p, &without_block(&t)) {
                            Ok(()) => done("node", format!("removed the glint block from {}", p.display())),
                            Err(e) => fail("node", e),
                        },
                        _ => skip("node", "not installed".into()),
                    },
                    None => skip("node", "unknown shell".into()),
                });
            }
            Target::Agents => steps.extend(crate::agents::uninstall(a)),
        }
    }
    steps.push(skip("data", format!("kept {} (events, SDK copies); delete it to remove everything", a.glint_dir().display())));
    steps
}

// ------------------------------------------------------------------ doctor

pub fn doctor(a: &Args, base_url: &str) -> Vec<Step> {
    let mut steps = Vec::new();
    let info = ureq::get(&format!("{}/v1/info", base_url.trim_end_matches('/'))).timeout(std::time::Duration::from_secs(1)).call();
    steps.push(match info {
        Ok(r) => {
            let v: serde_json::Value = r.into_json().unwrap_or_default();
            let d = &v["data"];
            let data = if d.is_object() {
                format!(
                    " · {} events, {:.1} MB, kept {} days / at most {} events",
                    d["events"],
                    d["bytes"].as_f64().unwrap_or(0.0) / 1_048_576.0,
                    d["keep_days"],
                    d["max_events"]
                )
            } else {
                String::new()
            };
            done("daemon", format!("glintd {} at {base_url}{data}", v["version"].as_str().unwrap_or("?")))
        }
        Err(e) => fail("daemon", format!("not reachable at {base_url} ({e}); run `glint install` or start glintd")),
    });
    let check = |name: &'static str, cmd: &str, args: &[&str], stdin: &str, hint: &str| -> Step {
        use std::io::Write as _;
        use std::process::Stdio;
        let out = Command::new(cmd).args(args).env("HOME", &a.home).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().and_then(|mut c| {
            if let Some(mut i) = c.stdin.take() {
                let _ = i.write_all(stdin.as_bytes());
            }
            c.wait_with_output()
        });
        match out {
            Err(_) => skip(name, format!("{cmd} not found")),
            Ok(o) if String::from_utf8_lossy(&o.stdout).trim() == "yes" => done(name, "glint() available without setup".into()),
            Ok(_) => fail(name, format!("glint() not available — {hint}")),
        }
    };
    // `php -r` skips auto_prepend_file; a script on stdin does not.
    steps.push(check("php", "php", &[], "<?php echo function_exists('glint') ? 'yes' : 'no';", "run `glint install --only php`"));
    steps.push(check("python", "python3", &["-c", "import builtins; print('yes' if hasattr(builtins, 'glint') else 'no')"], "", "run `glint install --only python`"));
    steps.push(check("node", "node", &["-e", "console.log(typeof glint === 'function' ? 'yes' : 'no')"], "", "run `glint install --only node`, then open a new shell"));
    steps.extend(crate::agents::doctor(a));
    steps
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
    fn embedded_sdks_are_complete() {
        assert!(JS.iter().any(|(p, b)| p.ends_with("index.js") && b.contains("export function glint")));
        assert!(PYTHON.iter().any(|(p, _)| p.ends_with("__init__.py")));
        assert!(PHP.iter().any(|(p, b)| p.ends_with("helpers.php") && b.contains("function glint")));
    }
}
