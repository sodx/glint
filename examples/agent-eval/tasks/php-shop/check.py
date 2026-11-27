"""Exit 0 when the shop answers all three orders right. PHP shows no errors to the browser, as
in production."""
import json
import random
import socket
import subprocess
import sys
import time
import urllib.request

port = 8600 + random.randint(0, 300)
srv = subprocess.Popen(["php", "-d", "display_errors=0", "-S", f"127.0.0.1:{port}", "index.php"],
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
for _ in range(50):
    try:
        socket.create_connection(("127.0.0.1", port), timeout=0.2).close()
        break
    except OSError:
        time.sleep(0.1)
want = {7: ("Ada", 36.0), 8: ("Grace", 200.0), 9: ("guest", 120.0)}
failed = 0
try:
    for oid, (name, total) in want.items():
        try:
            with urllib.request.urlopen(f"http://127.0.0.1:{port}/?order={oid}", timeout=3) as r:
                body = json.loads(r.read() or b"null")
        except urllib.error.HTTPError as e:
            print(f"order {oid}: HTTP {e.code}, empty body")
            failed += 1
            continue
        except OSError as e:
            print(f"order {oid}: no answer ({e})")
            failed += 1
            continue
        if not body or body.get("customer") != name or abs(float(body.get("total", -1)) - total) > 0.001:
            print(f"order {oid}: got {body}, want {name} {total}")
            failed += 1
        else:
            print(f"order {oid}: ok")
finally:
    srv.kill()
sys.exit(1 if failed else 0)
