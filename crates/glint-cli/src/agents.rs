//! The `agents` step of `glint install`: tell the coding agents on this machine that Glint exists.
//!
//! - Claude Code: the MCP server at user scope (`claude mcp add --scope user`, so every project
//!   sees it) and the hooks that show the agent's session in Glint (`~/.claude/settings.json`).
//! - Cursor, VS Code, Windsurf, Gemini CLI: the MCP server in each one's user-level config, over
//!   HTTP. Codex: over stdio (`glint mcp`), in a marked block of `~/.codex/config.toml`.
//!
//! Only agents that are installed are touched; existing settings are kept (keys keep their order)
//! and a file that is not plain JSON, or cannot be read, is left alone with a hint. Each change
//! first keeps a timestamped `.glint-backup-<time>` copy (the newest few are kept), and files are
//! replaced in one step (temp file + rename) so an agent never reads half a file. `glint
//! uninstall` removes exactly what was added. The server's MCP `instructions` tell a connected
//! agent when to use Glint.

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

/// A config file's text: empty when it does not exist yet, an error when it exists but cannot be
/// read (permissions, not UTF-8), so a file is never overwritten from a failed read.
fn read_config(path: &Path) -> Result<String, String> {
    match fs::read_to_string(path) {
        Ok(t) => Ok(t),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(format!("cannot read {} ({e}); left it alone", path.display())),
    }
}

/// How many timestamped backups of one file are kept.
const BACKUPS_KEPT: usize = 5;

/// Copies `text` (the file's content before a change) to `<file>.glint-backup-<time>`, and drops
/// the oldest backups beyond [`BACKUPS_KEPT`].
fn backup(a: &Args, path: &Path, text: &str) -> Result<(), String> {
    if a.dry_run || text.is_empty() {
        return Ok(());
    }
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("config");
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S%.3f");
    let file = path.with_file_name(format!("{name}.glint-backup-{stamp}"));
    fs::write(&file, text).map_err(|e| format!("backing up {} to {}: {e}", path.display(), file.display()))?;
    let prefix = format!("{name}.glint-backup-");
    if let Some(dir) = path.parent()
        && let Ok(entries) = fs::read_dir(dir)
    {
        let mut old: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with(&prefix))).collect();
        old.sort(); // the stamp sorts by time
        let excess = old.len().saturating_sub(BACKUPS_KEPT);
        for p in &old[..excess] {
            let _ = fs::remove_file(p);
        }
    }
    Ok(())
}

/// One JSON config edit: `edit` changes the parsed file and says whether anything changed.
fn edit_json(a: &Args, path: &Path, edit: impl FnOnce(&mut Map<String, Value>) -> bool) -> Result<bool, String> {
    let text = read_config(path)?;
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
    backup(a, path, &text)?;
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

/// An agent counts as installed when its settings folder exists, or (unless `a.probe` is off,
/// as in tests) when its app is in /Applications or its command is on PATH: someone who
/// installed Cursor or Claude Code but never opened it should still get Glint.
fn installed(a: &Args, folder: &Path, apps: &[&str], commands: &[&str]) -> bool {
    found(a, folder, apps, commands).is_some()
}

/// How an agent was found (its folder, app or command), or `None`.
fn found(a: &Args, folder: &Path, apps: &[&str], commands: &[&str]) -> Option<String> {
    let mut how = Vec::new();
    if folder.exists() {
        how.push(format!("folder {}", folder.display()));
    }
    if a.probe {
        for n in apps {
            for d in [PathBuf::from("/Applications"), a.home.join("Applications")] {
                let app = d.join(format!("{n}.app"));
                if app.exists() {
                    how.push(format!("app {}", app.display()));
                }
            }
        }
        let path = std::env::var_os("PATH").unwrap_or_default();
        for c in commands {
            if let Some(p) = std::env::split_paths(&path).map(|d| d.join(c)).find(|p| p.is_file()) {
                how.push(format!("command {}", p.display()));
            }
        }
    }
    (!how.is_empty()).then(|| how.join(", "))
}

/// Every agent Glint knows, and how each was found (for `glint report`).
pub fn detected(a: &Args) -> Vec<(&'static str, Option<String>)> {
    let mut v = vec![("Claude Code", found(a, &a.home.join(".claude"), &[], &["claude"]))];
    let vscode = vscode_dir(a);
    v.push(("Cursor", found(a, &a.home.join(".cursor"), &["Cursor"], &["cursor"])));
    v.push(("VS Code", found(a, &vscode, &["Visual Studio Code"], &["code"])));
    v.push(("Windsurf", found(a, &a.home.join(".codeium/windsurf"), &["Windsurf"], &["windsurf"])));
    v.push(("Gemini CLI", found(a, &a.home.join(".gemini"), &[], &["gemini"])));
    v.push(("Codex", found(a, &a.home.join(".codex"), &[], &["codex"])));
    v
}

fn vscode_dir(a: &Args) -> PathBuf {
    if cfg!(target_os = "macos") { a.home.join("Library/Application Support/Code/User") } else { a.home.join(".config/Code/User") }
}

/// A JSON-configured agent: where its user config lives and how it spells a server.
struct JsonAgent {
    name: &'static str,
    /// Installed: its settings folder exists, or its app or command does.
    present: bool,
    config: PathBuf,
    section: &'static str,
    server: Value,
}

fn json_agents(a: &Args) -> Vec<JsonAgent> {
    let h = &a.home;
    let url = mcp_url(a);
    let vscode = vscode_dir(a);
    vec![
        JsonAgent { name: "Cursor", present: installed(a, &h.join(".cursor"), &["Cursor"], &["cursor"]), config: h.join(".cursor/mcp.json"), section: "mcpServers", server: json!({ "url": url }) },
        JsonAgent { name: "VS Code", present: installed(a, &vscode, &["Visual Studio Code"], &["code"]), config: vscode.join("mcp.json"), section: "servers", server: json!({ "type": "http", "url": url }) },
        JsonAgent {
            name: "Windsurf",
            present: installed(a, &h.join(".codeium/windsurf"), &["Windsurf"], &["windsurf"]),
            config: h.join(".codeium/windsurf/mcp_config.json"),
            section: "mcpServers",
            server: json!({ "serverUrl": url }),
        },
        JsonAgent { name: "Gemini CLI", present: installed(a, &h.join(".gemini"), &[], &["gemini"]), config: h.join(".gemini/settings.json"), section: "mcpServers", server: json!({ "httpUrl": url }) },
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

/// What one `claude …` command did: success, stdout, stderr. `None`: it could not run.
type Ran = Option<(bool, String, String)>;

fn claude_run(a: &Args, args: &[&str]) -> Ran {
    claude(a, args).map(|o| (o.status.success(), String::from_utf8_lossy(&o.stdout).into_owned(), String::from_utf8_lossy(&o.stderr).into_owned()))
}

/// Registers Glint at user scope without ever leaving the user with no `glint` server: add
/// first; only when an older entry is in the way is it removed, and if the new one then cannot
/// be added, the old one is put back.
fn register_claude_mcp(url: &str, run: &dyn Fn(&[&str]) -> Ran) -> Result<String, String> {
    let added = |r: &Ran| matches!(r, Some((true, _, _)));
    let add = ["mcp", "add", "--scope", "user", "--transport", "http", NAME, url];
    let first = run(&add);
    if added(&first) {
        return Ok(format!("MCP server {NAME} → {url} for every project (user scope)"));
    }
    let Some((_, _, err)) = first else { return Err("could not run claude".into()) };
    // Most likely an older entry with this name: see what it is.
    let old = match run(&["mcp", "get", NAME]) {
        Some((true, out, _)) => out,
        _ => return Err(format!("claude mcp add failed: {}", err.trim())),
    };
    let field = |k: &str| old.lines().find_map(|l| l.trim().strip_prefix(k)).map(|v| v.trim().to_owned()).filter(|v| !v.is_empty());
    if field("URL:").as_deref() == Some(url) {
        return Ok(format!("MCP server {NAME} → {url} already registered"));
    }
    if !added(&run(&["mcp", "remove", "--scope", "user", NAME])) {
        return Err(format!("claude mcp add failed and the existing `{NAME}` entry was kept: {}", err.trim()));
    }
    match run(&add) {
        r if added(&r) => Ok(format!("MCP server {NAME} → {url} for every project (user scope, replaced the old entry)")),
        r => {
            let why = r.map(|(_, _, e)| e.trim().to_owned()).unwrap_or_else(|| "could not run claude".into());
            let restored = match (field("Type:"), field("URL:"), field("Command:")) {
                (Some(t), Some(u), _) if t == "http" || t == "sse" => added(&run(&["mcp", "add", "--scope", "user", "--transport", &t, NAME, &u])),
                (_, _, Some(cmd)) => {
                    let mut argv = vec!["mcp", "add", "--scope", "user", NAME, "--"];
                    argv.extend(cmd.split_whitespace());
                    added(&run(&argv))
                }
                _ => false,
            };
            let old_state = if restored { "the old entry was put back" } else { "the old entry could not be put back; see the backup of ~/.claude.json" };
            Err(format!("claude mcp add failed ({why}); {old_state}"))
        }
    }
}

/// A word for a POSIX shell: as is when it needs no quoting, else in single quotes.
pub fn shell_quote(s: &str) -> String {
    if !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"/._-+:@%,=~".contains(&b)) {
        s.to_owned()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

fn is_glint_hook(h: &Value) -> bool {
    h.get("command").and_then(Value::as_str).is_some_and(|c| {
        let Some(bin) = c.trim_end().strip_suffix(" hook") else { return false };
        let bin = bin.trim().trim_matches(['\'', '"']);
        bin == "glint" || bin.ends_with("/glint")
    })
}

/// Adds Glint's hook to each event unless a `glint hook` command is already there; one that is
/// there gets our current command (an older install may not have quoted a path with spaces).
fn add_hooks(root: &mut Map<String, Value>, ours: &Value) -> bool {
    let Some(ours) = ours.get("hooks").and_then(Value::as_object) else { return false };
    let hooks = root.entry("hooks").or_insert_with(|| json!({}));
    let Some(hooks) = hooks.as_object_mut() else { return false };
    let mut changed = false;
    for (event, groups) in ours {
        let command = groups.pointer("/0/hooks/0/command").cloned();
        let list = hooks.entry(event.clone()).or_insert_with(|| json!([]));
        let Some(list) = list.as_array_mut() else { continue };
        let mut present = false;
        for h in list.iter_mut().filter_map(|g| g.get_mut("hooks").and_then(Value::as_array_mut)).flatten() {
            if is_glint_hook(h) {
                present = true;
                if let (Some(c), Some(obj)) = (&command, h.as_object_mut())
                    && obj.get("command") != Some(c)
                {
                    obj.insert("command".into(), c.clone());
                    changed = true;
                }
            }
        }
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
    if !installed(a, &a.home.join(".claude"), &[], &["claude"]) {
        return; // no Claude Code here
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
        steps.push(match register_claude_mcp(&url, &|args| claude_run(a, args)) {
            Ok(m) => done("claude", m),
            Err(e) => fail("claude", e),
        });
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
        if !ag.present {
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
    if installed(a, &a.home.join(".codex"), &[], &["codex"]) {
        steps.push(match read_config(&codex) {
            Err(e) => fail("agents", format!("Codex: {e}")),
            Ok(text) if without_block(&text).contains(&format!("[mcp_servers.{NAME}]")) => {
                skip("agents", format!("Codex: {} already has an [mcp_servers.{NAME}] of its own", codex.display()))
            }
            Ok(text) => {
                let new = with_block(&text, &codex_block(a));
                if new == text {
                    skip("agents", "Codex: already set up".into())
                } else {
                    match ensure_bin(a).and_then(|()| backup(a, &codex, &text)).and_then(|()| write(a, &codex, &new)) {
                        Ok(()) => done("agents", format!("Codex: MCP server {NAME} (stdio) in {}", codex.display())),
                        Err(e) => fail("agents", format!("Codex: {e}")),
                    }
                }
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
    match read_config(&codex) {
        Err(e) => steps.push(fail("agents", format!("Codex: {e}"))),
        Ok(text) if text.contains(crate::install::MARK_BEGIN) => {
            steps.push(match backup(a, &codex, &text).and_then(|()| write(a, &codex, &without_block(&text))) {
                Ok(()) => done("agents", format!("Codex: removed from {}", codex.display())),
                Err(e) => fail("agents", format!("Codex: {e}")),
            });
        }
        Ok(_) => {}
    }
    if steps.is_empty() {
        steps.push(skip("agents", "not installed".into()));
    }
    steps
}

pub fn doctor(a: &Args) -> Vec<Step> {
    let mut found = Vec::new();
    let mut missing = Vec::new();
    if let Some(o) = installed(a, &a.home.join(".claude"), &[], &["claude"]).then(|| claude(a, &["mcp", "get", NAME])).flatten() {
        if o.status.success() { found.push("Claude Code".to_owned()) } else { missing.push("Claude Code".to_owned()) }
    }
    for ag in json_agents(a) {
        if !ag.present {
            continue;
        }
        let ok = fs::read_to_string(&ag.config)
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .is_some_and(|v| v.get(ag.section).and_then(|s| s.get(NAME)).is_some());
        if ok { found.push(ag.name.to_owned()) } else { missing.push(ag.name.to_owned()) }
    }
    if installed(a, &a.home.join(".codex"), &[], &["codex"]) {
        let ok = fs::read_to_string(a.home.join(".codex/config.toml")).is_ok_and(|t| t.contains(&format!("[mcp_servers.{NAME}]")));
        if ok { found.push("Codex".into()) } else { missing.push("Codex".into()) }
    }
    vec![match (found.is_empty(), missing.is_empty()) {
        (true, true) => skip("agents", "no coding agent found; Glint works with Claude Code, Cursor, VS Code, Windsurf, Gemini CLI and Codex".into()),
        (_, true) => done("agents", format!("Glint's MCP server is set up in {}", found.join(", "))),
        (_, false) => fail("agents", format!("not set up in {}{}: run `glint install --only agents`", missing.join(", "), if found.is_empty() { String::new() } else { format!(" (set up: {})", found.join(", ")) })),
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
            probe: false,
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
        let backups = || fs::read_dir(home.join(".cursor")).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().starts_with("mcp.json.glint-backup-")).count();
        assert_eq!(backups(), 1);
        let gemini: Value = serde_json::from_str(&fs::read_to_string(home.join(".gemini/settings.json")).unwrap()).unwrap();
        assert_eq!(gemini["mcpServers"]["glint"]["httpUrl"], "http://127.0.0.1:23600/mcp");
        assert!(!home.join(".codeium").exists(), "agents that are not installed are not touched");

        uninstall(&a);
        let cursor: Value = serde_json::from_str(&fs::read_to_string(home.join(".cursor/mcp.json")).unwrap()).unwrap();
        assert_eq!(cursor, serde_json::json!({"mcpServers":{"github":{"url":"https://x"}}}));
        assert_eq!(backups(), 2, "one backup per change");
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

    #[test]
    fn unreadable_config_is_not_overwritten() {
        let home = sandbox("unreadable");
        fs::create_dir_all(home.join(".cursor")).unwrap();
        // Not UTF-8: read_to_string fails with InvalidData, which must not count as empty.
        fs::write(home.join(".cursor/mcp.json"), [0xff, 0xfe, b'{', b'}']).unwrap();
        let steps = install(&args(&home));
        assert!(steps.iter().any(|s| s.result.as_ref().is_err_and(|e| e.contains("cannot read"))), "{:?}", steps.iter().map(|s| &s.result).collect::<Vec<_>>());
        assert_eq!(fs::read(home.join(".cursor/mcp.json")).unwrap(), [0xff, 0xfe, b'{', b'}']);
        fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn writes_replace_the_file_whole_and_follow_symlinks() {
        use std::os::unix::fs::PermissionsExt;
        let home = sandbox("atomic");
        let real = home.join("dotfiles/mcp.json");
        fs::create_dir_all(real.parent().unwrap()).unwrap();
        fs::write(&real, r#"{"a":1}"#).unwrap();
        fs::set_permissions(&real, fs::Permissions::from_mode(0o600)).unwrap();
        let link = home.join("mcp.json");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(edit_json(&args(&home), &link, |r| put_server(r, "mcpServers", json!({"url": "u"}))).unwrap());
        assert!(fs::symlink_metadata(&link).unwrap().file_type().is_symlink(), "the link stays a link");
        let v: Value = serde_json::from_str(&fs::read_to_string(&real).unwrap()).unwrap();
        assert_eq!(v["mcpServers"]["glint"]["url"], "u");
        assert_eq!(fs::metadata(&real).unwrap().permissions().mode() & 0o777, 0o600);
        assert!(!fs::read_dir(real.parent().unwrap()).unwrap().flatten().any(|e| e.file_name().to_string_lossy().contains("glint-tmp")));
        fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn backups_are_capped() {
        let home = sandbox("backups");
        let a = args(&home);
        let f = home.join("x.json");
        for i in 0..8 {
            fs::write(&f, format!("{{\"n\":{i}}}")).unwrap();
            assert!(edit_json(&a, &f, |r| { r.insert("m".into(), json!(i)); true }).unwrap());
            std::thread::sleep(std::time::Duration::from_millis(3));
        }
        let n = fs::read_dir(&home).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().starts_with("x.json.glint-backup-")).count();
        assert_eq!(n, BACKUPS_KEPT);
        fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn hook_command_quotes_a_home_with_spaces_and_upgrades_old_entries() {
        assert_eq!(shell_quote("/Users/me/.glint/bin/glint"), "/Users/me/.glint/bin/glint");
        assert_eq!(shell_quote("/Users/Jo Doe/.glint/bin/glint"), "'/Users/Jo Doe/.glint/bin/glint'");
        assert_eq!(shell_quote("/a/it's"), r"'/a/it'\''s'");
        let ours: Value = serde_json::from_str(&crate::hook::claude_code_config("/Users/Jo Doe/.glint/bin/glint")).unwrap();
        let cmd = ours["hooks"]["Stop"][0]["hooks"][0]["command"].as_str().unwrap();
        assert_eq!(cmd, "'/Users/Jo Doe/.glint/bin/glint' hook");
        // The quoted command runs in a real shell.
        let home = sandbox("quote dir");
        let bin = home.join("bin dir/glint");
        fs::create_dir_all(bin.parent().unwrap()).unwrap();
        fs::write(&bin, "#!/bin/sh\necho ran \"$1\"\n").unwrap();
        fs::set_permissions(&bin, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let line = format!("{} hook", shell_quote(&bin.display().to_string()));
        let out = Command::new("sh").args(["-c", &line]).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "ran hook");
        // An old unquoted entry is recognised as ours and replaced, not duplicated.
        let mut root: Map<String, Value> = serde_json::from_str(r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"/Users/Jo Doe/.glint/bin/glint hook"}]}]}}"#).unwrap();
        assert!(add_hooks(&mut root, &ours));
        assert_eq!(root["hooks"]["Stop"].as_array().unwrap().len(), 1);
        assert_eq!(root["hooks"]["Stop"][0]["hooks"][0]["command"], "'/Users/Jo Doe/.glint/bin/glint' hook");
        assert!(is_glint_hook(&json!({"command": "'/x y/glint' hook"})) && !is_glint_hook(&json!({"command": "rtk hook"})));
        assert!(remove_hooks(&mut root));
        fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn claude_mcp_is_never_left_without_a_glint_entry() {
        use std::cell::RefCell;
        let url = "http://127.0.0.1:23600/mcp";
        // A fake `claude`: `exists` is the current entry's URL, `add_works` whether add succeeds.
        let fake = |exists: Option<&'static str>, add_works: bool| {
            let state = RefCell::new((exists.map(str::to_owned), Vec::<String>::new()));
            move |args: &[&str]| -> Ran {
                let mut s = state.borrow_mut();
                s.1.push(args.join(" "));
                match args[1] {
                    "add" if s.0.is_some() => Some((false, String::new(), "MCP server glint already exists in user config".into())),
                    "add" if add_works || args.contains(&"http://old/mcp") => {
                        s.0 = Some(args.last().unwrap().to_string());
                        Some((true, String::new(), String::new()))
                    }
                    "add" => Some((false, String::new(), "boom".into())),
                    "get" => s.0.clone().map(|u| (true, format!("glint:\n  Scope: User config\n  Type: http\n  URL: {u}\n"), String::new())),
                    "remove" => {
                        s.0 = None;
                        Some((true, String::new(), String::new()))
                    }
                    _ => None,
                }
            }
        };
        // Fresh: one add, nothing removed.
        let f = fake(None, true);
        assert!(register_claude_mcp(url, &f).is_ok());
        // Same URL already there: left alone.
        let f = fake(Some(url), true);
        assert!(register_claude_mcp(url, &f).unwrap().contains("already registered"));
        // An old URL: replaced.
        let f = fake(Some("http://old/mcp"), true);
        assert!(register_claude_mcp(url, &f).unwrap().contains("replaced"));
        // The new add fails after the old was removed: the old one is put back.
        let calls = RefCell::new(Vec::new());
        let inner = fake(Some("http://old/mcp"), false);
        let f = |a: &[&str]| {
            calls.borrow_mut().push(a.join(" "));
            inner(a)
        };
        let e = register_claude_mcp(url, &f).unwrap_err();
        assert!(e.contains("old entry was put back"), "{e}");
        assert_eq!(calls.borrow().last().unwrap(), "mcp add --scope user --transport http glint http://old/mcp");
    }
}
