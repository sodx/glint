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
