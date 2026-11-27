"""Turns any Python value into JSON that says what the value was.

Rules: spec/conformance/README.md, requirement 4.
"""

from __future__ import annotations

import base64
import dataclasses
import datetime
import decimal
import enum
import math
import pathlib
import types
import uuid
from typing import Any, Dict

MAX_DEPTH = 10
MAX_ITEMS = 1000
MAX_STRING = 100_000


def type_name(value: Any) -> Any:
    """Qualified class name for a top-level value, or None for builtins."""
    if value is None:
        return None
    cls = type(value)
    module = getattr(cls, "__module__", None) or ""
    if module == "builtins":
        return None
    return f"{module}.{cls.__qualname__}"


def encode(value: Any) -> Any:
    """JSON-safe copy of `value`. Never raises."""
    try:
        return _Encoder().walk(value, 0, "$")
    except Exception as e:  # defensive: the caller must never see an encoder error
        return f"[encode failed: {type(e).__name__}]"


def _safe_str(x: Any) -> str:
    try:
        return str(x)
    except Exception:
        return "<unprintable>"


def _safe_getattr(obj: Any, name: str) -> Any:
    try:
        return getattr(obj, name)
    except Exception as e:
        return f"[threw: {type(e).__name__}]"


def _safe_repr(v: Any) -> str:
    try:
        text = repr(v)
    except Exception:
        text = f"<{type(v).__qualname__}>"
    return text if len(text) <= MAX_STRING else text[:MAX_STRING] + "…"


def _is_namedtuple(v: Any) -> bool:
    return isinstance(v, tuple) and hasattr(type(v), "_fields")


def _object_fields(v: Any) -> Any:
    """Instance attributes plus __slots__ values, or None when the object has neither."""
    fields: Dict[str, Any] = {}
    found = False
    try:
        d = vars(v)
    except Exception:
        d = None
    if isinstance(d, dict):
        fields.update(d)
        found = True
    for cls in type(v).__mro__:
        slots = cls.__dict__.get("__slots__", ())
        if isinstance(slots, str):
            slots = (slots,)
        for name in slots:
            if name in ("__dict__", "__weakref__"):
                continue
            found = True
            try:
                fields[name] = getattr(v, name)
            except AttributeError:
                pass
            except Exception as e:
                fields[name] = f"[threw: {type(e).__name__}]"
    return fields if found else None


class _Encoder:
    def __init__(self) -> None:
        self.ancestors: Dict[int, str] = {}  # id -> JSON path, only along the current branch

    def child(self, x: Any, depth: int, path: str) -> Any:
        try:
            return self.walk(x, depth, path)
        except Exception as e:
            return f"[threw: {type(e).__name__}]"

    def walk(self, v: Any, depth: int, path: str) -> Any:
        if v is None or isinstance(v, bool):
            return v
        if isinstance(v, enum.Enum):
            if depth >= MAX_DEPTH:
                return {"$truncated": True}
            return {"name": v.name, "value": self.child(v.value, depth + 1, path + ".value")}
        if isinstance(v, str):
            return v if len(v) <= MAX_STRING else v[:MAX_STRING] + "…"
        if isinstance(v, int):
            return int(v)
        if isinstance(v, float):
            if math.isnan(v):
                return "NaN"
            if math.isinf(v):
                return "Infinity" if v > 0 else "-Infinity"
            return v
        if isinstance(v, (bytes, bytearray, memoryview)):
            raw = bytes(v)[:MAX_STRING]
            return {"$bin": base64.b64encode(raw).decode("ascii")}
        if isinstance(v, (datetime.datetime, datetime.date, datetime.time)):
            return v.isoformat()
        if isinstance(v, (decimal.Decimal, uuid.UUID, pathlib.PurePath)):
            return str(v)
        if isinstance(v, (types.FunctionType, types.BuiltinFunctionType, types.MethodType, types.ModuleType, type)):
            if isinstance(v, type):
                kind = "class"
            elif isinstance(v, types.ModuleType):
                kind = "module"
            else:
                kind = "function"
            name = _safe_str(getattr(v, "__qualname__", None) or getattr(v, "__name__", "anonymous"))
            return f"[{kind} {name}]"

        # Containers and objects from here on.
        key = id(v)
        if key in self.ancestors:
            return {"$ref": self.ancestors[key]}
        if depth >= MAX_DEPTH:
            return {"$truncated": True}

        if isinstance(v, dict):
            return self.mapping(v, v, depth, path)
        if _is_namedtuple(v):
            return self.mapping(v, dict(zip(v._fields, v)), depth, path)
        if isinstance(v, (list, tuple, set, frozenset)):
            self.ancestors[key] = path
            try:
                items = list(v)
                out: list = []
                for i, x in enumerate(items):
                    if i >= MAX_ITEMS:
                        out.append({"$truncated": True, "more": len(items) - MAX_ITEMS})
                        break
                    out.append(self.child(x, depth + 1, f"{path}[{i}]"))
                return out
            finally:
                self.ancestors.pop(key, None)
        if dataclasses.is_dataclass(v) and not isinstance(v, type):
            fields = {f.name: _safe_getattr(v, f.name) for f in dataclasses.fields(v)}
            return self.mapping(v, fields, depth, path)
        fields = _object_fields(v)
        if fields is not None:
            if isinstance(v, BaseException) and "message" not in fields:
                fields["message"] = _safe_str(v)
            return self.mapping(v, fields, depth, path)
        return _safe_repr(v)

    def mapping(self, owner: Any, fields: Dict[Any, Any], depth: int, path: str) -> Any:
        key = id(owner)
        self.ancestors[key] = path
        try:
            out: Dict[str, Any] = {}
            for i, (k, x) in enumerate(fields.items()):
                if i >= MAX_ITEMS:
                    out["$truncated"] = True
                    break
                name = k if isinstance(k, str) else _safe_str(k)
                out[name] = self.child(x, depth + 1, f"{path}.{name}")
            return out
        finally:
            self.ancestors.pop(key, None)
