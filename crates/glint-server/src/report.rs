//! A diagnostics report for someone helping a tester: one text file in ~/Downloads with what
//! decides whether Glint works on this Mac. `glint report` writes it; the window's "Send
//! diagnostics" asks glintd (`POST /v1/report`), which runs `glint report` in the user's login
//! shell and, when the CLI cannot run at all (the case that most needs a report), writes the part
//! glintd can see itself. Nothing is sent anywhere: the user decides whom to give the file.
//!
//! Secrets stay out: config values whose key looks secret are replaced, and only the names of
//! GLINT_ variables with such names are listed.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// One titled block of the report.
pub type Section = (String, String);

const LOG_TAIL_LINES: usize = 200;
const SETUP_LOG_MAX: usize = 64 * 1024;

/// The user's login shell, from the user database (a process started at login has no $SHELL).
pub fn login_shell() -> String {
    let user = std::env::var("USER").unwrap_or_default();
    let from_db = if cfg!(target_os = "macos") {
        Command::new("dscl")
            .args([".", "-read", &format!("/Users/{user}"), "UserShell"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8_lossy(&o.stdout).split_whitespace().nth(1).map(str::to_owned))
    } else {
        Command::new("getent")
            .args(["passwd", &user])
            .output()
            .ok()
            .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().rsplit(':').next().map(str::to_owned))
    };
    from_db
        .or_else(|| std::env::var("SHELL").ok())
        .filter(|s| Path::new(s).is_file())
        .unwrap_or_else(|| "/bin/sh".into())
}

/// The Glint.app this process runs from, if any.
pub fn own_bundle() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    exe.ancestors().find(|a| a.extension().is_some_and(|x| x == "app")).map(Path::to_path_buf)
}

/// The Glint.app to describe: the given one, else the usual install places.
fn find_bundle(home: &Path, given: Option<&Path>) -> Option<PathBuf> {
    given
        .map(Path::to_path_buf)
        .into_iter()
        .chain([PathBuf::from("/Applications/Glint.app"), home.join("Applications/Glint.app")])
        .find(|p| p.is_dir())
}

/// Output of a short command, or why there is none. Gives up after `limit` (a shell profile can
/// wait for input or start a program that never ends).
pub fn command_output(cmd: &str, args: &[&str], limit: Duration) -> Result<String, String> {
    let mut child = Command::new(cmd)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not run {cmd}: {e}"))?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if start.elapsed() > limit => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{cmd} did not finish within {} s", limit.as_secs()));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => return Err(e.to_string()),
        }
    }
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    let err = String::from_utf8_lossy(&out.stderr);
    if !err.trim().is_empty() {
        text.push_str(&format!("\n(stderr)\n{}", err.trim_end()));
    }
    if !out.status.success() {
        text.push_str(&format!("\n({cmd} ended with {})", out.status));
    }
    Ok(text.trim_end().to_owned())
}

fn first_line(cmd: &str, args: &[&str]) -> String {
    command_output(cmd, args, Duration::from_secs(5)).map(|t| t.lines().next().unwrap_or("").to_owned()).unwrap_or_else(|e| e)
}

fn bundle_version(bundle: &Path) -> String {
    let plist = bundle.join("Contents/Info.plist");
    let text = std::fs::read_to_string(&plist).unwrap_or_default();
    // Our Info.plist is plain XML: <key>CFBundleShortVersionString</key><string>0.1.0</string>.
    text.split("<key>CFBundleShortVersionString</key>")
        .nth(1)
        .and_then(|rest| rest.split("<string>").nth(1))
        .and_then(|rest| rest.split("</string>").next())
        .map_or_else(|| format!("unknown (no version in {})", plist.display()), str::to_owned)
}

/// Versions of everything involved. `cli` and `glintd` come from the caller, which knows them.
pub fn versions(home: &Path, bundle: Option<&Path>, cli: &str, glintd: &str) -> Section {
    let mut s = String::new();
    match find_bundle(home, bundle) {
        Some(b) => s.push_str(&format!("app:      {} ({})\n", bundle_version(&b), b.display())),
        None => s.push_str("app:      no Glint.app in /Applications or ~/Applications\n"),
    }
    s.push_str(&format!("glintd:   {glintd}\n"));
    s.push_str(&format!("glint:    {cli}\n"));
    if cfg!(target_os = "macos") {
        s.push_str(&format!("macOS:    {} ({})\n", first_line("sw_vers", &["-productVersion"]), first_line("sw_vers", &["-buildVersion"])));
        let rosetta = first_line("sysctl", &["-n", "sysctl.proc_translated"]) == "1";
        s.push_str(&format!("arch:     {} (machine {}{})\n", std::env::consts::ARCH, first_line("uname", &["-m"]), if rosetta { ", under Rosetta" } else { "" }));
    } else {
        s.push_str(&format!("os:       {} {}\n", std::env::consts::OS, first_line("uname", &["-sr"])));
        s.push_str(&format!("arch:     {}\n", std::env::consts::ARCH));
    }
    s.push_str(&format!("user:     {} (HOME {})\n", std::env::var("USER").unwrap_or_default(), home.display()));
    ("Versions".into(), s)
}

/// Whether the files macOS would refuse to run still carry the download quarantine flag.
pub fn quarantine(home: &Path, bundle: Option<&Path>) -> Section {
    if !cfg!(target_os = "macos") {
        return ("Quarantine".into(), "not macOS".into());
    }
    let mut paths = Vec::new();
    if let Some(b) = find_bundle(home, bundle) {
        paths.extend([b.clone(), b.join("Contents/MacOS/Glint"), b.join("Contents/MacOS/glintd"), b.join("Contents/Resources/bin/glint")]);
    }
    paths.extend([home.join(".glint/bin/glintd"), home.join(".glint/bin/glint"), home.join(".glint/cache/glint")]);
    let mut s = String::new();
    for p in paths.iter().filter(|p| p.exists()) {
        let flag = Command::new("xattr").args(["-p", "com.apple.quarantine"]).arg(p).stderr(Stdio::null()).output();
        let state = match flag {
            Ok(o) if o.status.success() => {
                let value = String::from_utf8_lossy(&o.stdout).trim().to_owned();
                // The first field is hex flags; 0x40 means the user let it open (Open Anyway).
                let approved = value.split(';').next().and_then(|f| u32::from_str_radix(f, 16).ok()).is_some_and(|f| f & 0x40 != 0);
                format!("{} ({value})", if approved { "approved" } else { "QUARANTINED" })
            }
            Ok(_) => "clear".into(),
            Err(e) => format!("could not check: {e}"),
        };
        s.push_str(&format!("{state:<12} {}\n", p.display()));
    }
    if s.is_empty() {
        s.push_str("no Glint files found to check\n");
    } else {
        s.push_str("(a bundle marked approved lets the files inside run, flagged or not)\n");
    }
    ("Quarantine".into(), s)
}

/// The login shell and what it puts on PATH: the same view `glint install` gets from the window.
pub fn shell(limit: Duration) -> Section {
    let shell = login_shell();
    // Markers, because a profile may print its own things around ours.
    let script = r#"echo "@@GLINT@@"; echo "PATH=$PATH"; for c in php python3 node claude codex gemini cursor code windsurf; do printf '%-8s %s\n' "$c" "$(command -v $c 2>/dev/null || echo 'not found')"; done; echo "NODE_OPTIONS=${NODE_OPTIONS:-}"; echo "@@GLINT@@""#;
    let mut s = format!("login shell: {shell}\n");
    for flags in ["-lic", "-lc"] {
        match command_output(&shell, &[flags, script], limit) {
            Ok(text) => {
                let inside: Vec<&str> = text.split("@@GLINT@@").collect();
                if inside.len() >= 3 {
                    s.push_str(&format!("as `{shell} {flags}` sees it:\n{}\n", inside[1].trim()));
                    let noise = format!("{}{}", inside[0].trim(), inside[2..].join("").trim());
                    if !noise.trim().is_empty() {
                        s.push_str(&format!("the profile also printed:\n{}\n", tail(&noise, 20)));
                    }
                    break;
                }
                s.push_str(&format!("`{shell} {flags}` gave no answer; it printed:\n{}\n", tail(&text, 20)));
            }
            Err(e) => s.push_str(&format!("`{shell} {flags}`: {e}\n")),
        }
    }
    ("Login shell".into(), s)
}

/// The last `n` lines of `text`.
fn tail(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

fn file_section(title: &str, path: &Path, keep: impl Fn(&str) -> String) -> Section {
    let body = match std::fs::read(path) {
        Ok(b) => format!("{}\n{}", path.display(), keep(&String::from_utf8_lossy(&b))),
        Err(e) => format!("{}: {e}", path.display()),
    };
    (title.into(), body)
}

/// ~/.glint/setup.log (the last "Set up this Mac") and the end of glintd's log.
pub fn logs(home: &Path) -> Vec<Section> {
    vec![
        file_section("Last setup (setup.log)", &home.join(".glint/setup.log"), |t| {
            if t.len() <= SETUP_LOG_MAX {
                return t.to_owned();
            }
            let mut start = t.len() - SETUP_LOG_MAX;
            while !t.is_char_boundary(start) {
                start += 1;
            }
            format!("(first part cut)\n{}", &t[start..])
        }),
        file_section("glintd log (last lines)", &home.join(".glint/glintd.log"), |t| tail(t, LOG_TAIL_LINES)),
    ]
}

/// A key that probably holds a secret.
fn secret_key(key: &str) -> bool {
    let k = key.to_ascii_lowercase();
    ["secret", "token", "password", "passwd", "credential", "private", "auth", "apikey", "api_key", "cookie", "session"].iter().any(|w| k.contains(w))
        || k == "key"
        || k.ends_with("_key")
        || k.ends_with("-key")
}

/// config.toml with every value under a secret-looking key, or inside a secret-looking table,
/// replaced.
pub fn redact_toml(text: &str) -> String {
    let mut secret_table = false;
    let mut out = String::new();
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            secret_table = secret_key(t.trim_matches(|c| c == '[' || c == ']'));
            out.push_str(line);
        } else if let Some((k, _)) = t.split_once('=').filter(|_| !t.starts_with('#')) {
            let key = k.trim().trim_matches('"');
            if secret_table || secret_key(key) {
                out.push_str(&format!("{} = \"(removed)\"", k.trim_end()));
            } else {
                out.push_str(line);
            }
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    out
}

pub fn config(home: &Path) -> Section {
    let path = std::env::var_os("GLINT_CONFIG").map(PathBuf::from).unwrap_or_else(|| home.join(".glint/config.toml"));
    let mut s = file_section("config.toml (secrets removed)", &path, redact_toml);
    let vars: Vec<String> = std::env::vars()
        .filter(|(k, _)| k.starts_with("GLINT_"))
        .map(|(k, v)| if secret_key(&k) { format!("{k}=(removed)") } else { format!("{k}={v}") })
        .collect();
    if !vars.is_empty() {
        s.1.push_str(&format!("\nGLINT_ variables of this process:\n{}\n", vars.join("\n")));
    }
    s
}

/// Everything except doctor and agents, which only the CLI can check.
pub fn common(home: &Path, bundle: Option<&Path>, cli: &str, glintd: &str) -> Vec<Section> {
    let mut v = vec![versions(home, bundle, cli, glintd), quarantine(home, bundle), shell(Duration::from_secs(20))];
    v.extend(logs(home));
    v.push(config(home));
    v
}

pub fn render(sections: &[Section]) -> String {
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S %z");
    let mut out = format!(
        "Glint diagnostics, {now}\n\nThis file stays on your Mac until you send it. It has versions, setup results, logs and\nsettings (secrets removed). Read it before you send it if you like.\n"
    );
    for (title, body) in sections {
        out.push_str(&format!("\n== {title} ==\n{}\n", body.trim_end()));
    }
    out
}

/// Writes the report to ~/Downloads (made if missing) and returns its path.
pub fn save(home: &Path, dir: Option<&Path>, text: &str) -> std::io::Result<PathBuf> {
    let dir = dir.map_or_else(|| home.join("Downloads"), Path::to_path_buf);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("glint-report-{}.txt", chrono::Local::now().format("%Y%m%d-%H%M%S")));
    std::fs::write(&path, text)?;
    Ok(path)
}

/// Shows the file in Finder (or the folder in the file manager).
pub fn reveal(path: &Path) {
    let _ = if cfg!(target_os = "macos") {
        Command::new("open").arg("-R").arg(path).stdout(Stdio::null()).stderr(Stdio::null()).status()
    } else {
        Command::new("xdg-open").arg(path.parent().unwrap_or(path)).stdout(Stdio::null()).stderr(Stdio::null()).status()
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_are_removed_from_config() {
        let text = "addr = \"127.0.0.1:23600\"\nkeep_days = 7\nopenai_api_key = \"sk-123\"\ntoken=\"abc\"\n# token = \"in a comment\"\n[ai]\nmodel = \"x\"\n[secrets]\nanything = \"hidden\"\n";
        let out = redact_toml(text);
        assert!(out.contains("addr = \"127.0.0.1:23600\"") && out.contains("keep_days = 7") && out.contains("model = \"x\""));
        assert!(!out.contains("sk-123") && !out.contains("\"abc\"") && !out.contains("hidden"));
        assert!(out.contains("openai_api_key = \"(removed)\"") && out.contains("anything = \"(removed)\""));
    }

    #[test]
    fn report_has_titled_sections() {
        let text = render(&[("Versions".into(), "glint: 1\n".into()), ("Doctor".into(), "[]".into())]);
        assert!(text.starts_with("Glint diagnostics, "));
        assert!(text.contains("\n== Versions ==\nglint: 1\n") && text.contains("\n== Doctor ==\n[]\n"));
    }

    #[test]
    fn version_comes_from_the_bundle_plist() {
        let dir = std::env::temp_dir().join(format!("glint-report-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("Glint.app/Contents")).unwrap();
        std::fs::write(dir.join("Glint.app/Contents/Info.plist"), "<dict>\n  <key>CFBundleShortVersionString</key><string>9.8.7</string>\n</dict>").unwrap();
        assert_eq!(bundle_version(&dir.join("Glint.app")), "9.8.7");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
