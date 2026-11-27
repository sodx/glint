# glint-sdk (Python)

Send values, logs, tables, exceptions and timings from a Python program to a local Glint debug
desk. Standard library only, Python 3.9+.

## Install

```bash
pip install ./sdks/python        # from a checkout
```

## Usage

```python
from glint import glint

order = {"id": 7, "items": ["A-1", "B-7"]}
glint(order).label("before save").orange()     # one value; returns an Entry
glint(1, 2).label("pair")                      # several values, one call

glint.log("user signed in", {"user_id": 7}, "warn")
glint.table([{"sku": "A-1", "qty": 2}, {"sku": "B-7", "qty": 1}])

try:
    charge()
except Exception as err:
    glint.exception(err)

total = glint.measure("pricing", compute_total)   # returns compute_total()'s result
glint.send("x-myapp.event", {"a": 1}, {"label": "raw"})

glint.flush(timeout=2.0)   # optional: wait for queued events (exit already does this)
```

Entry methods change how the event is shown (they send `core.update`): `label`, `color`
(or `gray`, `green`, `orange`, `red`, `blue`, `purple`), `small`, `large`, `hide`, `remove`.

Values are encoded faithfully: objects keep their fields and report their class in `type`
(for example `__main__.Order`); dataclasses, named tuples, enums, `bytes` (as base64), dates,
`Decimal`/`UUID`/`Path`, and cycles (`{"$ref": ...}`) are handled. Non-finite floats become
`"NaN"` / `"Infinity"` / `"-Infinity"`. Depth beyond 10 becomes `{"$truncated": true}`.

`measure` also works with coroutine functions: it returns an awaitable that is timed when awaited.
What the callback sends nests under the measure (a ContextVar carries it through threads' contexts
and asyncio tasks), so the window shows the run as a call tree.

## Pause

```python
from glint import glint, GlintStop

answer = glint.pause(f"refund {order.total} EUR?")
if answer and answer.get("approve"):
    refund(order)
```

`pause` blocks until someone presses Continue in the Glint window, or a coding agent answers through
MCP, and returns the value they gave (or `None`). Stop raises `GlintStop`, also reachable as
`glint.GlintStop` from the global. It returns at once when Glint is off or unreachable, and on CI
unless `GLINT_FORCE_PAUSE` is set. After 5 minutes it gives up and carries on.

## Configuration

| Variable | Meaning |
|----------|---------|
| `GLINT_URL` | Glint server, default `http://127.0.0.1:23600`; in a container `http://host.docker.internal:23600` |
| `GLINT_ENABLED` | `0`/`false`/`off` disables; `1`/`true`/`on` enables |
| `GLINT_PROJECT` | optional project name attached to every event |
| `GLINT_PATH_MAP` | `/app:/Users/me/shop`: container path to host path, so files open in your editor |

Without `GLINT_ENABLED`, the SDK is **off** when any of `APP_ENV`, `ENV`, `PYTHON_ENV` or
`DJANGO_ENV` is `production`, and on otherwise. When disabled, no thread starts and nothing is
sent.

Or in code: `glint.configure(url=..., enabled=..., project=...)`.

## When Glint is down

Calls never raise and never wait on the network. Events go into a bounded queue (10 000); a
background thread posts them in batches of up to 500. If a post fails, the SDK stops trying for
5 seconds and drops events meanwhile. The number dropped is reported in `meta.dropped` of the
next delivered event. A single warning is written to stderr. At exit, the SDK waits up to 2 s
to deliver what is queued; with Glint unreachable the program still exits immediately.

## Development

```bash
cd <repo>
glint conformance -- python3 sdks/python/conformance.py   # expects 20/20 passed
```
