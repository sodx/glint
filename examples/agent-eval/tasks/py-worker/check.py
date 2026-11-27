"""Exit 0 when every order is priced as expected."""
import json
import subprocess
import sys

out = subprocess.run([sys.executable, "worker.py"], capture_output=True, text=True, timeout=30)
want = {"A-1": 6.0, "A-2": 32.4, "A-3": 6.53, "A-4": 3.0, "A-5": 32.5}
try:
    got = json.load(open("totals.json"))
except Exception as e:
    sys.exit(f"no totals.json: {e}")
bad = {k: (got.get(k), v) for k, v in want.items() if got.get(k) is None or abs(got[k] - v) > 0.011}
if bad or "skipped 0" not in out.stdout:
    sys.exit(f"wrong totals {bad} / {out.stdout.strip()}")
print("ok")
