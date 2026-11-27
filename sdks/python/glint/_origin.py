"""Call sites, stack frames and the origin block of each envelope."""

from __future__ import annotations

import os
import platform
import socket
import sys
import sysconfig
from types import FrameType, TracebackType
from typing import Any, Dict, List, Optional

from . import _config

SDK = "glint-python/0.1.0"

_PKG_DIR = os.path.dirname(os.path.abspath(__file__)) + os.sep
_STDLIB = tuple({p for p in (sysconfig.get_paths().get("stdlib"), sysconfig.get_paths().get("platstdlib")) if p})
_RUNTIME = f"{platform.python_implementation().lower()} {platform.python_version()}"
try:
    _HOST: Optional[str] = socket.gethostname()
except Exception:
    _HOST = None


def _in_package(filename: str) -> bool:
    return os.path.abspath(filename).startswith(_PKG_DIR)


def _is_vendor(path: str) -> bool:
    return "site-packages" in path or "dist-packages" in path or path.startswith(_STDLIB)


def _frame(filename: str, line: int, fn: str) -> Dict[str, Any]:
    path = os.path.abspath(filename)
    return {"file": path, "line": int(line), "fn": fn, "vendor": _is_vendor(path)}


def call_site() -> Dict[str, Any]:
    """The first frame outside this package: the user's call site."""
    frame: Optional[FrameType] = sys._getframe(1)
    while frame is not None and _in_package(frame.f_code.co_filename):
        frame = frame.f_back
    if frame is None:
        return {}
    code = frame.f_code
    return {"file": os.path.abspath(code.co_filename), "line": frame.f_lineno, "fn": code.co_name}


def frames_from_traceback(tb: Optional[TracebackType]) -> List[Dict[str, Any]]:
    """Innermost frame first (the traceback object lists outermost first)."""
    frames: List[Dict[str, Any]] = []
    while tb is not None:
        code = tb.tb_frame.f_code
        frames.append(_frame(code.co_filename, tb.tb_lineno, code.co_name))
        tb = tb.tb_next
    frames.reverse()
    return frames


def frames_from_stack(frame: Optional[FrameType]) -> List[Dict[str, Any]]:
    """Innermost first, starting at `frame` and walking outwards; skips this package's frames."""
    frames: List[Dict[str, Any]] = []
    while frame is not None:
        if not _in_package(frame.f_code.co_filename):
            frames.append(_frame(frame.f_code.co_filename, frame.f_lineno, frame.f_code.co_name))
        frame = frame.f_back
    return frames


def origin(site: Dict[str, Any]) -> Dict[str, Any]:
    o: Dict[str, Any] = {"lang": "python", "sdk": SDK, "runtime": _RUNTIME, "pid": os.getpid()}
    if _HOST:
        o["host"] = _HOST
    try:
        o["cwd"] = os.getcwd()
    except OSError:
        pass
    if _config.settings.path_map:
        o["path_map"] = _config.settings.path_map
    o.update(site)
    return o
