//! The `agents` step of `glint install`: tell the coding agents on this machine that Glint exists.
//!
//! - Claude Code: the MCP server at user scope (`claude mcp add --scope user`, so every project
//!   sees it) and the hooks that show the agent's session in Glint (`~/.claude/settings.json`).
//! - Cursor, VS Code, Windsurf, Gemini CLI: the MCP server in each one's user-level config, over
//!   HTTP. Codex: over stdio (`glint mcp`), in a marked block of `~/.codex/config.toml`.
//!
//! Only agents that are installed are touched; existing settings are kept (keys keep their order)
//! and a file that is not plain JSON is left alone with a hint. Each edited file gets a one-time
//! `.glint-backup` copy first. `glint uninstall` removes exactly what was added. The server's MCP
//! `instructions` tell a connected agent when to use Glint.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Map, Value, json};

use crate::install::{Args, Step, done, fail, skip, with_block, without_block, write};

const NAME: &str = "glint";

fn mcp_url(a: &Args) -> String {
    format!("{}/mcp", a.url.trim_end_matches('/'))
}

/// The `glint` binary hooks and stdio clients run: ~/.glint/bin/glint (`glint install` keeps it
/// current), since an agent's PATH rarely includes ~/.glint/bin.
fn glint_bin(a: &Args) -> PathBuf {
    a.home.join(".glint/bin/glint")
}

fn ensure_bin(a: &Args) -> Result<(), String> {
    let bin = glint_bin(a);
    if a.dry_run || bin.exists() {
        return Ok(());
    }
    let here = std::env::current_exe().map_err(|e| format!("cannot find this executable: {e}"))?;
    fs::create_dir_all(bin.parent().unwrap_or(Path::new("."))).map_err(|e| e.to_string())?;
    crate::install::replace_file(&here, &bin).map_err(|e| format!("copying glint to {}: {e}", bin.display()))
}

/// One JSON config edit: `edit` changes the parsed file and says whether anything changed.
fn edit_json(a: &Args, path: &Path, edit: impl FnOnce(&mut Map<String, Value>) -> bool) -> Result<bool, String> {
    let text = fs::read_to_string(path).unwrap_or_default();
    let mut root: Map<String, Value> = if text.trim().is_empty() {
        Map::new()
    } else {
        match serde_json::from_str::<Value>(&text) {
            Ok(Value::Object(m)) => m,
            _ => return Err(format!("{} is not plain JSON (comments?); add Glint there by hand", path.display())),
        }
    };
    if !edit(&mut root) {
        return Ok(false);
    }
    if !a.dry_run && !text.is_empty() {
        let backup = path.with_extension(format!("{}.glint-backup", path.extension().and_then(|e| e.to_str()).unwrap_or("")));
        if !backup.exists() {
            let _ = fs::write(&backup, &text);
        }
    }
    let mut out = serde_json::to_string_pretty(&Value::Object(root)).map_err(|e| e.to_string())?;
    out.push('\n');
    write(a, path, &out)?;
    Ok(true)
}

/// `root[section][glint] = server`.
fn put_server(root: &mut Map<String, Value>, section: &str, server: Value) -> bool {
    let servers = root.entry(section).or_insert_with(|| json!({}));
    let Some(servers) = servers.as_object_mut() else { return false };
    if servers.get(NAME) == Some(&server) {
        return false;
    }
    servers.insert(NAME.into(), server);
    true
}

fn drop_server(root: &mut Map<String, Value>, section: &str) -> bool {
    let removed = root.get_mut(section).and_then(Value::as_object_mut).is_some_and(|s| s.remove(NAME).is_some());
    if removed && root.get(section).and_then(Value::as_object).is_some_and(Map::is_empty) {
        root.remove(section);
    }
    removed
}

/// A JSON-configured agent: where its user config lives and how it spells a server.
struct JsonAgent {
    name: &'static str,
    /// The folder whose existence means the agent is installed.
    marker: PathBuf,
    config: PathBuf,
    section: &'static str,
    server: Value,
}

fn json_agents(a: &Args) -> Vec<JsonAgent> {
    let h = &a.home;
    let url = mcp_url(a);
    let vscode = if cfg!(target_os = "macos") { h.join("Library/Application Support/Code/User") } else { h.join(".config/Code/User") };
    vec![
        JsonAgent { name: "Cursor", marker: h.join(".cursor"), config: h.join(".cursor/mcp.json"), section: "mcpServers", server: json!({ "url": url }) },
        JsonAgent { name: "VS Code", marker: vscode.clone(), config: vscode.join("mcp.json"), section: "servers", server: json!({ "type": "http", "url": url }) },
        JsonAgent {
            name: "Windsurf",
            marker: h.join(".codeium/windsurf"),
            config: h.join(".codeium/windsurf/mcp_config.json"),
            section: "mcpServers",
            server: json!({ "serverUrl": url }),
        },
        JsonAgent { name: "Gemini CLI", marker: h.join(".gemini"), config: h.join(".gemini/settings.json"), section: "mcpServers", server: json!({ "httpUrl": url }) },
    ]
}

fn codex_block(a: &Args) -> String {
    format!(
        "{}\n[mcp_servers.{NAME}]\ncommand = {:?}\nargs = [\"mcp\"]\n{}\n",
        crate::install::MARK_BEGIN,
        glint_bin(a).display().to_string(),
        crate::install::MARK_END
    )
}

// ------------------------------------------------------------------ Claude Code

fn claude(a: &Args, args: &[&str]) -> Option<std::process::Output> {
    Command::new("claude").args(args).env("HOME", &a.home).output().ok()
}

fn is_glint_hook(h: &Value) -> bool {
    h.get("command").and_then(Value::as_str).is_some_and(|c| c.ends_with("glint hook") || c.contains("/glint hook"))
}

/// Adds Glint's hook to each event unless a `glint hook` command is already there.
fn add_hooks(root: &mut Map<String, Value>, ours: &Value) -> bool {
    let Some(ours) = ours.get("hooks").and_then(Value::as_object) else { return false };
    let hooks = root.entry("hooks").or_insert_with(|| json!({}));
    let Some(hooks) = hooks.as_object_mut() else { return false };
    let mut changed = false;
    for (event, groups) in ours {
        let list = hooks.entry(event.clone()).or_insert_with(|| json!([]));
        let Some(list) = list.as_array_mut() else { continue };
        let present = list.iter().any(|g| g.get("hooks").and_then(Value::as_array).is_some_and(|hs| hs.iter().any(is_glint_hook)));
        if !present {
            list.extend(groups.as_array().cloned().unwrap_or_default());
            changed = true;
        }
    }
    changed
}

fn remove_hooks(root: &mut Map<String, Value>) -> bool {
    let Some(hooks) = root.get_mut("hooks").and_then(Value::as_object_mut) else { return false };
    let mut changed = false;
    for list in hooks.values_mut() {
        let Some(groups) = list.as_array_mut() else { continue };
        for g in groups.iter_mut() {
            if let Some(hs) = g.get_mut("hooks").and_then(Value::as_array_mut) {
                let before = hs.len();
                hs.retain(|h| !is_glint_hook(h));
                changed |= hs.len() != before;
            }
        }
        groups.retain(|g| g.get("hooks").and_then(Value::as_array).is_none_or(|hs| !hs.is_empty()));
    }
    hooks.retain(|_, v| v.as_array().is_none_or(|l| !l.is_empty()));
    if hooks.is_empty() {
        root.remove("hooks");
    }
    changed
}

fn install_claude(a: &Args, steps: &mut Vec<Step>) {
    let settings = a.home.join(".claude/settings.json");
    if !a.home.join(".claude").exists() {
        return; // Claude Code has never run here
    }
    let has_cli = claude(a, &["--version"]).is_some_and(|o| o.status.success());
    // MCP at user scope, through Claude Code's own CLI (it rewrites ~/.claude.json often, so
    // editing that file directly could lose either side's change).
    let url = mcp_url(a);
    if !has_cli {
        steps.push(skip("claude", format!("`claude` is not on PATH; run: claude mcp add --scope user --transport http {NAME} {url}")));
    } else if a.dry_run {
        steps.push(done("claude", format!("would register MCP server {NAME} → {url} (user scope)")));
    } else {
        let _ = claude(a, &["mcp", "remove", "--scope", "user", NAME]);
        match claude(a, &["mcp", "add", "--scope", "user", "--transport", "http", NAME, &url]) {
            Some(o) if o.status.success() => steps.push(done("claude", format!("MCP server {NAME} → {url} for every project (user scope)"))),
            Some(o) => steps.push(fail("claude", format!("claude mcp add failed: {}", String::from_utf8_lossy(&o.stderr).trim()))),
            None => steps.push(fail("claude", "could not run claude".into())),
        }
    }
    // Hooks: the agent's prompts, tool calls and results show up in Glint next to the program's.
    let ours: Value = serde_json::from_str(&crate::hook::claude_code_config(&glint_bin(a).display().to_string())).unwrap_or_default();
    match ensure_bin(a).and_then(|()| edit_json(a, &settings, |root| add_hooks(root, &ours))) {
        Ok(true) => steps.push(done("claude", format!("hooks in {} (the session appears in Glint)", settings.display()))),
        Ok(false) => steps.push(skip("claude", "hooks already there".into())),
        Err(e) => steps.push(fail("claude", e)),
    }
}

// ------------------------------------------------------------------ install / uninstall / doctor

pub fn install(a: &Args) -> Vec<Step> {
    let mut steps = Vec::new();
    install_claude(a, &mut steps);
    for ag in json_agents(a) {
        if !ag.marker.exists() {
            continue;
        }
        let server = ag.server.clone();
        steps.push(match edit_json(a, &ag.config, |root| put_server(root, ag.section, server)) {
            Ok(true) => done("agents", format!("{}: MCP server {NAME} in {}", ag.name, ag.config.display())),
            Ok(false) => skip("agents", format!("{}: already set up", ag.name)),
            Err(e) => fail("agents", format!("{}: {e}", ag.name)),
        });
    }
    let codex = a.home.join(".codex/config.toml");
    if a.home.join(".codex").exists() {
        let text = fs::read_to_string(&codex).unwrap_or_default();
        let foreign = without_block(&text).contains(&format!("[mcp_servers.{NAME}]"));
        steps.push(if foreign {
            skip("agents", format!("Codex: {} already has an [mcp_servers.{NAME}] of its own", codex.display()))
        } else {
            match ensure_bin(a).and_then(|()| write(a, &codex, &with_block(&text, &codex_block(a)))) {
                Ok(()) => done("agents", format!("Codex: MCP server {NAME} (stdio) in {}", codex.display())),
                Err(e) => fail("agents", format!("Codex: {e}")),
            }
        });
    }
    if steps.is_empty() {
        steps.push(skip("agents", "no coding agent found (Claude Code, Cursor, VS Code, Windsurf, Gemini CLI, Codex)".into()));
    } else {
        steps.push(skip("agents", "new agent sessions see Glint; running ones need a restart".into()));
    }
    steps
}

pub fn uninstall(a: &Args) -> Vec<Step> {
    let mut steps = Vec::new();
    let has_claude = a.home.join(".claude").exists();
    if has_claude && !a.dry_run && claude(a, &["mcp", "remove", "--scope", "user", NAME]).is_some_and(|o| o.status.success()) {
        steps.push(done("claude", format!("removed MCP server {NAME}")));
    }
    let settings = a.home.join(".claude/settings.json");
    if settings.exists() {
        match edit_json(a, &settings, remove_hooks) {
            Ok(true) => steps.push(done("claude", format!("removed Glint's hooks from {}", settings.display()))),
            Ok(false) => {}
            Err(e) => steps.push(fail("claude", e)),
        }
    }
    for ag in json_agents(a) {
        if !ag.config.exists() {
            continue;
        }
        match edit_json(a, &ag.config, |root| drop_server(root, ag.section)) {
            Ok(true) => steps.push(done("agents", format!("{}: removed from {}", ag.name, ag.config.display()))),
            Ok(false) => {}
            Err(e) => steps.push(fail("agents", format!("{}: {e}", ag.name))),
        }
    }
    let codex = a.home.join(".codex/config.toml");
    if let Ok(text) = fs::read_to_string(&codex) {
        if text.contains(crate::install::MARK_BEGIN) {
            steps.push(match write(a, &codex, &without_block(&text)) {
                Ok(()) => done("agents", format!("Codex: removed from {}", codex.display())),
                Err(e) => fail("agents", format!("Codex: {e}")),
            });
        }
    }
    if steps.is_empty() {
        steps.push(skip("agents", "not installed".into()));
    }
    steps
}

pub fn doctor(a: &Args) -> Vec<Step> {
    let mut found = Vec::new();
    let mut missing = Vec::new();
    if let Some(o) = a.home.join(".claude").exists().then(|| claude(a, &["mcp", "get", NAME])).flatten() {
        if o.status.success() { found.push("Claude Code".to_owned()) } else { missing.push("Claude Code".to_owned()) }
    }
    for ag in json_agents(a) {
        if !ag.marker.exists() {
            continue;
        }
        let ok = fs::read_to_string(&ag.config)
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .is_some_and(|v| v.get(ag.section).and_then(|s| s.get(NAME)).is_some());
        if ok { found.push(ag.name.to_owned()) } else { missing.push(ag.name.to_owned()) }
    }
    if a.home.join(".codex").exists() {
        let ok = fs::read_to_string(a.home.join(".codex/config.toml")).is_ok_and(|t| t.contains(&format!("[mcp_servers.{NAME}]")));
        if ok { found.push("Codex".into()) } else { missing.push("Codex".into()) }
    }
    vec![match (found.is_empty(), missing.is_empty()) {
        (true, true) => skip("agents", "no coding agent found".into()),
        (_, true) => done("agents", format!("Glint's MCP server is set up in {}", found.join(", "))),
        (_, false) => fail("agents", format!("not set up in {} — run `glint install --only agents`{}", missing.join(", "), if found.is_empty() { String::new() } else { format!(" (set up: {})", found.join(", ")) })),
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(home: &Path) -> Args {
        Args {
            targets: vec![],
            dry_run: false,
            home: home.to_owned(),
            php_ini_dir: None,
            shell_profile: None,
            url: "http://127.0.0.1:23600".into(),
        }
    }

    fn sandbox(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("glint-agents-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn hooks_merge_keeps_user_hooks_and_order_and_uninstalls_cleanly() {
        let mut root: Map<String, Value> = serde_json::from_str(
            r#"{"model":"opus","hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"rtk hook"}]}]},"theme":"dark"}"#,
        )
        .unwrap();
        let ours: Value = serde_json::from_str(&crate::hook::claude_code_config("/h/.glint/bin/glint")).unwrap();
        assert!(add_hooks(&mut root, &ours));
        assert!(!add_hooks(&mut root, &ours), "second install changes nothing");
        let pre = root["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(pre.len(), 2);
        assert_eq!(pre[0]["hooks"][0]["command"], "rtk hook");
        assert_eq!(root.keys().collect::<Vec<_>>(), ["model", "hooks", "theme"]);
        assert!(remove_hooks(&mut root));
        assert_eq!(
            serde_json::to_string(&root).unwrap(),
            r#"{"model":"opus","hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"rtk hook"}]}]},"theme":"dark"}"#
        );
    }

    #[test]
    fn json_agents_are_added_next_to_other_servers_and_removed() {
        let home = sandbox("json");
        fs::create_dir_all(home.join(".cursor")).unwrap();
        fs::write(home.join(".cursor/mcp.json"), r#"{"mcpServers":{"github":{"url":"https://x"}}}"#).unwrap();
        fs::create_dir_all(home.join(".gemini")).unwrap(); // installed, no settings yet
        let a = args(&home);
        let steps = install(&a);
        assert!(steps.iter().all(|s| s.result.is_ok()));
        let cursor: Value = serde_json::from_str(&fs::read_to_string(home.join(".cursor/mcp.json")).unwrap()).unwrap();
        assert_eq!(cursor["mcpServers"]["github"]["url"], "https://x");
        assert_eq!(cursor["mcpServers"]["glint"]["url"], "http://127.0.0.1:23600/mcp");
        assert!(home.join(".cursor/mcp.json.glint-backup").exists());
        let gemini: Value = serde_json::from_str(&fs::read_to_string(home.join(".gemini/settings.json")).unwrap()).unwrap();
        assert_eq!(gemini["mcpServers"]["glint"]["httpUrl"], "http://127.0.0.1:23600/mcp");
        assert!(!home.join(".codeium").exists(), "agents that are not installed are not touched");

        uninstall(&a);
        let cursor: Value = serde_json::from_str(&fs::read_to_string(home.join(".cursor/mcp.json")).unwrap()).unwrap();
        assert_eq!(cursor, serde_json::json!({"mcpServers":{"github":{"url":"https://x"}}}));
        assert_eq!(fs::read_to_string(home.join(".gemini/settings.json")).unwrap().trim(), "{}");
        fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn json_with_comments_is_left_alone() {
        let home = sandbox("jsonc");
        fs::create_dir_all(home.join(".cursor")).unwrap();
        let original = "{\n  // mine\n  \"mcpServers\": {}\n}\n";
        fs::write(home.join(".cursor/mcp.json"), original).unwrap();
        let steps = install(&args(&home));
        assert!(steps.iter().any(|s| s.result.as_ref().is_err_and(|e| e.contains("not plain JSON"))));
        assert_eq!(fs::read_to_string(home.join(".cursor/mcp.json")).unwrap(), original);
        fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn codex_gets_a_marked_stdio_block() {
        let home = sandbox("codex");
        fs::create_dir_all(home.join(".codex")).unwrap();
        fs::write(home.join(".codex/config.toml"), "model = \"o4\"\n").unwrap();
        fs::create_dir_all(home.join(".glint/bin")).unwrap();
        fs::write(home.join(".glint/bin/glint"), "").unwrap();
        let a = args(&home);
        install(&a);
        install(&a);
        let t = fs::read_to_string(home.join(".codex/config.toml")).unwrap();
        assert_eq!(t.matches("[mcp_servers.glint]").count(), 1);
        assert!(t.starts_with("model = \"o4\"\n") && t.contains("args = [\"mcp\"]"));
        uninstall(&a);
        assert_eq!(fs::read_to_string(home.join(".codex/config.toml")).unwrap(), "model = \"o4\"\n");
        fs::remove_dir_all(&home).unwrap();
    }
}
