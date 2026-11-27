"""Background sender: a bounded queue drained by one daemon thread, one POST per batch.

Never raises into the caller. When Glint is unreachable the sender backs off for a few seconds
and drops events meanwhile; drops are reported in the next delivered envelope's meta.dropped.
"""

from __future__ import annotations

import atexit
import json
import queue
import sys
import threading
import time
import urllib.error
import urllib.request
from typing import Any, Dict, List

from . import _config

MAX_QUEUE = 10_000
MAX_BATCH = 500
TIMEOUT_S = 2.0
RETRY_AFTER_S = 5.0
FLUSH_AT_EXIT_S = 2.0

_queue: "queue.Queue[Dict[str, Any]]" = queue.Queue(maxsize=MAX_QUEUE)
_lock = threading.Lock()
_start_lock = threading.Lock()
_dropped = 0
_down_until = 0.0
_started = False
_warned = False
# Ignore HTTP_PROXY and friends: Glint is a local server.
_opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))


def submit(envelope: Dict[str, Any]) -> None:
    _ensure_started()
    while True:
        try:
            _queue.put_nowait(envelope)
            return
        except queue.Full:
            # Drop the oldest queued event to make room for the newest.
            try:
                _queue.get_nowait()
            except queue.Empty:
                continue
            _queue.task_done()
            _count_dropped(1)


def flush(timeout: float = 2.0) -> None:
    """Blocks until everything queued so far has been sent, or `timeout` seconds pass."""
    if not _started:
        return
    deadline = time.monotonic() + timeout
    with _queue.all_tasks_done:
        while _queue.unfinished_tasks:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                return
            _queue.all_tasks_done.wait(remaining)


def _ensure_started() -> None:
    global _started
    if _started:
        return
    with _start_lock:
        if _started:
            return
        threading.Thread(target=_worker, name="glint-sender", daemon=True).start()
        atexit.register(_at_exit)
        _started = True


def _at_exit() -> None:
    try:
        flush(FLUSH_AT_EXIT_S)
    except Exception:
        pass


def _worker() -> None:
    while True:
        batch = [_queue.get()]
        while len(batch) < MAX_BATCH:
            try:
                batch.append(_queue.get_nowait())
            except queue.Empty:
                break
        try:
            _deliver(batch)
        except Exception:
            pass
        finally:
            for _ in batch:
                _queue.task_done()


def _deliver(batch: List[Dict[str, Any]]) -> None:
    global _dropped
    if time.monotonic() < _down_until:
        _count_dropped(len(batch))
        return
    with _lock:
        carried, _dropped = _dropped, 0
    if carried:
        first = dict(batch[0])
        meta = dict(first.get("meta") or {})
        meta["dropped"] = carried
        first["meta"] = meta
        batch[0] = first
    url = _config.settings.url.rstrip("/") + "/v1/events"
    try:
        body = json.dumps(batch, allow_nan=False).encode("utf-8")
        req = urllib.request.Request(
            url, data=body, headers={"Content-Type": "application/json"}, method="POST"
        )
        with _opener.open(req, timeout=TIMEOUT_S) as res:
            res.read()
    except urllib.error.HTTPError as e:
        if e.code >= 500:
            _go_down(url, len(batch) + carried)
    except Exception:
        _go_down(url, len(batch) + carried)


def _go_down(url: str, lost: int) -> None:
    global _down_until, _warned
    _down_until = time.monotonic() + RETRY_AFTER_S
    _count_dropped(lost)
    if not _warned:
        _warned = True
        try:
            sys.stderr.write(f"glint: cannot reach {url}; events are dropped until it answers\n")
        except Exception:
            pass


def _count_dropped(n: int) -> None:
    global _dropped
    with _lock:
        _dropped += n
