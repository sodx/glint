# Written by `glint install`: glint() as a builtin in every Python on this machine, and uncaught
# exceptions reported to Glint without any code. Off in production like the SDK.
# Lazy: nothing is imported until glint is used, and a project's own `glint` package wins.
import builtins as _b
import sys as _sys

_SDK = "__GLINT_SDK__"


def _load():
    try:
        import glint as m
    except ImportError:
        if _SDK not in _sys.path:
            _sys.path.append(_SDK)
        import glint as m
    try:  # this file is not the caller: report the line that called glint()
        o = m._origin
        inner = o._in_package
        o._in_package = lambda f: f == __file__ or inner(f)
    except Exception:
        pass
    _b.glint = m.glint
    return m.glint


class _Lazy:
    def __call__(self, *a, **k):
        return _load()(*a, **k)

    def __getattr__(self, name):
        return getattr(_load(), name)

    def __repr__(self):
        return "<glint (loads on first use)>"


if not hasattr(_b, "glint"):
    _b.glint = _Lazy()


# Uncaught exceptions, in the main thread and in other threads: report, then let Python print the
# traceback and exit exactly as before. Ctrl-C and sys.exit() are not failures.
def _report(exc):
    if isinstance(exc, (KeyboardInterrupt, SystemExit)):
        return
    try:
        g = _load()
        g.exception(exc).label("uncaught")
        g.flush(1.5)
    except Exception:
        pass


_prev_hook = _sys.excepthook


def _excepthook(t, v, tb):
    _report(v)
    _prev_hook(t, v, tb)


_sys.excepthook = _excepthook

try:
    import threading as _threading

    _prev_thread_hook = _threading.excepthook

    def _thread_hook(args):
        _report(args.exc_value)
        _prev_thread_hook(args)

    _threading.excepthook = _thread_hook
except Exception:
    pass


# Frameworks, with no code: when Django, Flask, FastAPI/Starlette, SQLAlchemy, requests or httpx is
# imported, Glint hooks in (glint/_frameworks.py): a run per request, SQL with the app line behind
# it, exceptions with their request, failed outgoing calls. Other imports pass straight through.
_WATCH = ("django.core.handlers.base", "flask", "starlette.applications", "sqlalchemy", "requests", "httpx")


class _Loader:
    def __init__(self, loader, name):
        self._loader = loader
        self._name = name

    def create_module(self, spec):
        return self._loader.create_module(spec)

    def exec_module(self, module):
        self._loader.exec_module(module)
        try:
            _load()
            from glint import _frameworks

            _frameworks.patch(self._name, module)
        except Exception:
            pass

    def __getattr__(self, name):
        return getattr(self._loader, name)


class _Finder:
    _busy = set()

    def find_spec(self, name, path=None, target=None):
        if name not in _WATCH or name in self._busy:
            return None
        self._busy.add(name)
        try:
            for finder in _sys.meta_path:
                if finder is self or not hasattr(finder, "find_spec"):
                    continue
                spec = finder.find_spec(name, path, target)
                if spec is not None:
                    if spec.loader is not None and hasattr(spec.loader, "exec_module"):
                        spec.loader = _Loader(spec.loader, name)
                    return spec
            return None
        except Exception:
            return None
        finally:
            self._busy.discard(name)


try:
    import os as _os

    if _os.environ.get("GLINT_FRAMEWORKS", "").strip().lower() not in ("0", "off", "false", "no"):
        _sys.meta_path.insert(0, _Finder())
except Exception:
    pass
