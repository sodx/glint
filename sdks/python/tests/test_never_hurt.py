"""The SDK must never change, slow down or break the program it is in.

    python3 -s -m unittest discover sdks/python/tests

Each test runs the SDK from this checkout against a fake glintd in this process (or a socket that
accepts and never answers), never against a real one. Run with -s so a `glint install` in your own
user site-packages stays out of it.
"""

from __future__ import annotations

import json
import os
import socket
import subprocess
import sys
import textwrap
import threading
import time
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import Any, Dict, List

HERE = os.path.dirname(os.path.abspath(__file__))
SDK = os.path.dirname(HERE)
SHIM = os.path.join(os.path.dirname(SDK), "global")
sys.path.insert(0, SDK)


class FakeGlintd:
    """Stores what is POSTed to /v1/events; answers GET /v1/ctl/<id> with `pause_answer`."""

    def __init__(self) -> None:
        self.events: List[Dict[str, Any]] = []
        self.pause_answer: Any = {"action": "continue", "value": 7}
        fake = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *a: Any) -> None:
                pass

            def do_POST(self) -> None:
                body = self.rfile.read(int(self.headers.get("Content-Length") or 0))
                fake.events.extend(json.loads(body))
                self.send_response(200)
                self.send_header("Content-Length", "2")
                self.end_headers()
                self.wfile.write(b"{}")

            def do_GET(self) -> None:
                if not self.path.startswith("/v1/ctl/") or fake.pause_answer is None:
                    self.send_response(404)
                    self.send_header("Content-Length", "0")
                    self.end_headers()
                    return
                body = json.dumps(fake.pause_answer).encode()
                self.send_response(200)
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.url = "http://127.0.0.1:%d" % self.server.server_address[1]
        threading.Thread(target=self.server.serve_forever, daemon=True).start()

    def values(self) -> List[Any]:
        return [e["payload"].get("value") for e in self.events if e["kind"] == "core.value"]

    def wait_for(self, pred: Any, seconds: float = 3.0) -> bool:
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            if pred():
                return True
            time.sleep(0.02)
        return False

    def close(self) -> None:
        self.server.shutdown()
        self.server.server_close()


def stalled_port() -> int:
    """A port that accepts connections and never answers: glintd up but stuck."""
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    s.listen(64)
    held = [s]

    def loop() -> None:
        while True:
            c, _ = s.accept()
            held.append(c)

    threading.Thread(target=loop, daemon=True).start()
    return s.getsockname()[1]


def closed_port() -> int:
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


def run(code: str, url: str, **env: str) -> "tuple[subprocess.CompletedProcess[str], float]":
    proxies = ("HTTP_PROXY", "HTTPS_PROXY", "http_proxy", "https_proxy")
    full_env = {k: v for k, v in os.environ.items() if not k.startswith("GLINT_") and k not in proxies}
    full_env.update(PYTHONPATH=SDK, GLINT_URL=url, GLINT_ENABLED="1", PYTHONWARNINGS="ignore")
    full_env.update(env)
    t = time.monotonic()
    p = subprocess.run([sys.executable, "-s", "-c", textwrap.dedent(code)], env=full_env, capture_output=True, text=True, timeout=60)
    return p, time.monotonic() - t


class Base(unittest.TestCase):
    def setUp(self) -> None:
        self.glintd = FakeGlintd()

    def tearDown(self) -> None:
        self.glintd.close()


class LoggingStaysTheApps(Base):
    """a. Hooking into logging must not change what the app prints."""

    def test_unconfigured_app_still_prints_warnings_after_a_request(self) -> None:
        p, _ = run(
            """
            import logging
            from glint import _frameworks as f
            f.end(f.begin("Test", "GET", "/", "http://x/"), 200)
            logging.getLogger("app").warning("LAST-RESORT-WARNING")
            logging.getLogger("app").error("boom", exc_info=ValueError("logged"))
            from glint import glint; glint.flush()
            """,
            self.glintd.url,
        )
        self.assertIn("LAST-RESORT-WARNING", p.stderr)
        self.assertIn("boom", p.stderr)
        self.assertTrue(self.glintd.wait_for(lambda: any(e["kind"] == "core.log" and e["payload"]["message"] == "LAST-RESORT-WARNING" for e in self.glintd.events)))
        self.assertTrue(any(e["kind"] == "core.exception" for e in self.glintd.events))

    def test_basic_config_after_a_request_still_works(self) -> None:
        p, _ = run(
            """
            import logging
            from glint import _frameworks as f
            f.end(f.begin("Test", "GET", "/", "http://x/"), 200)
            logging.basicConfig(level=logging.INFO, format="APP %(message)s")
            logging.getLogger("app").info("configured")
            assert logging.getLogger().handlers, "basicConfig did nothing"
            """,
            self.glintd.url,
        )
        self.assertEqual(p.returncode, 0, p.stderr)
        self.assertIn("APP configured", p.stderr)


class ForkedChildren(Base):
    """b. A forked child sends its own events and exits without waiting for its parent's."""

    def test_multiprocessing_and_os_fork_children(self) -> None:
        if not hasattr(os, "fork"):
            self.skipTest("no fork here")
        p, _ = run(
            """
            import multiprocessing as mp, os, sys, time
            from glint import glint

            def child():
                glint("mp-child")

            glint("parent")
            glint.flush()
            mp.set_start_method("fork")
            t = time.monotonic(); c = mp.Process(target=child); c.start(); c.join()
            print("mp %.3f" % (time.monotonic() - t), flush=True)
            t = time.monotonic(); pid = os.fork()
            if pid == 0:
                glint("fork-child")
                sys.exit(0)
            os.waitpid(pid, 0)
            print("fork %.3f" % (time.monotonic() - t), flush=True)
            """,
            self.glintd.url,
        )
        self.assertEqual(p.returncode, 0, p.stderr)
        times = dict(line.split() for line in p.stdout.split("\n") if line)
        self.assertLess(float(times["mp"]), 1.0, p.stdout)
        self.assertLess(float(times["fork"]), 1.0, p.stdout)
        self.assertTrue(self.glintd.wait_for(lambda: {"parent", "mp-child", "fork-child"} <= set(self.glintd.values())), self.glintd.values())


class PauseIgnoresProxy(Base):
    """c. pause() goes straight to the local glintd, and says once when it cannot ask."""

    def test_pause_answers_through_a_broken_proxy(self) -> None:
        dead = "http://127.0.0.1:%d" % closed_port()
        p, _ = run(
            "from glint import glint; print('value', glint.pause('x', timeout=5))",
            self.glintd.url,
            HTTP_PROXY=dead, http_proxy=dead, GLINT_FORCE_PAUSE="1",
        )
        self.assertIn("value 7", p.stdout, p.stderr)

    def test_failed_poll_warns_once(self) -> None:
        self.glintd.pause_answer = None  # 404: glintd does not know the pause
        p, _ = run(
            "from glint import glint; print(glint.pause('a', timeout=5), glint.pause('b', timeout=5))",
            self.glintd.url,
            GLINT_FORCE_PAUSE="1",
        )
        self.assertIn("None None", p.stdout)
        self.assertEqual(p.stderr.count("glint: pause skipped"), 1, p.stderr)


CRASH = """
import sys
sys.path.insert(0, %r)
import glint_builtin
for i in range(20):
    glint(i)
raise KeyError("boom")
""" % SHIM


class ExitIsNotDelayed(Base):
    """d. An uncaught exception exits promptly whether glintd is down, stalled or healthy."""

    def test_crash_exit_delay(self) -> None:
        base = min(run("raise KeyError('boom')", self.glintd.url)[1] for _ in range(3))
        for name, url, budget in (
            ("down", "http://127.0.0.1:%d" % closed_port(), 0.25),
            ("stalled", "http://127.0.0.1:%d" % stalled_port(), 0.6),
            ("healthy", self.glintd.url, 0.25),
        ):
            p, took = min((run(CRASH, url) for _ in range(3)), key=lambda r: r[1])
            with self.subTest(glintd=name):
                self.assertIn("KeyError: 'boom'", p.stderr)  # Python's own traceback, as before
                self.assertNotEqual(p.returncode, 0)
                self.assertLess(took - base, budget, "%s: +%.3fs" % (name, took - base))
        # healthy: every crash delivered its values and the exception before exiting
        self.assertEqual(sum(1 for e in self.glintd.events if e["kind"] == "core.exception"), 3)
        self.assertEqual(sorted(v for v in self.glintd.values() if isinstance(v, int)), sorted(list(range(20)) * 3))

    def test_known_unreachable_skips_report(self) -> None:
        p, _ = run(
            """
            import sys, time
            sys.path.insert(0, %r)
            import glint_builtin
            from glint import _transport
            glint("x")
            time.sleep(0.3)  # the sender finds glintd unreachable
            assert _transport.is_down()
            calls = []
            _transport.flush_at_exit = lambda: calls.append(1)
            glint_builtin._report(ValueError("later"))
            print("flushes", len(calls))
            """ % SHIM,
            "http://127.0.0.1:%d" % closed_port(),
        )
        self.assertIn("flushes 0", p.stdout, p.stderr)


class ExceptionDedupe(unittest.TestCase):
    """e. An exception is reported once; a different one at a reused address is reported too."""

    def setUp(self) -> None:
        from glint import _frameworks

        self.f = _frameworks
        self.sent: List[Any] = []
        self._emit = _frameworks._emit
        _frameworks._emit = lambda kind, payload, *a, **k: self.sent.append(payload)  # type: ignore[assignment]

    def tearDown(self) -> None:
        self.f._emit = self._emit  # type: ignore[assignment]

    def test_same_exception_once(self) -> None:
        e = ValueError("one")
        self.f.report(e)
        self.f.report(e)
        self.assertEqual(len(self.sent), 1)

    def test_reused_id_is_not_mistaken_for_a_reported_exception(self) -> None:
        first = ValueError("first")
        self.f.report(first)
        old = id(first)
        del first
        reused = None
        keep = []
        for i in range(100_000):
            e = ValueError("second %d" % i)
            if id(e) == old:
                reused = e
                break
            keep.append(e)
        if reused is None:
            self.skipTest("allocator did not reuse the address")
        self.f.report(reused)
        self.assertEqual(len(self.sent), 2)

    def test_frozen_dataclass_exception(self) -> None:
        import dataclasses

        @dataclasses.dataclass(frozen=True)
        class Frozen(Exception):
            code: int

        e = Frozen(3)
        self.f.report(e)
        self.f.report(e)
        self.assertEqual(len(self.sent), 1)


if __name__ == "__main__":
    unittest.main()
