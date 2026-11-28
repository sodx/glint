"""Does Glint help a coding agent fix bugs? A small, repeatable A/B experiment.

    python3 examples/agent-eval/run.py --dry-run            # what would run, costs nothing
    python3 examples/agent-eval/run.py --yes --trials 2      # spends money: see README.md

Each task in tasks/ is a small program with a seeded bug, a prompt and a check command. For every
trial the task is copied to a fresh folder and Claude Code (headless, `claude -p`) is asked to fix
it twice: once with Glint (MCP server, hooks, glint() in the programs) and once without (no MCP
server, no hooks, GLINT_ENABLED=0). Then the check runs. Turns, wall time, cost and pass/fail go to
results.jsonl, plus how the agent used Glint (from glintd's agent statistics), and a summary per
condition is printed.

A private glintd is started for the run (its own port, database and HOME under the output
folder), so your own events and settings are not touched. glint() inside the programs needs
`glint install` to have been run once on this machine.

Opt-in only: nothing in the test suites calls this.
"""
import argparse
import json
import os
import shutil
import statistics
import subprocess
import sys
import time
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(os.path.dirname(HERE))
CONDITIONS = ("glint", "plain")


def parse():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--tasks", nargs="*", help="task names (default: all in tasks/)")
    p.add_argument("--trials", type=int, default=1, help="trials per task and condition (default 1)")
    p.add_argument("--conditions", nargs="*", default=list(CONDITIONS), choices=CONDITIONS)
    p.add_argument("--model", help="passed to claude --model (default: Claude Code's default)")
    p.add_argument("--max-turns", type=int, help="passed to claude --max-turns (default: no limit)")
    p.add_argument("--timeout", type=int, default=900, help="seconds per agent session")
    p.add_argument("--bin-dir", default=os.path.join(REPO, "target", "release"), help="where glintd and glint are")
    p.add_argument("--port", type=int, default=23799, help="port for the private glintd")
    p.add_argument("--out", default=os.path.join(HERE, "out", time.strftime("%Y%m%d-%H%M%S")))
    p.add_argument("--yes", action="store_true", help="really run the agent (costs money)")
    p.add_argument("--dry-run", action="store_true", help="print the plan and exit")
    return p.parse_args()


def tasks(names):
    root = os.path.join(HERE, "tasks")
    found = sorted(d for d in os.listdir(root) if os.path.isfile(os.path.join(root, d, "task.json")))
    for n in names or found:
        if n not in found:
            sys.exit(f"no task {n!r} (have: {', '.join(found)})")
        t = json.load(open(os.path.join(root, n, "task.json")))
        t["name"], t["dir"] = n, os.path.join(root, n)
        missing = [x for x in t.get("needs", []) if not shutil.which(x)]
        if missing:
            print(f"skipping {n}: needs {', '.join(missing)}")
            continue
        yield t


def get(url):
    with urllib.request.urlopen(urllib.request.Request(url, headers={"x-glint": "1"}), timeout=10) as r:
        return json.loads(r.read())


def start_glintd(a, home):
    url = f"http://127.0.0.1:{a.port}"
    env = dict(os.environ, HOME=home, GLINT_DB=os.path.join(home, "glint.db"), GLINT_ADDR=f"127.0.0.1:{a.port}",
               GLINT_OTLP_ADDR="off")
    log = open(os.path.join(home, "glintd.log"), "w")
    proc = subprocess.Popen([os.path.join(a.bin_dir, "glintd")], env=env, stdout=log, stderr=subprocess.STDOUT)
    for _ in range(100):
        try:
            get(url + "/v1/info")
            return proc, url
        except Exception:
            time.sleep(0.1)
    proc.kill()
    sys.exit(f"glintd did not start on {url}; see {home}/glintd.log")


def agent_config(cond, url, a, home, work):
    """--mcp-config and --settings files for one condition."""
    mcp = {"mcpServers": {"glint": {"type": "http", "url": url + "/mcp"}}} if cond == "glint" else {"mcpServers": {}}
    settings = {}
    if cond == "glint":
        # The hook's own state goes to the private HOME, not yours.
        cmd = f"HOME={home} GLINT_URL={url} {os.path.join(a.bin_dir, 'glint')} hook"
        out = subprocess.run([os.path.join(a.bin_dir, "glint"), "hook", "--print-config"], capture_output=True, text=True, check=True)
        settings = json.loads(out.stdout.replace('"glint hook"', json.dumps(cmd)))
    files = []
    for name, body in (("mcp.json", mcp), ("settings.json", settings)):
        path = os.path.join(work, "..", f"{os.path.basename(work)}.{name}")
        json.dump(body, open(path, "w"), indent=2)
        files.append(os.path.abspath(path))
    return files


def run_trial(t, cond, n, a, url, home):
    work = os.path.join(a.out, "work", f"{t['name']}-{cond}-{n}")
    shutil.copytree(t["dir"], work, ignore=shutil.ignore_patterns("task.json"))
    subprocess.run(["git", "init", "-q"], cwd=work)
    subprocess.run(["git", "add", "-A"], cwd=work)
    subprocess.run(["git", "-c", "user.name=eval", "-c", "user.email=eval@localhost", "commit", "-qm", "task"], cwd=work)
    project = os.path.basename(work)
    mcp_file, settings_file = agent_config(cond, url, a, home, work)
    env = dict(os.environ, GLINT_URL=url, GLINT_PROJECT=project)
    if cond == "plain":
        env["GLINT_ENABLED"] = "0"
    cmd = ["claude", "-p", t["prompt"], "--output-format", "json",
           "--mcp-config", mcp_file, "--strict-mcp-config", "--settings", settings_file,
           # Only this run's settings: your own hooks (maybe Glint's) must not leak into "plain".
           "--setting-sources", "project,local",
           "--permission-mode", "acceptEdits", "--allowedTools", "Bash,Read,Edit,Write,Glob,Grep,mcp__glint"]
    if a.model:
        cmd += ["--model", a.model]
    if a.max_turns:
        cmd += ["--max-turns", str(a.max_turns)]
    started = time.time()
    try:
        p = subprocess.run(cmd, cwd=work, env=env, capture_output=True, text=True, timeout=a.timeout)
        out = json.loads(p.stdout or "{}")
    except subprocess.TimeoutExpired:
        out = {"is_error": True, "result": "timeout"}
    except json.JSONDecodeError:
        out = {"is_error": True, "result": (p.stdout or p.stderr)[-500:]}
    seconds = round(time.time() - started, 1)
    check_env = dict(env, GLINT_ENABLED="0")  # the check itself is not part of the agent's evidence
    check = subprocess.run(t["check"], cwd=work, env=check_env, capture_output=True, text=True, timeout=120)
    use = {}
    if cond == "glint":
        st = get(f"{url}/v1/agents/stats?days=1&project={urllib.request.quote(project)}")["totals"]
        use = {k: st[k] for k in ("calls", "tools", "hook_context", "stop_guard", "failures", "read", "read_in_time", "median_read_s") if k in st}
    return {
        "task": t["name"], "condition": cond, "trial": n, "pass": check.returncode == 0,
        "turns": out.get("num_turns"), "seconds": seconds, "agent_ms": out.get("duration_ms"),
        "cost_usd": out.get("total_cost_usd"), "agent_error": bool(out.get("is_error")),
        "glint": use, "check_output": (check.stdout + check.stderr).strip()[-300:], "folder": work,
    }


def summary(rows):
    print("\ncondition  pass  median turns  median s  cost $")
    for cond in CONDITIONS:
        r = [x for x in rows if x["condition"] == cond]
        if not r:
            continue
        med = lambda k: statistics.median([x[k] for x in r if x[k] is not None] or [0])
        cost = sum(x["cost_usd"] or 0 for x in r)
        print(f"{cond:<9}  {sum(x['pass'] for x in r)}/{len(r):<3} {med('turns'):>12} {med('seconds'):>9} {cost:>7.2f}")
    print("\nPer task: " + ", ".join(
        f"{t} {'/'.join(str(sum(x['pass'] for x in rows if x['task'] == t and x['condition'] == c)) for c in CONDITIONS)}"
        for t in sorted({x["task"] for x in rows})) + " (passes with / without Glint)")


def main():
    a = parse()
    plan = [(t, c, n) for t in tasks(a.tasks) for n in range(1, a.trials + 1) for c in a.conditions]
    print(f"{len(plan)} agent sessions: " + ", ".join(sorted({f'{t["name"]}' for t, _, _ in plan})) + f" × {a.conditions} × {a.trials}")
    print("Each is one headless Claude Code session (usually 5 to 25 turns); see README.md for the cost.")
    if a.dry_run:
        for t, c, n in plan:
            print(f"  {t['name']:<10} {c:<6} #{n}  check: {' '.join(t['check'])}")
        return
    if not a.yes:
        if not sys.stdin.isatty() or input("This spends API credit. Type yes to go on: ").strip() != "yes":
            sys.exit("Not started. Pass --yes to run without asking.")
    if not shutil.which("claude"):
        sys.exit("claude (Claude Code) is not on PATH")
    os.makedirs(a.out, exist_ok=True)
    home = os.path.join(a.out, "home")
    os.makedirs(home, exist_ok=True)
    proc, url = start_glintd(a, home)
    rows = []
    try:
        with open(os.path.join(a.out, "results.jsonl"), "a") as f:
            for t, c, n in plan:
                r = run_trial(t, c, n, a, url, home)
                rows.append(r)
                f.write(json.dumps(r) + "\n")
                f.flush()
                print(f"{r['task']:<10} {c:<6} #{n}  {'PASS' if r['pass'] else 'FAIL'}  {r['turns']} turns  {r['seconds']} s  ${r['cost_usd'] or 0:.2f}"
                      + (f"  glint calls {r['glint'].get('calls', 0)}" if c == "glint" else ""))
    finally:
        proc.terminate()
    summary(rows)
    print(f"\nResults: {a.out}/results.jsonl. The private glintd's database is {home}/glint.db:")
    print(f"  GLINT_ADDR=127.0.0.1:{a.port} GLINT_DB={home}/glint.db GLINT_OTLP_ADDR=off HOME={home} glintd")
    print("  then open its window and press g for the Agents view.")


if __name__ == "__main__":
    main()
