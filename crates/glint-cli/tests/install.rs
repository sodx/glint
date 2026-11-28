//! `glint install` / `glint uninstall` in a sandbox HOME: what install writes is recorded in
//! ~/.glint/installed.json, uninstall removes exactly that (even when PHP's ini folder is not the
//! one on PATH any more), and a deleted ~/.glint never stops PHP.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Sandbox {
    home: PathBuf,
}

impl Sandbox {
    fn new(name: &str) -> Sandbox {
        let home = std::env::temp_dir().join(format!("glint-install-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join("php-conf.d")).unwrap();
        Sandbox { home: home.canonicalize().unwrap() }
    }

    fn cmd(&self, program: impl AsRef<std::ffi::OsStr>) -> Command {
        let mut c = Command::new(program);
        c.env("HOME", &self.home)
            .env("GLINT_SKIP_SERVICE_START", "1")
            .env("PHP_INI_SCAN_DIR", self.home.join("php-conf.d"))
            .env("PYTHONUSERBASE", self.home.join("pyuser"))
            .env("SHELL", "/bin/zsh")
            .env("GLINT_URL", "http://127.0.0.1:9")
            .env_remove("ZDOTDIR")
            .env_remove("NODE_OPTIONS");
        c
    }

    fn glint(&self, args: &[&str]) -> Output {
        let out = self.cmd(env!("CARGO_BIN_EXE_glint")).args(args).output().unwrap();
        assert!(out.status.success(), "glint {args:?}:\n{}\n{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        out
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.home.join(rel)
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

fn has(cmd: &str) -> bool {
    Command::new(cmd).arg("--version").output().is_ok_and(|o| o.status.success())
}

fn manifest(s: &Sandbox) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(s.path(".glint/installed.json")).unwrap()).unwrap()
}

fn recorded(m: &serde_json::Value, target: &str) -> Vec<String> {
    m["written"].as_array().unwrap().iter().filter(|w| w["target"] == target).map(|w| w["path"].as_str().unwrap().to_owned()).collect()
}

#[test]
fn install_records_what_it_wrote_and_uninstall_removes_exactly_that() {
    let s = Sandbox::new("manifest");
    let ini_dir = s.path("php-conf.d");
    let profile = s.path(".zshrc");
    std::fs::write(&profile, "alias ll='ls -l'\n").unwrap();
    let ini_arg = ini_dir.display().to_string();
    let profile_arg = profile.display().to_string();
    s.glint(&["install", "--only", "php,python,node", "--php-ini-dir", &ini_arg, "--shell-profile", &profile_arg]);

    let m = manifest(&s);
    assert!(recorded(&m, "node").contains(&profile_arg), "{m}");
    assert_eq!(m["written"].as_array().unwrap().iter().find(|w| w["target"] == "node").unwrap()["how"], "block");
    let profile_text = std::fs::read_to_string(&profile).unwrap();
    assert!(profile_text.contains(".glint/bin") && profile_text.starts_with("alias ll"));
    if has("php") {
        let php = recorded(&m, "php");
        assert!(php.contains(&ini_dir.join("99-glint.ini").display().to_string()), "{m}");
        assert!(php.contains(&ini_dir.join("glint-prepend.php").display().to_string()), "{m}");
        let ini = std::fs::read_to_string(ini_dir.join("99-glint.ini")).unwrap();
        assert!(ini.contains("glint-prepend.php"), "the ini must point at the stub, not into ~/.glint: {ini}");
    }
    if has("python3") {
        assert_eq!(recorded(&m, "python").len(), 1, "{m}");
    }

    // Uninstall without --php-ini-dir and with PHP looking at another folder now: the recorded
    // files still go.
    let out = s.cmd(env!("CARGO_BIN_EXE_glint")).env("PHP_INI_SCAN_DIR", s.path("elsewhere")).args(["uninstall", "--only", "php,python,node", "--shell-profile", &profile_arg]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));
    assert!(!ini_dir.join("99-glint.ini").exists());
    assert!(!ini_dir.join("glint-prepend.php").exists());
    assert_eq!(std::fs::read_to_string(&profile).unwrap(), "alias ll='ls -l'\n");
    for p in recorded(&m, "python") {
        assert!(!Path::new(&p).exists(), "{p} is still there");
    }
    assert!(!s.path(".glint/installed.json").exists(), "nothing left to record");
}

#[test]
fn a_deleted_glint_folder_never_stops_php() {
    if !has("php") {
        return eprintln!("skipping: php not installed");
    }
    let s = Sandbox::new("php");
    let ini_dir = s.path("php-conf.d");
    let ini_arg = ini_dir.display().to_string();
    s.glint(&["install", "--only", "php", "--php-ini-dir", &ini_arg]);
    let run = |code: &str| {
        let mut c = s.cmd("php");
        c.stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
        let mut child = c.spawn().unwrap();
        use std::io::Write as _;
        child.stdin.take().unwrap().write_all(code.as_bytes()).unwrap();
        child.wait_with_output().unwrap()
    };
    let out = run("<?php echo function_exists('glint') ? 'yes' : 'no';");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "yes", "{}", String::from_utf8_lossy(&out.stderr));
    std::fs::remove_dir_all(s.path(".glint")).unwrap();
    let out = run("<?php echo 'still runs';");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "still runs");
}

#[test]
fn zsh_profile_follows_zdotdir_inside_home() {
    let s = Sandbox::new("zdotdir");
    let zdot = s.path(".config/zsh");
    std::fs::create_dir_all(&zdot).unwrap();
    s.cmd(env!("CARGO_BIN_EXE_glint")).env("ZDOTDIR", &zdot).args(["install", "--only", "node"]).output().unwrap();
    assert!(std::fs::read_to_string(zdot.join(".zshrc")).unwrap().contains("# >>> glint >>>"));
    assert!(!s.path(".zshrc").exists());
    // A ZDOTDIR outside HOME (the real one, while HOME is a sandbox) is never written to.
    let outside = std::env::temp_dir().join(format!("glint-zdot-outside-{}", std::process::id()));
    std::fs::create_dir_all(&outside).unwrap();
    s.cmd(env!("CARGO_BIN_EXE_glint")).env("ZDOTDIR", &outside).args(["install", "--only", "node"]).output().unwrap();
    assert!(!outside.join(".zshrc").exists());
    let _ = std::fs::remove_dir_all(&outside);
}
