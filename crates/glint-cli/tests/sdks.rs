//! Every first-party SDK must pass the conformance suite. An SDK is skipped (with a note) when
//! its runtime is not installed, so `cargo test` works on any machine; CI installs them all.

use std::path::Path;
use std::process::Command;

fn conformance(runtime: &str, program: &str) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    if !root.join(program).exists() {
        eprintln!("skipping {program}: not present");
        return;
    }
    if Command::new(runtime).arg("--version").output().is_err() {
        eprintln!("skipping {program}: `{runtime}` not installed");
        return;
    }
    let out = Command::new(env!("CARGO_BIN_EXE_glint"))
        .current_dir(&root)
        .args(["conformance", "--", runtime, program])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{program} failed conformance:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn js_sdk_conforms() {
    conformance("node", "sdks/js/conformance.mjs");
}

#[test]
fn python_sdk_conforms() {
    conformance("python3", "sdks/python/conformance.py");
}

#[test]
fn php_sdk_conforms() {
    conformance("php", "sdks/php/conformance.php");
}

fn has(runtime: &str) -> bool {
    Command::new(runtime).arg("--version").output().is_ok_and(|o| o.status.success())
}

/// A glintd that is up but stalled: it accepts connections and never answers.
fn stalled_glintd() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        let mut held = Vec::new();
        for s in listener.incoming().flatten() {
            held.push(s);
        }
    });
    url
}

fn timed(cmd: &mut Command) -> std::time::Duration {
    let t = std::time::Instant::now();
    let out = cmd.env_remove("NODE_OPTIONS").output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    t.elapsed()
}

#[test]
fn a_stalled_glintd_does_not_hold_up_a_node_program() {
    if !has("node") {
        return eprintln!("skipping: node not installed");
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let sdk = root.join("sdks/js/src/index.js").canonicalize().unwrap();
    let script = format!("const {{ default: glint }} = await import({:?}); glint(1); glint.log('x');", format!("file://{}", sdk.display()));
    let took = timed(Command::new("node").args(["--input-type=module", "-e", &script]).env("GLINT_URL", stalled_glintd()));
    // It was over 2 s (the whole request timeout) before.
    assert!(took < std::time::Duration::from_millis(1200), "node took {took:?} to exit");
}

#[test]
fn a_stalled_glintd_costs_php_one_short_wait_shared_by_later_processes() {
    if !has("php") {
        return eprintln!("skipping: php not installed");
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let src = root.join("sdks/php/src").canonicalize().unwrap();
    let script = format!(
        "spl_autoload_register(function ($c) {{ require {0:?} . '/' . substr($c, 6) . '.php'; }}); require {0:?} . '/helpers.php'; glint(1); glint(2);",
        src.display().to_string()
    );
    let url = stalled_glintd();
    let first = timed(Command::new("php").args(["-n", "-r", &script]).env("GLINT_URL", &url));
    assert!(first < std::time::Duration::from_millis(1500), "first php took {first:?}");
    // The next process (php-fpm's next request) knows from the marker file and does not wait.
    let second = timed(Command::new("php").args(["-n", "-r", &script]).env("GLINT_URL", &url));
    assert!(second < std::time::Duration::from_millis(400), "second php took {second:?}");
}

#[test]
fn wordpress_reports_only_on_development_sites() {
    if !has("php") {
        return eprintln!("skipping: php not installed");
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for mode in ["prod", "debug", "local"] {
        let out = Command::new("php").arg("-n").arg(root.join("sdks/php/tests/wordpress_env.php")).arg(mode).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "ok", "{mode}: {}", String::from_utf8_lossy(&out.stderr));
    }
}
