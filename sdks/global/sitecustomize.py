# Written by `glint docker`: glint() as a builtin, and uncaught exceptions reported, in Python
# inside a container or on a remote machine. PYTHONPATH points at this folder, so Python runs this
# file at start; nothing else is imported until glint is used.
try:
    import glint_builtin  # noqa: F401
except Exception:
    pass


# This file hides an image's own sitecustomize.py; run that one too.
def _chain():
    import os
    import sys

    here = os.path.dirname(os.path.abspath(__file__))
    for entry in sys.path:
        d = os.path.abspath(entry or ".")
        if d == here:
            continue
        f = os.path.join(d, "sitecustomize.py")
        if os.path.isfile(f):
            with open(f, encoding="utf-8") as src:
                code = compile(src.read(), f, "exec")
            exec(code, {"__name__": "sitecustomize", "__file__": f})
            return


try:
    _chain()
except Exception:
    pass
del _chain
