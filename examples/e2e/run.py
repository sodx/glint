"""End-to-end check of the zero-config glint(): real programs, real traffic, real daemon.

    python3 examples/e2e/run.py

Needs `glint install` (global glint() in PHP, Python and Node) and a running glintd. Starts the PHP
shop and the Node API, sends them requests, runs the Python worker, answers the Node API's pause
the way the UI does, then checks what glintd stored and prints a verdict per scenario.
"""
import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
GLINT = os.environ.get("GLINT_URL", "http://127.0.0.1:23600")
SHOP, API = "http://127.0.0.1:8091", "http://127.0.0.1:8092"


def http(method, url, body=None, timeout=10):
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(url, data=data, method=method, headers={"content-type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            return r.status, r.read().decode()
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode()


def events(**q):
    qs = "&".join(f"{k}={urllib.request.quote(str(v))}" for k, v in q.items())
    return json.loads(http("GET", f"{GLINT}/v1/events?{qs}")[1])["events"]


def wait_port(url, tries=50):
    for _ in range(tries):
        try:
            urllib.request.urlopen(url, timeout=0.3)
            return
        except urllib.error.HTTPError:
            return
        except Exception:
            time.sleep(0.1)
    raise SystemExit(f"{url} did not start")


def node_env():
    env = dict(os.environ)
    shim = os.path.expanduser("~/.glint/sdk/js/global.mjs")
    if "global.mjs" not in env.get("NODE_OPTIONS", ""):
        env["NODE_OPTIONS"] = f"--import=file://{shim} {env.get('NODE_OPTIONS', '')}"
    return env


checks = []


def check(name, ok, detail=""):
    checks.append(ok)
    print(f"  {'✓' if ok else '✗'} {name}{' — ' + detail if detail else ''}")


if http("GET", f"{GLINT}/v1/info")[0] != 200:
    raise SystemExit(f"glintd is not answering at {GLINT}; run `glint install` or start glintd")
# By time, not id: ids are opaque (agent hooks, for one, bring their own).
from datetime import datetime, timezone
since = datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")

procs = [
    subprocess.Popen(["php", "-S", "127.0.0.1:8091", "-t", os.path.join(HERE, "shop")], cwd=HERE, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL),
    subprocess.Popen(["node", os.path.join(HERE, "api.mjs")], cwd=HERE, env=node_env(), stdout=subprocess.DEVNULL, stderr=subprocess.PIPE),
]
try:
    wait_port(SHOP + "/"), wait_port(API + "/")
    print("PHP shop")
    print("   ", http("GET", SHOP + "/cart/7"))
    print("   ", http("GET", SHOP + "/checkout/7"))
    print("   ", http("GET", SHOP + "/checkout/8"))
    print("Python worker")
    subprocess.run([sys.executable, os.path.join(HERE, "worker.py")], cwd=HERE, check=True)
    print("Node API")
    print("   ", http("GET", API + "/users/7"))
    print("   ", http("GET", API + "/report"))

    # The refund request blocks on glint.pause(); answer it like the UI's Continue button.
    import threading
    out = {}
    t = threading.Thread(target=lambda: out.update(r=http("POST", API + "/refunds/7", timeout=30)))
    t.start()
    pause = None
    for _ in range(50):
        pauses = json.loads(http("GET", f"{GLINT}/v1/ctl")[1])["pauses"]
        pause = next((p for p in pauses if "refund" in (p.get("reason") or "")), None)
        if pause:
            break
        time.sleep(0.1)
    if pause:
        http("POST", f"{GLINT}/v1/ctl/{pause['id']}/resume", {"by": "e2e", "value": {"approve": True}})
    t.join(30)
    print("   ", out.get("r"))
    time.sleep(0.5)

    got = [e for e in events(limit=1000) if e["ts"] >= since]
    by_sdk = lambda sdk: [e for e in got if (e.get("origin") or {}).get("sdk", "").startswith(sdk)]
    php, py, js = by_sdk("glint-php"), by_sdk("glint-python"), by_sdk("glint-js")
    kinds = lambda es: {e["kind"] for e in es}
    print("\nWhat glintd stored")
    check("PHP: request logs, SQL with bindings, table, measure, values", {"core.log", "sql.query", "core.table", "core.measure", "core.value"} <= kinds(php), ", ".join(sorted(kinds(php))))
    sql = next((e for e in php if e["kind"] == "sql.query" and len(e["payload"].get("bindings", [])) == 3), None)
    check("PHP: SQL carries bindings and timing", bool(sql and "duration_ms" in sql["payload"]), sql and json.dumps(sql["payload"]["bindings"]))
    exc = next((e for e in php if e["kind"] == "core.exception"), None)
    check("PHP: uncaught exception arrives as an error", bool(exc and exc["payload"]["class"] == "UnexpectedValueException"), exc and exc["payload"]["message"])
    check("PHP: origin is shop/index.php, not the SDK", all(e["origin"].get("file", "").endswith("shop/index.php") for e in php), "")
    check("Python: job values, logs, measures, table", {"core.value", "core.log", "core.measure", "core.table"} <= kinds(py), ", ".join(sorted(kinds(py))))
    kerr = next((e for e in py if e["kind"] == "core.exception"), None)
    check("Python: KeyError from job 3 is an exception", bool(kerr and kerr["payload"]["class"] == "KeyError"), kerr and kerr["payload"]["message"])
    check("Python: origin is worker.py", all(os.path.basename(e["origin"].get("file", "")) == "worker.py" for e in py if e["origin"].get("file")), "")
    terr = next((e for e in js if e["kind"] == "core.exception"), None)
    check("Node: TypeError from /report is an exception", bool(terr and terr["payload"]["class"] == "TypeError"), terr and terr["payload"]["message"])
    big = next((e for e in js if e.get("meta", {}).get("label") == "catalogue" or e["kind"] == "core.value" and isinstance(e["payload"].get("value"), list)), None)
    check("Node: the 60-product catalogue arrives whole", bool(big and len(big["payload"]["value"]) == 60), "")
    check("Node: pause answered, refund approved", (out.get("r") or (0, ""))[1].startswith("refunded"), (out.get("r") or (0, ""))[1].strip())
    check("All three name the project after the repository", {e["project"] for e in php + py + js} == {"glint"}, ", ".join(sorted({e["project"] for e in php + py + js})))
finally:
    for p in procs:
        p.terminate()
    for p in procs:
        p.wait(5)

print(f"\n{sum(checks)}/{len(checks)} checks passed")
sys.exit(0 if all(checks) else 1)
