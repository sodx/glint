//! `glint report`: a diagnostics file in ~/Downloads for whoever helps with this Mac. The parts
//! glintd can also collect live in `glint_server::report`; doctor and the agents found are added
//! here. Nothing is sent anywhere.

use std::path::PathBuf;
use std::time::Duration;

use glint_server::report;

use crate::install;

pub struct Args {
    /// The Glint.app to describe (the window passes its own; default: /Applications/Glint.app).
    pub app: Option<PathBuf>,
    /// Folder to write to instead of ~/Downloads.
    pub out: Option<PathBuf>,
    pub reveal: bool,
    pub json: bool,
    pub url: String,
}

pub fn run(a: Args) -> Result<(), String> {
    let home = std::env::var_os("HOME").map(PathBuf::from).ok_or("HOME is not set")?;
    let setup = install::Args {
        targets: install::Target::ALL.to_vec(),
        dry_run: false,
        home: home.clone(),
        php_ini_dir: None,
        shell_profile: None,
        url: a.url.clone(),
        probe: true,
    };
    let info = ureq::get(&format!("{}/v1/info", a.url.trim_end_matches('/'))).timeout(Duration::from_secs(2)).call();
    let glintd = match info {
        Ok(r) => {
            let v: serde_json::Value = r.into_json().unwrap_or_default();
            format!("{} at {}", v["version"].as_str().unwrap_or("?"), a.url)
        }
        Err(e) => format!("not reachable at {} ({e})", a.url),
    };
    let exe = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_default();
    let cli = format!("{} ({exe})", env!("CARGO_PKG_VERSION"));
    let bundle = a.app.clone().or_else(report::own_bundle);
    let mut sections = report::common(&home, bundle.as_deref(), &cli, &glintd);

    let steps = install::doctor(&setup, &a.url);
    let doctor = serde_json::to_string_pretty(&install::to_json(&steps)).unwrap_or_default();
    let agents = crate::agents::detected(&setup)
        .into_iter()
        .map(|(name, how)| format!("{name:<12} {}", how.unwrap_or_else(|| "not found".into())))
        .collect::<Vec<_>>()
        .join("\n");
    // After versions and quarantine, before the shell and logs: what a helper reads first.
    sections.insert(2, ("Doctor (glint doctor --json)".into(), doctor));
    sections.insert(3, ("Coding agents found".into(), agents));

    let path = report::save(&home, a.out.as_deref(), &report::render(&sections)).map_err(|e| format!("writing the report: {e}"))?;
    if a.json {
        println!("GLINT_JSON:{}", serde_json::json!({ "path": path.display().to_string() }));
    }
    println!("Wrote {}", path.display());
    println!("Nothing was sent. Read it, then send the file to whoever helps you with Glint.");
    if a.reveal {
        report::reveal(&path);
    }
    Ok(())
}
