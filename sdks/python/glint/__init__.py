"""Glint SDK for Python (3.9+, stdlib only).

    from glint import glint
    glint(order).label("before save").orange()
    glint.log("user signed in", {"user_id": 7}, "info")

Contract: spec/conformance/README.md. Never raises into your program, never blocks it.
"""

from __future__ import annotations

import contextvars
import inspect
import json
import os
import sys
import time
import urllib.parse
from datetime import datetime, timezone
from types import FrameType
from typing import Any, Dict, Iterable, List, Mapping, Optional

from . import _config, _encode, _ids, _origin, _transport

__version__ = "0.1.0"
__all__ = ["glint", "Entry", "GlintStop", "__version__"]


class GlintStop(Exception):
    """Raised by glint.pause() when someone answers the pause with Stop."""

COLORS = ("gray", "green", "orange", "red", "blue", "purple")
RUN = "python-" + _ids.ulid()  # one run per process


def _now() -> str:
    return datetime.now(timezone.utc).isoformat(timespec="microseconds").replace("+00:00", "Z")


# Nesting: everything sent inside glint.measure(name, fn) has that measure as its parent, so a run
# reads as a call tree. A ContextVar follows threads' own contexts and asyncio tasks.
_PARENT: "contextvars.ContextVar[Optional[str]]" = contextvars.ContextVar("glint_parent", default=None)
# Framework integrations give each request its own run (glint/_frameworks.py).
_RUN: "contextvars.ContextVar[str]" = contextvars.ContextVar("glint_run", default=RUN)


def _emit(kind: str, payload: Any, meta: Optional[Dict[str, Any]] = None, site: Optional[Dict[str, Any]] = None, event_id: Optional[str] = None) -> Optional[str]:
    """Builds and queues one envelope. Returns its id, or None when disabled or on any failure."""
    settings = _config.settings
    if not settings.enabled:
        return None
    try:
        if site is None:
            site = _origin.call_site()
        event_id = event_id or _ids.ulid()
        envelope: Dict[str, Any] = {
            "id": event_id,
            "ts": _now(),
            "run": _RUN.get(),
            "kind": kind,
            "payload": payload,
            "origin": _origin.origin(site),
        }
        parent = _PARENT.get()
        if parent and parent != event_id:
            envelope["parent"] = parent
        if meta:
            envelope["meta"] = meta
        if settings.project:
            envelope["project"] = settings.project
        _transport.submit(envelope)
        return event_id
    except Exception:
        return None


def _value_payload(v: Any) -> Dict[str, Any]:
    payload: Dict[str, Any] = {"value": _encode.encode(v)}
    type_ = _encode.type_name(v)
    if type_:
        payload["type"] = type_
    return payload


def _record_measure(name: str, start: float, site: Dict[str, Any], event_id: Optional[str]) -> None:
    duration_ms = round((time.perf_counter() - start) * 1000, 3)
    _emit("core.measure", {"name": str(name), "duration_ms": duration_ms}, None, site, event_id)


async def _await_measure(awaitable: Any, name: str, start: float, site: Dict[str, Any], event_id: Optional[str]) -> Any:
    token = _PARENT.set(event_id)
    try:
        return await awaitable
    finally:
        _PARENT.reset(token)
        _record_measure(name, start, site, event_id)


class Entry:
    """Handle for events just sent. Its methods change how they are shown, by sending core.update."""

    __slots__ = ("_ids",)

    def __init__(self, ids: Iterable[Optional[str]]) -> None:
        self._ids: List[str] = [i for i in ids if i]

    def _update(self, change: Dict[str, Any]) -> "Entry":
        site = _origin.call_site()
        for target in self._ids:
            _emit("core.update", {"target": target, **change}, None, site)
        return self

    def label(self, label: Any) -> "Entry":
        return self._update({"meta": {"label": str(label)}})

    def color(self, color: str) -> "Entry":
        return self._update({"meta": {"color": color}}) if color in COLORS else self

    def gray(self) -> "Entry":
        return self.color("gray")

    def green(self) -> "Entry":
        return self.color("green")

    def orange(self) -> "Entry":
        return self.color("orange")

    def red(self) -> "Entry":
        return self.color("red")

    def blue(self) -> "Entry":
        return self.color("blue")

    def purple(self) -> "Entry":
        return self.color("purple")

    def small(self) -> "Entry":
        return self._update({"meta": {"size": "sm"}})

    def large(self) -> "Entry":
        return self._update({"meta": {"size": "lg"}})

    def hide(self) -> "Entry":
        return self._update({"hidden": True})

    def remove(self) -> "Entry":
        return self._update({"removed": True})


def _exception_payload(err: Any, stack: Optional[FrameType], depth: int = 0) -> Dict[str, Any]:
    if not isinstance(err, BaseException):
        return {"class": type(err).__name__, "message": _safe_message(err), "frames": []}
    cls = type(err)
    name = cls.__qualname__ if cls.__module__ == "builtins" else f"{cls.__module__}.{cls.__qualname__}"
    tb = err.__traceback__
    frames = _origin.frames_from_traceback(tb) if tb is not None else _origin.frames_from_stack(stack)
    payload: Dict[str, Any] = {"class": name, "message": _safe_message(err), "frames": frames}
    cause = err.__cause__ or err.__context__
    if cause is not None and depth < 5:
        payload["cause"] = _exception_payload(cause, None, depth + 1)
    return payload


def _safe_message(err: BaseException) -> str:
    try:
        return str(err)
    except Exception:
        return "<unprintable>"


_pause_warned = False


def _warn_pause(url: str, err: BaseException) -> None:
    """Once per process: a pause that could not be asked about carries on, but says why."""
    global _pause_warned
    if _pause_warned:
        return
    _pause_warned = True
    try:
        sys.stderr.write(f"glint: pause skipped, cannot ask {url}: {err}\n")
    except Exception:
        pass


class _Glint:
    """The `glint` object: call it with values, or use its helper methods."""

    def __call__(self, *values: Any) -> Entry:
        """Sends each value as its own event; an exception becomes core.exception."""
        site = _origin.call_site()
        stack = sys._getframe(1)
        return Entry(
            _emit("core.exception", _exception_payload(v, stack), None, site)
            if isinstance(v, BaseException)
            else _emit("core.value", _value_payload(v), None, site)
            for v in values
        )

    def log(self, message: Any, fields: Optional[Mapping[str, Any]] = None, level: str = "info") -> Entry:
        site = _origin.call_site()
        payload: Dict[str, Any] = {"message": str(message)}
        if fields is not None:
            payload["fields"] = _encode.encode(fields)
        return Entry([_emit("core.log", payload, {"level": level}, site)])

    def table(self, rows: Iterable[Any], columns: Optional[Iterable[Any]] = None) -> Entry:
        site = _origin.call_site()
        payload: Dict[str, Any] = {"rows": _encode.encode(list(rows or []))}
        if columns is not None:
            payload["columns"] = [str(c) for c in columns]
        return Entry([_emit("core.table", payload, None, site)])

    def exception(self, err: BaseException) -> Entry:
        site = _origin.call_site()
        stack = sys._getframe(1)
        return Entry([_emit("core.exception", _exception_payload(err, stack), None, site)])

    def measure(self, name: str, fn: Any) -> Any:
        site = _origin.call_site()
        start = time.perf_counter()
        # The id is chosen up front so that what fn sends can name the measure as its parent.
        event_id = _ids.ulid() if _config.settings.enabled else None
        token = _PARENT.set(event_id)
        try:
            result = fn()
        except BaseException:
            _PARENT.reset(token)
            _record_measure(name, start, site, event_id)
            raise
        _PARENT.reset(token)
        if inspect.iscoroutine(result):
            return _await_measure(result, name, start, site, event_id)
        _record_measure(name, start, site, event_id)
        return result

    def send(self, kind: str, payload: Any, meta: Optional[Mapping[str, Any]] = None) -> Entry:
        site = _origin.call_site()
        encoded_meta = _encode.encode(dict(meta)) if meta else None
        return Entry([_emit(str(kind), _encode.encode(payload), encoded_meta, site)])

    def flush(self, timeout: float = 2.0) -> None:
        try:
            _transport.flush(timeout)
        except Exception:
            pass

    def pause(self, reason: Optional[str] = None, timeout: float = 300.0) -> Any:
        """Stops here until someone presses Continue in Glint (or an agent answers through MCP),
        then returns the value they gave, or None. A Stop raises GlintStop. Returns at once when
        Glint is off or unreachable, and on CI (unless GLINT_FORCE_PAUSE is set)."""
        settings = _config.settings
        if not settings.enabled or (os.environ.get("CI") and not os.environ.get("GLINT_FORCE_PAUSE")):
            return None
        pause_id = _emit("ctl.pause", {} if reason is None else {"reason": str(reason)}, None, _origin.call_site())
        if not pause_id:
            return None
        self.flush(5.0)  # the pause must be stored before we ask about it
        url = f"{settings.url.rstrip('/')}/v1/ctl/{urllib.parse.quote(pause_id)}?timeout_s=25"
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            try:
                # The sender's opener: no HTTP_PROXY for a local server.
                with _transport._opener.open(url, timeout=30) as res:
                    if res.status == 204:
                        continue
                    answer = json.loads(res.read() or b"{}")
            except Exception as e:
                _warn_pause(url, e)
                return None  # unknown pause (glintd restarted) or Glint went away: carry on
            if answer.get("action") == "stop":
                raise GlintStop()
            return answer.get("value")
        return None

    def configure(self, url: Optional[str] = None, enabled: Optional[bool] = None, project: Optional[str] = None) -> None:
        if url:
            _config.settings.url = url
        if enabled is not None:
            _config.settings.enabled = bool(enabled)
        if project:
            _config.settings.project = project


glint = _Glint()
_Glint.GlintStop = GlintStop  # reachable from the global too: `except glint.GlintStop`
