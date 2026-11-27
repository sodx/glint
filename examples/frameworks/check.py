"""Runs each framework example against a sandbox glintd and checks what arrives.

    python3 examples/frameworks/check.py                 # all of them
    python3 examples/frameworks/check.py django express  # some
    python3 examples/frameworks/check.py --keep /tmp/fw  # keep (and reuse) the apps and installs

A framework whose toolchain is missing (php and composer, python3, node and npm) is skipped with
the reason. Dependencies are installed into a temporary folder (a venv, node_modules, a fresh
Laravel app), never globally; the folder is deleted at the end unless --keep is given. Nothing
touches your own glintd, ~/.glint or shell profile: the SDKs are loaded from this checkout the way
`glint install` would load them, and glintd runs with its own HOME, database and port.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
from typing import Any, Callable, Dict, List, Optional, Tuple

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))
SDKS = os.path.join(ROOT, "sdks")


# ---------------------------------------------------------------- plumbing


def free_port(preferred: int) -> int:
    for port in (preferred, 0):
        with socket.socket() as s:
            try:
                s.bind(("127.0.0.1", port))
                return s.getsockname()[1]
            except OSError:
                continue
    raise SystemExit("no free port")


def get(url: str, timeout: float = 30) -> Tuple[int, str]:
    try:
        with urllib.request.urlopen(url, timeout=timeout) as r:
            return r.status, r.read().decode("utf-8", "replace")
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode("utf-8", "replace")
    except Exception as e:  # noqa: BLE001
        return 0, str(e)


def wait_http(url: str, seconds: float, proc: Optional[subprocess.Popen] = None) -> bool:
    deadline = time.time() + seconds
    while time.time() < deadline:
        if proc is not None and proc.poll() is not None:
            return False
        status, _ = get(url, timeout=2)
        if status:
            return True
        time.sleep(0.3)
    return False


def run(cmd: List[str], cwd: str, env: Optional[Dict[str, str]] = None, log: Optional[str] = None, timeout: float = 900) -> bool:
    out = open(log, "a") if log else subprocess.DEVNULL
    try:
        return subprocess.run(cmd, cwd=cwd, env=env, stdout=out, stderr=subprocess.STDOUT, timeout=timeout).returncode == 0
    except (OSError, subprocess.TimeoutExpired):
        return False
    finally:
        if log:
            out.close()


class Checker:
    def __init__(self, work: str, glint: str) -> None:
        self.work = work
        self.glint = glint
        self.procs: List[subprocess.Popen] = []
        self.inst = self._layout()

    def _layout(self) -> Dict[str, str]:
        """The SDKs laid out the way `glint install` writes them to ~/.glint/sdk, from this checkout."""
        inst = os.path.join(self.work, "sdk")
        for d in ("php", "js", "python-shim"):
            os.makedirs(os.path.join(inst, d), exist_ok=True)
        for link, target in (("php/src", "php/src"), ("js/src", "js/src")):
            path = os.path.join(inst, link)
            if not os.path.islink(path):
                os.symlink(os.path.join(SDKS, target), path)
        shutil.copy(os.path.join(SDKS, "global/autoload.php"), os.path.join(inst, "php/autoload.php"))
        shutil.copy(os.path.join(SDKS, "global/global.mjs"), os.path.join(inst, "js/global.mjs"))
        with open(os.path.join(SDKS, "global/glint_builtin.py")) as f:
            shim = f.read().replace("__GLINT_SDK__", os.path.join(SDKS, "python"))
        with open(os.path.join(inst, "python-shim/glint_builtin.py"), "w") as f:
            f.write(shim)
        ini = os.path.join(inst, "php-ini")
        os.makedirs(ini, exist_ok=True)
        with open(os.path.join(ini, "zz-glint-check.ini"), "w") as f:
            f.write(f'auto_prepend_file="{os.path.join(inst, "php/autoload.php")}"\n')
        return {"root": inst, "php_ini": ini, "node": os.path.join(inst, "js/global.mjs"), "shim": os.path.join(inst, "python-shim")}

    def env(self, project: str, **extra: str) -> Dict[str, str]:
        env = dict(os.environ)
        env.update(GLINT_URL=self.glint, GLINT_PROJECT=project, GLINT_ENABLED="1", PYTHONNOUSERSITE="1", NEXT_TELEMETRY_DISABLED="1")
        env["NODE_OPTIONS"] = f"--import=file://{self.inst['node']}"
        # Appends to PHP's own scan directory, so this prepend wins over an installed one.
        env["PHP_INI_SCAN_DIR"] = os.pathsep + self.inst["php_ini"]
        env.pop("APP_ENV", None)
        env.update(extra)
        return env

    def start(self, cmd: List[str], cwd: str, env: Dict[str, str], log: str) -> subprocess.Popen:
        p = subprocess.Popen(cmd, cwd=cwd, env=env, stdout=open(log, "a"), stderr=subprocess.STDOUT, start_new_session=True)
        self.procs.append(p)
        return p

    def stop(self, p: subprocess.Popen) -> None:
        try:
            os.killpg(p.pid, 15)
            p.wait(5)
        except Exception:
            try:
                os.killpg(p.pid, 9)
            except Exception:
                pass

    def stop_all(self) -> None:
        for p in self.procs:
            if p.poll() is None:
                self.stop(p)

    def events(self, project: str) -> List[Dict[str, Any]]:
        status, body = get(f"{self.glint}/v1/events?limit=1000")
        if status != 200:
            return []
        return [e for e in json.loads(body)["events"] if e.get("project") == project]

    def wait(self, project: str, ok: Callable[[List[Dict[str, Any]]], bool], seconds: float = 15) -> List[Dict[str, Any]]:
        deadline = time.time() + seconds
        evs: List[Dict[str, Any]] = []
        while time.time() < deadline:
            evs = self.events(project)
            if ok(evs):
                break
            time.sleep(0.4)
        return evs


# ---------------------------------------------------------------- what every integration must show


def roots(evs: List[Dict[str, Any]], label: str) -> List[Dict[str, Any]]:
    return [e for e in evs if e["kind"] == "core.measure" and (e.get("meta") or {}).get("label") == label]


def common(evs: List[Dict[str, Any]], label: str, app_file: str, orders: str, boom: str, sql: bool = True) -> List[Tuple[bool, str]]:
    """Request runs, SQL nested under them with the app line, the exception, the failed call."""
    out: List[Tuple[bool, str]] = []
    rs = roots(evs, label)
    order_root = next((r for r in rs if orders in (r["payload"].get("route") or r["payload"].get("url") or "") and r["payload"].get("status") == 200), None)
    out.append((order_root is not None, f"request summary for {orders} with status 200 ({label})"))
    if order_root is not None:
        kids = [e for e in evs if e.get("parent") == order_root["id"] and e["run"] == order_root["run"]]
        q = [e for e in kids if e["kind"] == "sql.query"]
        out.append((not sql or (len(q) >= 2 and order_root["payload"].get("queries", 0) >= 2), f"{len(q)} queries nest under it, summary counts {order_root['payload'].get('queries')}"))
        if sql:
            at = (q[0].get("origin") or {}).get("file", "") if q else ""
            out.append((at.endswith(app_file), f"SQL points at the app line ({os.path.basename(at) or 'none'}:{(q[0].get('origin') or {}).get('line') if q else ''})"))
        other = {e["run"] for e in rs if e is not order_root}
        out.append((order_root["run"] not in other, "each request is a run of its own"))
    boom_root = next((r for r in rs if boom in (r["payload"].get("url") or "")), None)
    out.append((boom_root is not None and boom_root["payload"].get("status") == 500 and (boom_root.get("meta") or {}).get("level") == "error", f"{boom} summary has status 500 as an error"))
    exc = [e for e in evs if e["kind"] == "core.exception" and boom in json.dumps((e["payload"].get("context") or {}))]
    out.append((len(exc) == 1, f"the exception arrives once with its request ({len(exc)} found)"))
    if exc and boom_root is not None:
        out.append((exc[0].get("parent") == boom_root["id"], "the exception nests under its request"))
        out.append(((exc[0].get("origin") or {}).get("file", "").endswith(app_file), f"the exception points at the app ({os.path.basename((exc[0].get('origin') or {}).get('file', ''))}:{(exc[0].get('origin') or {}).get('line')})"))
    http = [e for e in evs if e["kind"] == "http.request" and "127.0.0.1:59999" in e["payload"].get("url", "")]
    out.append((bool(http) and http[0]["meta"].get("level") == "error", "the failed outgoing call arrives as an error"))
    return out


# ---------------------------------------------------------------- frameworks

Result = Tuple[str, List[Tuple[bool, str]], str]  # status, checks, note


def python_env(c: Checker, packages: List[str]) -> Optional[str]:
    if not shutil.which("python3"):
        return None
    venv = os.path.join(c.work, "venv")
    py = os.path.join(venv, "bin", "python")
    log = os.path.join(c.work, "pip.log")
    if not os.path.exists(py) and not run([sys.executable, "-m", "venv", venv], c.work, log=log):
        return None
    if not run([py, "-m", "pip", "install", "-q", *packages], c.work, log=log):
        return None
    site = subprocess.run([py, "-c", "import sysconfig; print(sysconfig.get_paths()['purelib'])"], capture_output=True, text=True).stdout.strip()
    with open(os.path.join(site, "glint.pth"), "w") as f:
        f.write(f"{c.inst['shim']}\nimport glint_builtin\n")
    return py


def check_python(c: Checker, name: str, label: str, packages: List[str], cmd: Callable[[str, int], List[str]]) -> Result:
    py = python_env(c, packages)
    if py is None:
        return "skip", [], f"python3 missing or `pip install {' '.join(packages)}` failed (see pip.log)"
    port = free_port({"django": 8781, "flask": 8782, "fastapi": 8783}[name])
    app = os.path.join(HERE, name, "app.py")
    project = f"check-{name}"
    p = c.start(cmd(py, port), os.path.join(HERE, name), c.env(project), os.path.join(c.work, f"{name}.log"))
    base = f"http://127.0.0.1:{port}"
    if not wait_http(base + "/orders/ada", 120, p):
        return "fail", [(False, f"{name} did not start (see {name}.log)")], ""
    for path in ("/orders/ada", "/orders/ada", "/boom", "/upstream"):
        get(base + path)
    evs = c.wait(project, lambda e: len(roots(e, label)) >= 4 and any(x["kind"] == "http.request" for x in e), 30)
    c.stop(p)
    checks = common(evs, label, app, "/orders/", "/boom")
    logs = [e for e in evs if e["kind"] == "core.log" and "has" in e["payload"].get("message", "")]
    checks.append((bool(logs) and (logs[0].get("origin") or {}).get("file") == app, "logging records arrive with their line"))
    return "pass" if all(ok for ok, _ in checks) else "fail", checks, ""


def django(c: Checker) -> Result:
    return check_python(c, "django", "Django", ["django", "requests"], lambda py, port: [py, "app.py", "runserver", str(port), "--noreload"])


def flask(c: Checker) -> Result:
    return check_python(c, "flask", "Flask", ["flask", "sqlalchemy", "requests"], lambda py, port: [py, "app.py", str(port)])


def fastapi(c: Checker) -> Result:
    return check_python(c, "fastapi", "FastAPI", ["fastapi", "uvicorn", "sqlalchemy", "httpx"], lambda py, port: [py, "app.py", str(port)])


def npm_app(c: Checker, name: str, extra: List[str] = ()) -> Optional[str]:
    if not (shutil.which("node") and shutil.which("npm")):
        return None
    dest = os.path.join(c.work, name)
    if not os.path.isdir(dest):
        shutil.copytree(os.path.join(HERE, name), dest)
    if not os.path.isdir(os.path.join(dest, "node_modules")) or extra:
        if not run(["npm", "install", "--no-audit", "--no-fund", "--loglevel=error", *extra], dest, env=dict(os.environ, NODE_OPTIONS=""), log=os.path.join(c.work, "npm.log")):
            return None
    return dest


def postgres(c: Checker) -> Optional[str]:
    """A throwaway PostgreSQL cluster in the work folder, if PostgreSQL is installed."""
    server = shutil.which("postgres")
    bindir = os.path.dirname(os.path.realpath(server)) if server else ""
    initdb = shutil.which("initdb") or os.path.join(bindir, "initdb")
    pg_ctl = shutil.which("pg_ctl") or os.path.join(bindir, "pg_ctl")
    if not (os.access(initdb, os.X_OK) and os.access(pg_ctl, os.X_OK)):
        return None
    c.pg_ctl = pg_ctl  # type: ignore[attr-defined]
    data = os.path.join(c.work, "pg")
    log = os.path.join(c.work, "postgres.log")
    if not os.path.isdir(data) and not run([initdb, "-D", data, "-A", "trust", "-U", "postgres", "--locale=C", "-E", "UTF8"], c.work, env=dict(os.environ, LC_ALL="C", LANG="C"), log=log):
        return None
    port = free_port(5491)
    if not run([pg_ctl, "-D", data, "-l", log, "-w", "-o", f"-p {port} -c listen_addresses=127.0.0.1 -c unix_socket_directories=''", "start"], c.work, env=dict(os.environ, LC_ALL="C", LANG="C"), log=log, timeout=60):
        return None
    c.pg_data = data  # type: ignore[attr-defined]
    return f"postgres://postgres@127.0.0.1:{port}/postgres"


def express(c: Checker) -> Result:
    dest = npm_app(c, "express")
    if dest is None:
        return "skip", [], "node or npm missing" if not (shutil.which("node") and shutil.which("npm")) else "`npm install` failed (see npm.log)"
    pg = postgres(c)
    port = free_port(8785)
    project = "check-express"
    p = c.start(["node", "app.mjs", str(port)], dest, c.env(project, **({"PG_URL": pg} if pg else {})), os.path.join(c.work, "express.log"))
    base = f"http://127.0.0.1:{port}"
    if not wait_http(base + "/orders/ada", 60, p):
        return "fail", [(False, "express did not start (see express.log)")], ""
    paths = ["/orders/ada", "/orders/ada", "/boom", "/upstream"] + (["/pg/ada"] if pg else [])
    for path in paths:
        get(base + path)
    evs = c.wait(project, lambda e: len(roots(e, "Express")) >= len(paths) + 1)  # + the start probe
    c.stop(p)
    if pg:
        run([c.pg_ctl, "-D", c.pg_data, "-m", "fast", "stop"], c.work, log=os.path.join(c.work, "postgres.log"))  # type: ignore[attr-defined]
    checks = common(evs, "Express", os.path.join(dest, "app.mjs"), "/orders/", "/boom")
    up = next((r for r in roots(evs, "Express") if "/upstream" in r["payload"].get("url", "")), None)
    checks.append((up is not None and any(e["kind"] == "core.exception" and e.get("parent") == up["id"] for e in evs), "an error passed to next() is reported too"))
    if pg:
        q = [e for e in evs if e["kind"] == "sql.query" and e["payload"].get("conn") == "pg"]
        at = (q[0].get("origin") or {}) if q else {}
        checks.append((bool(q) and q[0]["payload"].get("rows") == 1 and at.get("file", "").endswith("app.mjs"), f"pg queries arrive with rows and the app line ({len(q)} found, {os.path.basename(at.get('file', '')) or 'no file'}:{at.get('line')})"))
    note = "" if pg else "pg not checked: PostgreSQL (initdb, pg_ctl) not found"
    return "pass" if all(ok for ok, _ in checks) else "fail", checks, note


def nextjs(c: Checker) -> Result:
    dest = npm_app(c, "nextjs")
    if dest is None:
        return "skip", [], "node or npm missing" if not (shutil.which("node") and shutil.which("npm")) else "`npm install` failed (see npm.log)"
    port = free_port(8786)
    project = "check-nextjs"
    p = c.start([os.path.join(dest, "node_modules/.bin/next"), "dev", "-p", str(port)], dest, c.env(project), os.path.join(c.work, "nextjs.log"))
    base = f"http://127.0.0.1:{port}"
    if not wait_http(base + "/api/orders", 120, p):
        return "fail", [(False, "next dev did not start (see nextjs.log)")], ""
    for path in ("/api/orders?customer=ada", "/api/orders?customer=ada", "/api/boom", "/api/boom"):
        get(base + path, timeout=120)
    evs = c.wait(project, lambda e: len(roots(e, "Next.js")) >= 4, 30)
    c.stop(p)
    checks = common(evs, "Next.js", "route.js", "/api/orders", "/api/boom")
    # Next.js compiles route files; the SQL line points into .next/ unless source maps apply.
    checks = [(ok or "points at the app" in d, d + ("" if ok else " (compiled code, not counted)")) for ok, d in checks if "failed outgoing" not in d]
    return "pass" if all(ok for ok, _ in checks) else "fail", checks, "failed fetch() is covered by the express check"


def vite(c: Checker) -> Result:
    dest = npm_app(c, "vite")
    if dest is None:
        return "skip", [], "node or npm missing" if not (shutil.which("node") and shutil.which("npm")) else "`npm install` failed (see npm.log)"
    cfg = os.path.join(dest, "vite.config.js")
    with open(cfg) as f:
        text = f.read().replace("../../../sdks/js/src/vite.js", os.path.join(SDKS, "js/src/vite.js"))
    with open(cfg, "w") as f:
        f.write(text)
    port = free_port(8787)
    env = c.env("check-vite", GLINT_PROJECT="")
    env.pop("GLINT_PROJECT")
    vite_bin = os.path.join(dest, "node_modules/.bin/vite")
    p = c.start([vite_bin, "--port", str(port), "--strictPort"], dest, env, os.path.join(c.work, "vite.log"))
    ok = wait_http(f"http://127.0.0.1:{port}/", 30, p)
    _, html = get(f"http://127.0.0.1:{port}/")
    c.stop(p)
    tag = f'src="{c.glint}/v1/browser.js"'
    checks = [(ok and tag in html and 'data-project="vite"' in html, "the dev page gets the browser script with the project name")]
    built = run([vite_bin, "build"], dest, env=env, log=os.path.join(c.work, "vite.log"))
    out = ""
    try:
        with open(os.path.join(dest, "dist/index.html")) as f:
            out = f.read()
    except OSError:
        pass
    checks.append((built and out != "" and "browser.js" not in out, "a production build has no browser script"))
    return "pass" if all(x for x, _ in checks) else "fail", checks, ""


def laravel(c: Checker) -> Result:
    if not (shutil.which("php") and shutil.which("composer")):
        return "skip", [], "php or composer missing"
    dest = os.path.join(c.work, "laravel")
    log = os.path.join(c.work, "laravel.log")
    plain = dict(os.environ, PHP_INI_SCAN_DIR="")
    plain.pop("PHP_INI_SCAN_DIR")
    if not os.path.isfile(os.path.join(dest, "artisan")):
        # Composer's cache stays in the work folder: when downloads fall back to git clones
        # (a rejected GitHub token does that), the clones take gigabytes.
        composer = dict(plain, COMPOSER_CACHE_DIR=os.path.join(c.work, "composer-cache"))
        if not run(["composer", "create-project", "--no-interaction", "--prefer-dist", "laravel/laravel", dest], c.work, env=composer, log=log, timeout=1800):
            return "skip", [], "`composer create-project laravel/laravel` failed (see laravel.log)"
    shutil.copytree(os.path.join(HERE, "laravel"), dest, dirs_exist_ok=True)
    db = os.path.join(dest, "database/database.sqlite")
    open(db, "a").close()
    env_file = os.path.join(dest, ".env")
    lines = [ln for ln in open(env_file).read().splitlines() if not ln.startswith(("DB_", "QUEUE_CONNECTION=", "APP_ENV="))]
    lines += ["APP_ENV=local", "DB_CONNECTION=sqlite", f"DB_DATABASE={db}", "QUEUE_CONNECTION=database"]
    with open(env_file, "w") as f:
        f.write("\n".join(lines) + "\n")
    php = ["php"]
    if not any(m.endswith("_create_jobs_table.php") for m in os.listdir(os.path.join(dest, "database/migrations"))):
        run(php + ["artisan", "queue:table"], dest, env=plain, log=log)
    if not run(php + ["artisan", "migrate", "--force"], dest, env=plain, log=log):
        return "fail", [(False, "artisan migrate failed (see laravel.log)")], ""
    project = "check-laravel"
    env = c.env(project)
    port = free_port(8784)
    p = c.start(php + ["artisan", "serve", f"--port={port}"], dest, env, log)
    base = f"http://127.0.0.1:{port}"
    if not wait_http(base + "/orders/ada", 90, p):
        return "fail", [(False, "artisan serve did not start (see laravel.log)")], ""
    for path in ("/orders/ada", "/orders/ada", "/boom", "/upstream", "/ship/ada"):
        get(base + path)
    run(php + ["artisan", "queue:work", "--once", "--stop-when-empty", "--tries=1"], dest, env=env, log=log, timeout=60)
    evs = c.wait(project, lambda e: len(roots(e, "Laravel")) >= 5 and roots(e, "Laravel job"))
    c.stop(p)
    controller = os.path.join(dest, "app/Http/Controllers/OrderController.php")
    checks = common(evs, "Laravel", controller, "/orders/", "/boom")
    logs = [e for e in evs if e["kind"] == "core.log" and "has" in e["payload"].get("message", "")]
    checks.append((bool(logs) and (logs[0].get("origin") or {}).get("file") == controller, "Log:: records arrive with their line"))
    order_root = next((r for r in roots(evs, "Laravel") if r["payload"].get("route") == "/orders/{customer}"), None)
    checks.append((order_root is not None and "OrderController@index" in (order_root["payload"].get("action") or ""), "the summary names the route and the controller action"))
    job = next(iter(roots(evs, "Laravel job")), None)
    checks.append((job is not None and (job.get("meta") or {}).get("level") == "error" and job["payload"].get("status") == "failed", "a failed queue job is a run with a failed summary"))
    if job is not None:
        kids = [e for e in evs if e.get("parent") == job["id"]]
        checks.append((any(e["kind"] == "sql.query" for e in kids) and any(e["kind"] == "core.exception" for e in kids), "the job's SQL and exception nest under it"))
        checks.append((all(job["run"] != r["run"] for r in roots(evs, "Laravel")), "the job runs apart from the requests"))
    return "pass" if all(ok for ok, _ in checks) else "fail", checks, ""


FRAMEWORKS: Dict[str, Callable[[Checker], Result]] = {
    "laravel": laravel,
    "django": django,
    "flask": flask,
    "fastapi": fastapi,
    "express": express,
    "nextjs": nextjs,
    "vite": vite,
}


def glintd_binary(arg: Optional[str]) -> Optional[str]:
    target = os.environ.get("CARGO_TARGET_DIR") or os.path.join(ROOT, "target")
    for path in (arg, os.environ.get("GLINTD"), os.path.join(target, "release/glintd"), os.path.join(target, "debug/glintd"), os.path.join(ROOT, "target/release/glintd"), os.path.join(ROOT, "target/debug/glintd"), shutil.which("glintd")):
        if path and os.path.isfile(path) and os.access(path, os.X_OK):
            return path
    return None


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("frameworks", nargs="*", help=f"some of: {', '.join(FRAMEWORKS)} (default: all)")
    ap.add_argument("--keep", metavar="DIR", help="work folder to keep and reuse (apps, venv, node_modules)")
    ap.add_argument("--glintd", metavar="PATH", help="glintd binary (default: target/release/glintd, then $GLINTD)")
    args = ap.parse_args()
    names = args.frameworks or list(FRAMEWORKS)
    unknown = [n for n in names if n not in FRAMEWORKS]
    if unknown:
        ap.error(f"unknown framework {', '.join(unknown)}; choose from {', '.join(FRAMEWORKS)}")

    binary = glintd_binary(args.glintd)
    if binary is None:
        print("no glintd binary: run `cargo build --release -p glintd` or pass --glintd", file=sys.stderr)
        return 2
    work = os.path.abspath(args.keep) if args.keep else tempfile.mkdtemp(prefix="glint-frameworks-")
    os.makedirs(work, exist_ok=True)
    port = free_port(23779)
    glint = f"http://127.0.0.1:{port}"
    home = os.path.join(work, "home")
    shutil.rmtree(home, ignore_errors=True)  # a fresh store each time, also with --keep
    os.makedirs(home)
    daemon_env = dict(os.environ, HOME=home, GLINT_DB=os.path.join(home, "glint.db"), GLINT_ADDR=f"127.0.0.1:{port}", GLINT_RAY_ADDR="off", GLINT_OTLP_ADDR="off")
    c = Checker(work, glint)
    daemon = c.start([binary], work, daemon_env, os.path.join(work, "glintd.log"))
    failed = False
    try:
        if not wait_http(glint + "/v1/info", 15, daemon):
            print(f"glintd did not start (see {work}/glintd.log)", file=sys.stderr)
            return 2
        print(f"sandbox glintd {glint}, work folder {work}\n")
        for name in names:
            tty = sys.stdout.isatty()
            if tty:
                print(f"{name}: running…", end="", flush=True)
            try:
                status, checks, note = FRAMEWORKS[name](c)
            except Exception as e:  # noqa: BLE001
                status, checks, note = "fail", [(False, f"the check crashed: {e!r}")], ""
            print(("\r" if tty else "") + f"{name}: {status.upper()}" + (f" ({note})" if note else "") + (" " * 4 if tty else ""))
            for ok, desc in checks:
                print(f"  {'ok  ' if ok else 'FAIL'} {desc}")
            failed = failed or status == "fail"
    finally:
        c.stop_all()
        if not args.keep:
            shutil.rmtree(work, ignore_errors=True)
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
