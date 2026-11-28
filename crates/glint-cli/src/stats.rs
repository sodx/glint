//! `glint stats`: what coding agents did with Glint, from glintd's records (the same numbers as
//! the window's Agents view).

use std::time::Duration;

use glint_core::agents::{AgentStats, render};

pub struct Args {
    pub days: u32,
    pub project: Option<String>,
    pub within: u64,
    pub json: bool,
}

pub fn run(base: &str, a: &Args) -> Result<(), String> {
    let mut url = format!("{}/v1/agents/stats?days={}&within_min={}", base.trim_end_matches('/'), a.days, a.within);
    if let Some(p) = &a.project {
        url.push_str("&project=");
        url.push_str(&enc(p));
    }
    let http = crate::client::builder().timeout_connect(Duration::from_secs(1)).timeout(Duration::from_secs(30)).build();
    let res = http.get(&url).call().map_err(|e| format!("could not reach glintd at {base}: {e}"))?;
    let body: serde_json::Value = res.into_json().map_err(|e| e.to_string())?;
    if a.json {
        println!("{}", serde_json::to_string_pretty(&body).unwrap_or_default());
        return Ok(());
    }
    let st: AgentStats = serde_json::from_value(body).map_err(|e| format!("unexpected answer from glintd: {e}"))?;
    let scope = a.project.as_deref().map(|p| format!(" in {p}")).unwrap_or_default();
    println!("Coding agents and Glint, last {} day{}{scope}\n", a.days, if a.days == 1 { "" } else { "s" });
    print!("{}", render(&st));
    Ok(())
}

fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect()
}
