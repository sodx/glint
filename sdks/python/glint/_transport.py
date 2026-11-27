"""Background sender: a bounded queue drained by one daemon thread, one POST per batch.

Never raises into the caller and never blocks it: submit() only puts on a queue. When Glint is
unreachable the sender backs off for a few seconds and drops events meanwhile; drops are reported
in the next delivered envelope's meta.dropped.

Exit is cheap whatever state glintd is in: the exit flush waits only while batches keep getting
delivered (EXIT_IDLE_S without progress and it gives up, EXIT_MAX_S in all), returns at once when
glintd was just found unreachable, and does not wait a second time for a send it already gave up
on. A forked child starts with an empty queue and its own sender thread, so it sends its own events
and never waits for its parent's.
"""

from __future__ import annotations

import atexit
import json
import os
import queue
import sys
import threading
import time
import urllib.error
import urllib.request
from typing import Any, Dict, List, Optional

from . import _config

MAX_QUEUE = 10_000
MAX_BATCH = 500
# glintd is local: connecting is instant or refused, and storing a batch takes milliseconds. A
# longer wait means it is stalled; the sender gives up and backs off.
TIMEOUT_S = 1.0
RETRY_AFTER_S = 5.0
EXIT_IDLE_S = 0.3
EXIT_MAX_S = 2.0

_queue: "queue.Queue[Dict[str, Any]]" = queue.Queue(maxsize=MAX_QUEUE)
_lock = threading.Lock()
_start_lock = threading.Lock()
_pid = os.getpid()
_dropped = 0
_down_until = 0.0
_started = False
_exit_hooks = False
_warned = False
_batches = 0  # batches the sender has finished (delivered or dropped)
_gave_up_at: Optional[int] = None  # _batches when an exit flush last gave up on a stuck send
# Ignore HTTP_PROXY and friends: Glint is a local server.
_opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))


def submit(envelope: Dict[str, Any]) -> None:
    _ensure_started()
    q = _queue
    while True:
        try:
            q.put_nowait(envelope)
            return
        except queue.Full:
            # Drop the oldest queued event to make room for the newest.
            try:
                q.get_nowait()
            except queue.Empty:
                continue
            q.task_done()
            _count_dropped(1)


def is_down() -> bool:
    """True while glintd was just found unreachable (the sender drops events for a while)."""
    return time.monotonic() < _down_until


def flush(timeout: float = 2.0) -> None:
    """Blocks until everything queued so far has been sent, or `timeout` seconds pass."""
    if not _started or _pid != os.getpid():
        return
    deadline = time.monotonic() + timeout
    q = _queue
    with q.all_tasks_done:
        while q.unfinished_tasks:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                return
            q.all_tasks_done.wait(remaining)


def flush_at_exit() -> None:
    """Sends what is queued before the process goes away, without delaying its exit: waits only
    while batches keep being delivered, and not at all when glintd is down or already stuck."""
    global _gave_up_at
    if not _started or _pid != os.getpid() or is_down():
        return
    if _gave_up_at is not None and _gave_up_at == _batches:
        return  # an earlier exit flush gave up on this very send
    start = last_t = time.monotonic()
    last = _batches
    q = _queue
    while q.unfinished_tasks and not is_down():
        now = time.monotonic()
        if _batches != last:
            last, last_t = _batches, now
        if now - last_t >= EXIT_IDLE_S or now - start >= EXIT_MAX_S:
            _gave_up_at = _batches
            return
        time.sleep(0.005)


def _ensure_started() -> None:
    global _started, _exit_hooks
    if _started and _pid == os.getpid():
        return
    with _start_lock:
        if _pid != os.getpid():
            _after_fork_in_child()  # a fork that skipped os.register_at_fork's hooks
        if _started:
            return
        threading.Thread(target=_worker, args=(_queue,), name="glint-sender", daemon=True).start()
        if not _exit_hooks:
            _exit_hooks = True  # inherited by forked children, which run it on their own state
            atexit.register(_at_exit)
        # multiprocessing children leave through os._exit, which skips atexit; its own exit
        # function runs these finalizers first. The registry is per process (cleared in a child).
        mp_util = sys.modules.get("multiprocessing.util")
        if mp_util is not None:
            try:
                mp_util.Finalize(None, _at_exit, exitpriority=10)
            except Exception:
                pass
        _started = True


def _after_fork_in_child() -> None:
    """The child has a copy of the parent's queue and locks but not its sender thread: start
    over, so the child sends its own events and its exit does not wait for the parent's."""
    global _queue, _lock, _start_lock, _pid, _dropped, _started, _batches, _gave_up_at
    _queue = queue.Queue(maxsize=MAX_QUEUE)
    _lock = threading.Lock()
    _start_lock = threading.Lock()
    _pid = os.getpid()
    _dropped = 0
    _started = False
    _batches = 0
    _gave_up_at = None


if hasattr(os, "register_at_fork"):
    os.register_at_fork(after_in_child=_after_fork_in_child)


def _at_exit() -> None:
    try:
        flush_at_exit()
    except Exception:
        pass


def _worker(q: "queue.Queue[Dict[str, Any]]") -> None:
    global _batches
    while True:
        batch = [q.get()]
        while len(batch) < MAX_BATCH:
            try:
                batch.append(q.get_nowait())
            except queue.Empty:
                break
        try:
            _deliver(batch)
        except Exception:
            pass
        finally:
            _batches += 1
            for _ in batch:
                q.task_done()


def _deliver(batch: List[Dict[str, Any]]) -> None:
    global _dropped
    if is_down():
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
