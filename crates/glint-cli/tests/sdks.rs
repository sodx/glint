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
