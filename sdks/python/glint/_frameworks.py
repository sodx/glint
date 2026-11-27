"""Framework integrations, with no code: the global shim (glint_builtin.py) calls patch() when a
framework module is imported, and this module hooks in.

- Django: each request (sync and async) is a run with one root event; SQL of every connection
  through execute_wrappers; exceptions through got_request_exception; logging records.
- Flask: each request through Flask.wsgi_app; unhandled exceptions; logging records.
- FastAPI and Starlette: each HTTP request through Starlette.__call__; unhandled exceptions.
- SQLAlchemy: every statement of every Engine (Flask-SQLAlchemy, SQLModel, plain SQLAlchemy).
- requests and httpx: failed outgoing calls (connection errors, 4xx and 5xx).

The root event of a request is a core.measure named like "GET /orders/<id> → 500 · shop.views.order"
with the method, route, view, status, time, memory and number of queries; everything sent while
the request runs nests under it. GLINT_FRAMEWORKS=off turns all of it off.
"""

from __future__ import annotations

import contextvars
import logging
import os
import sys
import time
from typing import Any, Callable, Dict, List, Optional, Tuple

from . import _config, _encode, _ids, _origin

_SHIM = os.path.join("python-shim", "glint_builtin.py")


def _glint():
    import glint  # the package this module belongs to

    return glint


class _Req:
    __slots__ = ("framework", "method", "path", "url", "root", "run", "t0", "queries", "sql_ms", "route", "view", "func", "tokens", "reported")

    def __init__(self, framework: str, method: str, path: str, url: str) -> None:
        self.framework = framework
        self.method = method
        self.path = path
        self.url = url
        self.root = _ids.ulid()
        self.run = "python-" + self.root
        self.t0 = time.perf_counter()
        self.queries = 0
        self.sql_ms = 0.0
        self.route: Optional[str] = None
        self.view: Optional[str] = None
        self.func: Any = None
        self.tokens: Tuple[Any, ...] = ()
        self.reported: set = set()


_REQ: "contextvars.ContextVar[Optional[_Req]]" = contextvars.ContextVar("glint_request", default=None)
_reported_ids: List[int] = []  # exceptions reported outside a request, newest last


def _on() -> bool:
    v = os.environ.get("GLINT_FRAMEWORKS", "").strip().lower()
    return _config.settings.enabled and v not in ("0", "off", "false", "no")


def _emit(kind: str, payload: Any, meta: Optional[Dict[str, Any]] = None, site: Optional[Dict[str, Any]] = None, event_id: Optional[str] = None) -> Optional[str]:
    return _glint()._emit(kind, payload, meta, site if site is not None else app_site(), event_id)


def _is_ours(filename: str) -> bool:
    return _origin._in_package(filename) or filename.endswith(_SHIM) or filename.endswith("glint_builtin.py")


def app_site() -> Dict[str, Any]:
    """The first frame of the app's own code: outside site-packages, the stdlib and Glint."""
    frame = sys._getframe(1)
    while frame is not None:
        name = frame.f_code.co_filename
        if not _is_ours(name) and not _origin._is_vendor(os.path.abspath(name)) and not name.startswith("<"):
            return {"file": os.path.abspath(name), "line": frame.f_lineno, "fn": frame.f_code.co_name}
        frame = frame.f_back
    return {}


def _func_site(func: Any) -> Dict[str, Any]:
    try:
        while hasattr(func, "__wrapped__"):
            func = func.__wrapped__
        func = getattr(func, "view_class", func)
        code = getattr(func, "__code__", None)
        if code is None and hasattr(func, "__call__"):
            code = getattr(func.__call__, "__code__", None)
        if code is not None:
            return {"file": os.path.abspath(code.co_filename), "line": code.co_firstlineno, "fn": getattr(func, "__qualname__", code.co_name)}
    except Exception:
        pass
    return {}


def _func_name(func: Any) -> Optional[str]:
    try:
        while hasattr(func, "__wrapped__"):
            func = func.__wrapped__
        return f"{func.__module__}.{func.__qualname__}"
    except Exception:
        return None


# ---------------------------------------------------------------- requests: begin and end


def begin(framework: str, method: str, path: str, url: str) -> Optional[_Req]:
    if not _on() or _REQ.get() is not None:
        return None
    try:
        g = _glint()
        req = _Req(framework, method, path, url)
        req.tokens = (g._RUN.set(req.run), g._PARENT.set(req.root), _REQ.set(req))
        _attach_logging()
        return req
    except Exception:
        return None


def end(req: Optional[_Req], status: Optional[int]) -> None:
    if req is None:
        return
    g = _glint()
    run_token, parent_token, req_token = req.tokens
    try:
        g._PARENT.reset(parent_token)
    except Exception:
        pass
    try:
        route = req.route or req.path
        label = f"{req.method} {route}" + (f" → {status}" if status else "") + (f" · {req.view}" if req.view else "")
        payload: Dict[str, Any] = {
            "name": label,
            "duration_ms": round((time.perf_counter() - req.t0) * 1000, 1),
            "method": req.method,
            "url": req.url,
            "route": req.route,
            "view": req.view,
            "status": status,
            "queries": req.queries,
            "sql_ms": round(req.sql_ms, 2),
        }
        mem = _peak_memory()
        if mem:
            payload["memory_bytes"] = mem
        meta: Dict[str, Any] = {"label": req.framework}
        if status is not None and status >= 500:
            meta["level"] = "error"
        site = _func_site(req.func) if req.func is not None else {}
        _emit("core.measure", {k: v for k, v in payload.items() if v is not None}, meta, site, req.root)
    except Exception:
        pass
    for var, token in ((g._RUN, run_token), (_REQ, req_token)):
        try:
            var.reset(token)
        except Exception:
            pass


def _peak_memory() -> Optional[int]:
    try:
        import resource

        rss = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
        return int(rss if sys.platform == "darwin" else rss * 1024)
    except Exception:
        return None


def report(exc: BaseException, extra: Optional[Dict[str, Any]] = None) -> None:
    """Sends exc once, with the request it happened in."""
    if not _on() or exc is None:
        return
    req = _REQ.get()
    seen = req.reported if req is not None else _reported_ids
    if id(exc) in seen:
        return
    if req is not None:
        req.reported.add(id(exc))
    else:
        _reported_ids.append(id(exc))
        del _reported_ids[:-50]
    try:
        g = _glint()
        payload = g._exception_payload(exc, None)
        ctx: Dict[str, Any] = {}
        if req is not None:
            ctx = {"method": req.method, "url": req.url, "route": req.route, "view": req.view}
        if extra:
            ctx.update(extra)
        ctx = {k: v for k, v in ctx.items() if v is not None}
        if ctx:
            payload["context"] = _encode.encode(ctx)
        site: Dict[str, Any] = {}
        for f in payload.get("frames", []):
            if not f.get("vendor") and f.get("file") and not _is_ours(f["file"]):
                site = {"file": f["file"], "line": f.get("line"), "fn": f.get("fn")}
                break
        _emit("core.exception", payload, {"level": "error", "label": req.framework if req else "uncaught"}, site)
    except Exception:
        pass


def _already_reported(exc: Any) -> bool:
    req = _REQ.get()
    return id(exc) in (req.reported if req is not None else _reported_ids)


# ---------------------------------------------------------------- logging


class _Handler(logging.Handler):
    """Log records of the app (WARNING and up from the frameworks' own loggers) as core.log."""

    _LEVELS = {logging.DEBUG: "debug", logging.INFO: "info", logging.WARNING: "warn", logging.ERROR: "error", logging.CRITICAL: "fatal"}
    _QUIET = ("django.server", "werkzeug", "uvicorn.access", "glint", "urllib3", "httpx", "httpcore", "asyncio")

    def emit(self, record: logging.LogRecord) -> None:
        try:
            name = record.name or ""
            if name.startswith(self._QUIET):
                return
            if name.startswith(("django", "flask", "uvicorn", "fastapi", "starlette", "sqlalchemy")) and record.levelno < logging.WARNING:
                return
            exc = record.exc_info[1] if record.exc_info else None
            if exc is not None:
                if _already_reported(exc):
                    return  # the framework logs what it reported: one event, not two
                report(exc)
                return
            payload: Dict[str, Any] = {"message": record.getMessage(), "logger": name}
            site = {"file": os.path.abspath(record.pathname), "line": record.lineno, "fn": record.funcName} if record.pathname else {}
            level = self._LEVELS.get(record.levelno, "info")
            _emit("core.log", payload, {"level": level, "label": name}, site)
        except Exception:
            pass


_logging_attached = False


def _attach_logging() -> None:
    global _logging_attached
    if _logging_attached:
        return
    _logging_attached = True
    try:
        root = logging.getLogger()
        if not any(isinstance(h, _Handler) for h in root.handlers):
            root.addHandler(_Handler(logging.INFO))
    except Exception:
        pass


# ---------------------------------------------------------------- SQL


def _sql(sql: Any, params: Any, conn: str, duration_ms: float, rows: Any, error: Optional[BaseException], site: Dict[str, Any]) -> None:
    req = _REQ.get()
    if req is not None:
        req.queries += 1
        req.sql_ms += duration_ms
    payload: Dict[str, Any] = {"sql": str(sql), "duration_ms": round(duration_ms, 3), "conn": conn}
    if params:
        try:
            payload["bindings"] = _encode.encode(list(params) if isinstance(params, (list, tuple)) else params)
        except Exception:
            pass
    if isinstance(rows, int) and rows >= 0:
        payload["rows"] = rows
    meta = None
    if error is not None:
        payload["error"] = f"{type(error).__name__}: {error}"
        meta = {"level": "error"}
    _emit("sql.query", payload, meta, site)


# ---------------------------------------------------------------- Django


def _django_handler(mod: Any) -> None:
    BaseHandler = mod.BaseHandler
    if getattr(BaseHandler, "_glint", False):
        return
    BaseHandler._glint = True
    sync, async_ = BaseHandler.get_response, getattr(BaseHandler, "get_response_async", None)

    def start(request: Any) -> Optional[_Req]:
        try:
            url = request.build_absolute_uri()
        except Exception:
            url = request.get_full_path()
        return begin("Django", request.method or "GET", request.path, url)

    def finish(req: Optional[_Req], request: Any, response: Any) -> None:
        if req is None:
            return
        try:
            rm = getattr(request, "resolver_match", None)
            if rm is not None:
                req.route = "/" + rm.route if getattr(rm, "route", None) else None
                req.view = getattr(rm, "_func_path", None) or rm.view_name
                req.func = rm.func
        except Exception:
            pass
        end(req, getattr(response, "status_code", None))

    def get_response(self: Any, request: Any) -> Any:
        req = start(request)
        response = None
        try:
            response = sync(self, request)
            return response
        finally:
            finish(req, request, response)

    BaseHandler.get_response = get_response
    if async_ is not None:

        async def get_response_async(self: Any, request: Any) -> Any:
            req = start(request)
            response = None
            try:
                response = await async_(self, request)
                return response
            finally:
                finish(req, request, response)

        BaseHandler.get_response_async = get_response_async

    from django.core import signals as core_signals
    from django.db.backends import signals as db_signals

    def on_exception(sender: Any, request: Any = None, **kw: Any) -> None:
        exc = sys.exc_info()[1]
        if exc is not None:
            report(exc)

    def on_connection(sender: Any, connection: Any, **kw: Any) -> None:
        wrappers = getattr(connection, "execute_wrappers", None)
        if isinstance(wrappers, list) and _django_sql not in wrappers:
            wrappers.append(_django_sql)

    core_signals.got_request_exception.connect(on_exception, weak=False, dispatch_uid="glint")
    db_signals.connection_created.connect(on_connection, weak=False, dispatch_uid="glint")


def _django_sql(execute: Callable, sql: Any, params: Any, many: bool, context: Dict[str, Any]) -> Any:
    if not _on():
        return execute(sql, params, many, context)
    site = app_site()
    t0 = time.perf_counter()
    error = None
    try:
        return execute(sql, params, many, context)
    except Exception as e:
        error = e
        raise
    finally:
        try:
            cursor = context.get("cursor")
            conn = context.get("connection")
            _sql(sql, None if many else params, getattr(conn, "alias", "default"), (time.perf_counter() - t0) * 1000, getattr(cursor, "rowcount", None), error, site)
        except Exception:
            pass


# ---------------------------------------------------------------- Flask


def _flask(mod: Any) -> None:
    Flask = mod.Flask
    if getattr(Flask, "_glint", False):
        return
    Flask._glint = True
    wsgi_app, full_dispatch, handle_exception = Flask.wsgi_app, Flask.full_dispatch_request, Flask.handle_exception

    def patched_wsgi_app(self: Any, environ: Dict[str, Any], start_response: Callable) -> Any:
        path = environ.get("PATH_INFO", "/")
        query = environ.get("QUERY_STRING")
        url = f"{environ.get('wsgi.url_scheme', 'http')}://{environ.get('HTTP_HOST', '')}{path}" + (f"?{query}" if query else "")
        req = begin("Flask", environ.get("REQUEST_METHOD", "GET"), path, url)
        if req is None:
            return wsgi_app(self, environ, start_response)
        status: List[Optional[int]] = [None]

        def sr(st: str, headers: Any, exc_info: Any = None) -> Any:
            try:
                status[0] = int(st.split(" ", 1)[0])
            except Exception:
                pass
            return start_response(st, headers, exc_info) if exc_info is not None else start_response(st, headers)

        try:
            return wsgi_app(self, environ, sr)
        finally:
            end(req, status[0])

    def patched_full_dispatch(self: Any) -> Any:
        try:
            from flask import request

            req = _REQ.get()
            if req is not None and request.url_rule is not None:
                req.route = request.url_rule.rule
                req.view = request.endpoint
                req.func = self.view_functions.get(request.endpoint)
        except Exception:
            pass
        return full_dispatch(self)

    def patched_handle_exception(self: Any, e: BaseException) -> Any:
        report(e)
        return handle_exception(self, e)

    Flask.wsgi_app = patched_wsgi_app
    Flask.full_dispatch_request = patched_full_dispatch
    Flask.handle_exception = patched_handle_exception


# ---------------------------------------------------------------- FastAPI and Starlette


def _starlette(mod: Any) -> None:
    Starlette = mod.Starlette
    if getattr(Starlette, "_glint", False):
        return
    Starlette._glint = True
    call = Starlette.__call__

    async def patched_call(self: Any, scope: Dict[str, Any], receive: Callable, send: Callable) -> None:
        if scope.get("type") != "http" or _REQ.get() is not None:
            return await call(self, scope, receive, send)
        path = scope.get("path", "/")
        query = scope.get("query_string", b"").decode("latin-1")
        host = next((v.decode("latin-1") for k, v in scope.get("headers", []) if k == b"host"), "")
        url = f"{scope.get('scheme', 'http')}://{host}{path}" + (f"?{query}" if query else "")
        framework = "FastAPI" if any(c.__name__ == "FastAPI" for c in type(self).__mro__) else "Starlette"
        req = begin(framework, scope.get("method", "GET"), path, url)
        if req is None:
            return await call(self, scope, receive, send)
        status: List[Optional[int]] = [None]

        async def snd(message: Dict[str, Any]) -> None:
            if message.get("type") == "http.response.start":
                status[0] = message.get("status")
            await send(message)

        try:
            await call(self, scope, receive, snd)
        except Exception as e:
            report(e)
            if status[0] is None:
                status[0] = 500
            raise
        finally:
            try:
                route = scope.get("route")
                req.route = getattr(route, "path", None) or getattr(route, "path_format", None)
                endpoint = scope.get("endpoint") or getattr(route, "endpoint", None)
                if endpoint is not None:
                    req.view = _func_name(endpoint)
                    req.func = endpoint
            except Exception:
                pass
            end(req, status[0])

    Starlette.__call__ = patched_call


# ---------------------------------------------------------------- SQLAlchemy


def _sqlalchemy(mod: Any) -> None:
    from sqlalchemy import event
    from sqlalchemy.engine import Engine

    if getattr(Engine, "_glint", False):
        return
    Engine._glint = True

    def before(conn: Any, cursor: Any, statement: Any, parameters: Any, context: Any, executemany: bool) -> None:
        if context is not None:
            context._glint = (time.perf_counter(), app_site())

    def after(conn: Any, cursor: Any, statement: Any, parameters: Any, context: Any, executemany: bool) -> None:
        if not _on():
            return
        t0, site = getattr(context, "_glint", (time.perf_counter(), {}))
        _sql(statement, None if executemany else parameters, _sa_name(conn), (time.perf_counter() - t0) * 1000, getattr(cursor, "rowcount", None), None, site)

    def failed(exc_context: Any) -> None:
        if not _on():
            return
        ctx = getattr(exc_context, "execution_context", None)
        t0, site = getattr(ctx, "_glint", (time.perf_counter(), app_site()))
        _sql(exc_context.statement or "", exc_context.parameters, _sa_name(exc_context.connection), (time.perf_counter() - t0) * 1000, None, exc_context.original_exception, site)

    event.listen(Engine, "before_cursor_execute", before)
    event.listen(Engine, "after_cursor_execute", after)
    event.listen(Engine, "handle_error", failed)


def _sa_name(conn: Any) -> str:
    try:
        return conn.engine.url.get_backend_name()
    except Exception:
        return "sqlalchemy"


# ---------------------------------------------------------------- outgoing HTTP


def _http_failed(method: str, url: str, status: Optional[int], error: Optional[BaseException], duration_ms: float, body: Optional[str] = None) -> None:
    if not _on():
        return
    try:
        if url.startswith(_config.settings.url.rstrip("/")):
            return  # Glint's own traffic
        payload: Dict[str, Any] = {"method": method.upper(), "url": url, "duration_ms": round(duration_ms, 1)}
        if status is not None:
            payload["status"] = status
        if error is not None:
            payload["error"] = f"{type(error).__name__}: {error}"
        if body:
            payload["body"] = body[:2000]
        level = "error" if error is not None or (status or 0) >= 500 else "warn"
        _emit("http.request", payload, {"level": level, "label": "HTTP client"})
    except Exception:
        pass


def _requests(mod: Any) -> None:
    Session = mod.Session
    if getattr(Session, "_glint", False):
        return
    Session._glint = True
    send = Session.send

    def patched_send(self: Any, request: Any, **kw: Any) -> Any:
        t0 = time.perf_counter()
        try:
            res = send(self, request, **kw)
        except Exception as e:
            _http_failed(request.method or "GET", request.url or "", None, e, (time.perf_counter() - t0) * 1000)
            raise
        if res.status_code >= 400:
            body = None
            try:
                if not kw.get("stream"):
                    body = res.text
            except Exception:
                pass
            _http_failed(request.method or "GET", request.url or "", res.status_code, None, (time.perf_counter() - t0) * 1000, body)
        return res

    Session.send = patched_send


def _httpx(mod: Any) -> None:
    Client, AsyncClient = mod.Client, mod.AsyncClient
    if getattr(Client, "_glint", False):
        return
    Client._glint = True
    send, asend = Client.send, AsyncClient.send

    def patched_send(self: Any, request: Any, **kw: Any) -> Any:
        t0 = time.perf_counter()
        try:
            res = send(self, request, **kw)
        except Exception as e:
            _http_failed(request.method, str(request.url), None, e, (time.perf_counter() - t0) * 1000)
            raise
        if res.status_code >= 400:
            _http_failed(request.method, str(request.url), res.status_code, None, (time.perf_counter() - t0) * 1000)
        return res

    async def patched_asend(self: Any, request: Any, **kw: Any) -> Any:
        t0 = time.perf_counter()
        try:
            res = await asend(self, request, **kw)
        except Exception as e:
            _http_failed(request.method, str(request.url), None, e, (time.perf_counter() - t0) * 1000)
            raise
        if res.status_code >= 400:
            _http_failed(request.method, str(request.url), res.status_code, None, (time.perf_counter() - t0) * 1000)
        return res

    Client.send = patched_send
    AsyncClient.send = patched_asend


# ---------------------------------------------------------------- entry point

WATCH: Dict[str, Callable[[Any], None]] = {
    "django.core.handlers.base": _django_handler,
    "flask": _flask,
    "starlette.applications": _starlette,
    "sqlalchemy": _sqlalchemy,
    "requests": _requests,
    "httpx": _httpx,
}


def patch(name: str, module: Any) -> None:
    """Called once a watched module has been imported. Never raises."""
    fn = WATCH.get(name)
    if fn is None or not _on():
        return
    try:
        fn(module)
    except Exception:
        pass
