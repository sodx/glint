//! `glint docker`: glint() and uncaught-error capture for code that runs in a container or on a
//! remote machine, where `glint install` did not run and 127.0.0.1 is not this machine.
//!
//! It writes a self-contained copy of the SDKs to `~/.glint/sdk/container` (mounted read-only at
//! `/opt/.glint/sdk` in the container) and prints what to add to a compose file, a `docker run`,
//! a devcontainer.json, or an SSH session:
//!
//! - PHP: `PHP_INI_SCAN_DIR=:/opt/.glint/sdk/php/conf.d`, whose ini sets `auto_prepend_file`
//!   (the leading colon keeps the image's own ini folder);
//! - Python: `PYTHONPATH=/opt/.glint/sdk/python`, whose `sitecustomize.py` puts glint into
//!   builtins (and still runs an image's own sitecustomize);
//! - Node: `NODE_OPTIONS=--import=/opt/.glint/sdk/js/global.mjs`;
//! - `GLINT_URL` (host.docker.internal), `GLINT_PROJECT` (the git repository's name, which a
//!   container path like /var/www/html does not tell), `GLINT_PATH_MAP` (container folder to
//!   this folder, so glintd shows and opens the files here), and `host-gateway` for Linux.

use std::fs;
use std::path::{Path, PathBuf};

use crate::install::{JS, PHP, PYTHON};

/// Where the container copy of the SDKs is mounted.
pub const MOUNT: &str = "/opt/.glint/sdk";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    Php,
    Python,
    Node,
}

impl Lang {
    pub fn parse(s: &str) -> Result<Lang, String> {
        match s {
            "php" => Ok(Lang::Php),
            "python" | "py" => Ok(Lang::Python),
            "node" | "js" | "ts" => Ok(Lang::Node),
            other => Err(format!("unknown language `{other}` (php, python, node)")),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Compose,
    Run,
    Devcontainer,
    Ssh,
}

pub struct Args {
    pub format: Format,
    pub langs: Vec<Lang>,
    /// The project folder on this machine (default: the current directory).
    pub dir: PathBuf,
    /// Where that folder is in the container.
    pub app_dir: Option<String>,
    pub project: Option<String>,
    /// The SSH host, for `--ssh`.
    pub ssh_host: Option<String>,
    pub home: PathBuf,
    /// glintd's port on this machine.
    pub port: u16,
}

pub fn run(a: &Args) -> Result<(), String> {
    let kit = a.home.join(".glint/sdk/container");
    write_kit(&kit)?;
    eprintln!("glint: SDK for containers written to {}", kit.display());
    print!("{}", render(a, &kit));
    Ok(())
}

/// Writes the container copy of the SDKs. Paths inside it are container paths (`MOUNT`).
pub fn write_kit(kit: &Path) -> Result<(), String> {
    let put = |rel: &str, body: &str| -> Result<(), String> {
        let p = kit.join(rel);
        if let Some(d) = p.parent() {
            fs::create_dir_all(d).map_err(|e| format!("creating {}: {e}", d.display()))?;
        }
        fs::write(&p, body).map_err(|e| format!("writing {}: {e}", p.display()))
    };
    for (rel, body) in PHP.iter().chain(PYTHON).chain(JS) {
        put(rel, body)?;
    }
    put("php/autoload.php", include_str!("../../../sdks/global/autoload.php"))?;
    put("php/conf.d/99-glint.ini", &format!("; Written by `glint docker`.\nauto_prepend_file=\"{MOUNT}/php/autoload.php\"\n"))?;
    put("python/glint_builtin.py", &include_str!("../../../sdks/global/glint_builtin.py").replace("__GLINT_SDK__", &format!("{MOUNT}/python")))?;
    put("python/sitecustomize.py", include_str!("../../../sdks/global/sitecustomize.py"))?;
    put("js/global.mjs", include_str!("../../../sdks/global/global.mjs"))?;
    Ok(())
}

/// The languages a folder uses, from its manifest files; all three when none is recognised.
pub fn detect(dir: &Path) -> Vec<Lang> {
    let has = |names: &[&str]| names.iter().any(|n| dir.join(n).exists());
    let ext = |e: &str| fs::read_dir(dir).into_iter().flatten().flatten().any(|f| f.path().extension().is_some_and(|x| x == e));
    let mut out = vec![];
    if has(&["composer.json", "wp-config.php", "artisan"]) || ext("php") {
        out.push(Lang::Php);
    }
    if has(&["pyproject.toml", "requirements.txt", "setup.py", "Pipfile", "manage.py"]) || ext("py") {
        out.push(Lang::Python);
    }
    if has(&["package.json"]) || ext("js") || ext("mjs") || ext("ts") {
        out.push(Lang::Node);
    }
    if out.is_empty() { vec![Lang::Php, Lang::Python, Lang::Node] } else { out }
}

/// The project name glintd would give this folder on this machine: its git repository.
pub fn project_name(dir: &Path) -> String {
    let d = dir.to_string_lossy();
    glint_core::project::resolve(&d)
        .map(|r| r.project)
        .or_else(|| glint_core::project::folder_name(&d))
        .unwrap_or_else(|| "default".to_owned())
}

fn env(a: &Args, url: &str, map: Option<String>, sdk: &str) -> Vec<(&'static str, String)> {
    let mut v = vec![("GLINT_URL", url.to_owned())];
    v.push(("GLINT_PROJECT", a.project.clone().unwrap_or_else(|| project_name(&a.dir))));
    if let Some(m) = map {
        v.push(("GLINT_PATH_MAP", m));
    }
    for l in &a.langs {
        match l {
            // Over SSH the ini names the remote home, so it lives outside the synced folder.
            Lang::Php if a.format == Format::Ssh => v.push(("PHP_INI_SCAN_DIR", ":$HOME/.glint/sdk/remote-php".to_owned())),
            Lang::Php => v.push(("PHP_INI_SCAN_DIR", format!(":{sdk}/php/conf.d"))),
            Lang::Python => v.push(("PYTHONPATH", format!("{sdk}/python"))),
            Lang::Node => v.push(("NODE_OPTIONS", format!("--import={sdk}/js/global.mjs"))),
        }
    }
    v
}

fn default_app_dir(a: &Args) -> String {
    if a.format == Format::Devcontainer {
        let name = a.dir.file_name().map_or("app".into(), |n| n.to_string_lossy().into_owned());
        return format!("/workspaces/{name}");
    }
    if a.langs == [Lang::Php] { "/var/www/html".into() } else { "/app".into() }
}

pub fn render(a: &Args, kit: &Path) -> String {
    let host_dir = a.dir.display().to_string();
    let app_dir = a.app_dir.clone().unwrap_or_else(|| default_app_dir(a));
    let docker_url = format!("http://host.docker.internal:{}", a.port);
    let kit = kit.display().to_string();
    let mut o = String::new();
    match a.format {
        Format::Compose => {
            o.push_str(&format!(
                "# Glint in a container: add these keys to the service in docker-compose.yml (or\n\
                 # compose.override.yml), then recreate it: docker compose up -d --force-recreate.\n\
                 # GLINT_PATH_MAP assumes this folder is mounted at {app_dir}; change the left side\n\
                 # if your compose file mounts it elsewhere (or pass --app-dir).\n\
                 # Setting NODE_OPTIONS or PYTHONPATH here replaces the image's own; join them if it has any.\n"
            ));
            o.push_str(&format!("    volumes:\n      - {kit}:{MOUNT}:ro\n    environment:\n"));
            for (k, v) in env(a, &docker_url, Some(format!("{app_dir}:{host_dir}")), MOUNT) {
                o.push_str(&format!("      {k}: {}\n", yaml(&v)));
            }
            o.push_str("    extra_hosts:\n      - host.docker.internal:host-gateway   # Linux; Docker Desktop has it already\n");
        }
        Format::Run => {
            o.push_str(&format!("# Glint in a container: add these flags to `docker run`. GLINT_PATH_MAP assumes this\n# folder is mounted at {app_dir} (pass --app-dir if not).\n"));
            o.push_str(&format!("-v {}:{MOUNT}:ro \\\n", sh(&kit)));
            for (k, v) in env(a, &docker_url, Some(format!("{app_dir}:{host_dir}")), MOUNT) {
                o.push_str(&format!("-e {} \\\n", sh(&format!("{k}={v}"))));
            }
            o.push_str("--add-host host.docker.internal:host-gateway\n");
        }
        Format::Devcontainer => {
            o.push_str("// Glint in a dev container: merge into .devcontainer/devcontainer.json, then rebuild the container.\n");
            o.push_str(&format!(
                "\"mounts\": [\"source=${{localEnv:HOME}}/.glint/sdk/container,target={MOUNT},type=bind,readonly\"],\n\
                 \"runArgs\": [\"--add-host=host.docker.internal:host-gateway\"],\n\
                 \"containerEnv\": {{\n"
            ));
            let map = if a.app_dir.is_some() { format!("{app_dir}:${{localWorkspaceFolder}}") } else { "${containerWorkspaceFolder}:${localWorkspaceFolder}".to_owned() };
            let vars = env(a, &docker_url, Some(map), MOUNT);
            for (i, (k, v)) in vars.iter().enumerate() {
                let comma = if i + 1 < vars.len() { "," } else { "" };
                o.push_str(&format!("  {}: {}{comma}\n", json(k), json(v)));
            }
            o.push_str("}\n");
        }
        Format::Ssh => {
            let host = a.ssh_host.clone().unwrap_or_else(|| "HOST".into());
            let remote_sdk = "$HOME/.glint/sdk/container";
            let remote_dir = a.app_dir.clone().unwrap_or_else(|| "/path/on/remote".into());
            o.push_str(&format!(
                "# Glint on a remote machine over SSH. glintd stays here; the tunnel makes 127.0.0.1:{port}\n\
                 # on the remote machine reach it.\n\
                 #\n\
                 # 1. Copy the SDK to the remote machine (the rsync again after `glint docker` updates it):\n\
                 ssh {host} 'mkdir -p ~/.glint/sdk/container ~/.glint/sdk/remote-php && printf \"auto_prepend_file=\\\"%s\\\"\\n\" \"$HOME/.glint/sdk/container/php/autoload.php\" > ~/.glint/sdk/remote-php/99-glint.ini'\n\
                 rsync -a {kit}/ {host}:.glint/sdk/container/\n\
                 #\n\
                 # 2. Connect with the tunnel (or put `RemoteForward {port} 127.0.0.1:{port}` under the host in\n\
                 #    ~/.ssh/config; VS Code Remote SSH uses that file too):\n\
                 ssh -R {port}:127.0.0.1:{port} {host}\n\
                 #\n\
                 # 3. On the remote machine, in the shell profile:\n",
                port = a.port,
                kit = sh(&kit),
            ));
            let map = format!("{remote_dir}:{host_dir}");
            for (k, v) in env(a, &format!("http://127.0.0.1:{}", a.port), Some(map), remote_sdk) {
                o.push_str(&format!("export {k}=\"{}\"\n", v.replace('"', "\\\"")));
            }
            o.push_str("# GLINT_PATH_MAP: the remote folder, then where the same code is on this machine. Drop it\n# if the code lives only on the remote machine.\n");
        }
    }
    if cfg!(target_os = "linux") && matches!(a.format, Format::Compose | Format::Run | Format::Devcontainer) {
        o.push_str(&format!(
            "# On Linux, containers reach this machine through the Docker bridge, not 127.0.0.1. Let glintd\n\
             # take events there: remote_addr = \"172.17.0.1:{}\" in ~/.glint/config.toml, then restart it.\n",
            a.port
        ));
    }
    o
}

fn yaml(v: &str) -> String {
    if v.starts_with([':', '-', '@', '*', '&', '!', '{', '[', '\'', '"', '%', '|', '>', '#', '`']) || v.contains(": ") || v.contains(" #") || v.is_empty() {
        format!("\"{}\"", v.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        v.to_owned()
    }
}

fn json(v: &str) -> String {
    serde_json::Value::String(v.to_owned()).to_string()
}

fn sh(v: &str) -> String {
    if v.chars().all(|c| c.is_ascii_alphanumeric() || "/._-=:,+@".contains(c)) { v.to_owned() } else { format!("'{}'", v.replace('\'', "'\\''")) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(format: Format, langs: Vec<Lang>) -> Args {
        Args { format, langs, dir: "/Users/me/shop".into(), app_dir: None, project: Some("shop".into()), ssh_host: None, home: "/Users/me".into(), port: 23600 }
    }

    #[test]
    fn compose_snippet() {
        let out = render(&args(Format::Compose, vec![Lang::Php]), Path::new("/Users/me/.glint/sdk/container"));
        for want in [
            "      - /Users/me/.glint/sdk/container:/opt/.glint/sdk:ro\n",
            "      GLINT_URL: http://host.docker.internal:23600\n",
            "      GLINT_PROJECT: shop\n",
            "      GLINT_PATH_MAP: /var/www/html:/Users/me/shop\n",
            "      PHP_INI_SCAN_DIR: \":/opt/.glint/sdk/php/conf.d\"\n",
            "      - host.docker.internal:host-gateway",
        ] {
            assert!(out.contains(want), "{want:?} not in\n{out}");
        }
        assert!(!out.contains("NODE_OPTIONS:"));
    }

    #[test]
    fn run_and_devcontainer_snippets() {
        let all = vec![Lang::Php, Lang::Python, Lang::Node];
        let run = render(&args(Format::Run, all.clone()), Path::new("/Users/me/.glint/sdk/container"));
        assert!(run.contains("-e GLINT_PATH_MAP=/app:/Users/me/shop \\\n"), "{run}");
        assert!(run.contains("-e PYTHONPATH=/opt/.glint/sdk/python \\\n") && run.contains("-e NODE_OPTIONS=--import=/opt/.glint/sdk/js/global.mjs \\\n"), "{run}");
        let dc = render(&args(Format::Devcontainer, all), Path::new("/x"));
        assert!(dc.contains(r#""GLINT_PATH_MAP": "${containerWorkspaceFolder}:${localWorkspaceFolder}","#), "{dc}");
        assert!(dc.contains("target=/opt/.glint/sdk,type=bind,readonly"));
        // The containerEnv body, braces added, is JSON.
        let body = dc.split("\"containerEnv\": ").nth(1).unwrap();
        let body = body.split("\n#").next().unwrap();
        serde_json::from_str::<serde_json::Value>(body).unwrap();
    }

    #[test]
    fn ssh_snippet_uses_the_tunnel() {
        let mut a = args(Format::Ssh, vec![Lang::Python]);
        a.ssh_host = Some("devbox".into());
        a.app_dir = Some("/home/me/shop".into());
        let out = render(&a, Path::new("/Users/me/.glint/sdk/container"));
        assert!(out.contains("ssh -R 23600:127.0.0.1:23600 devbox\n"), "{out}");
        assert!(out.contains("export GLINT_URL=\"http://127.0.0.1:23600\"\n"));
        assert!(out.contains("export PYTHONPATH=\"$HOME/.glint/sdk/container/python\"\n"));
        assert!(out.contains("export GLINT_PATH_MAP=\"/home/me/shop:/Users/me/shop\"\n"));
    }

    #[test]
    fn kit_is_complete() {
        let d = std::env::temp_dir().join(format!("glint-kit-{}", std::process::id()));
        write_kit(&d).unwrap();
        for f in ["php/autoload.php", "php/src/Transport.php", "php/conf.d/99-glint.ini", "python/glint/__init__.py", "python/glint_builtin.py", "python/sitecustomize.py", "js/src/index.js", "js/global.mjs"] {
            assert!(d.join(f).is_file(), "{f}");
        }
        assert!(fs::read_to_string(d.join("python/glint_builtin.py")).unwrap().contains("\"/opt/.glint/sdk/python\""));
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn detects_languages() {
        let d = std::env::temp_dir().join(format!("glint-detect-{}", std::process::id()));
        fs::create_dir_all(&d).unwrap();
        assert_eq!(detect(&d).len(), 3);
        fs::write(d.join("composer.json"), "{}").unwrap();
        assert_eq!(detect(&d), vec![Lang::Php]);
        fs::write(d.join("package.json"), "{}").unwrap();
        assert_eq!(detect(&d), vec![Lang::Php, Lang::Node]);
        fs::remove_dir_all(&d).unwrap();
    }
}
