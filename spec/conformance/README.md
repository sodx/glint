# Glint SDK conformance, v1

A language SDK is **supported** when its conformance program passes:

```bash
glint conformance -- <command that runs the program>
# e.g.
glint conformance -- node sdks/js/conformance.mjs
glint conformance -- python3 sdks/python/conformance.py
glint conformance -- php sdks/php/conformance.php
```

The runner starts an in-memory Glint server on a random port and runs the program with
`GLINT_URL` pointing at it. It then checks what arrived against `scenarios.json`. The runner runs the
program twice more to check that the SDK behaves when Glint is unreachable and when it is
disabled. Exit code 0 means every check passed.

## SDK surface

Names follow each language's conventions. Behaviour must match. "Entry" is the handle a call
returns. Its methods change how the events it sent are shown, by sending `core.update`.

| Call | Sends |
|------|-------|
| `glint(v1, v2, …)` | one `core.value` per value: `{"value": <encoded v>, "type"?: <source type name>}`. Returns an Entry for all of them. |
| `glint.log(message, fields?, level?)` | `core.log {message, fields?}`, `meta.level` (default `info`) |
| `glint.table(rows, columns?)` | `core.table {rows, columns?}` |
| `glint(error)` | same as `glint.exception(error)`: an error passed as a value is an exception |
| `glint.measure(name, fn)` nesting | what `fn` sends has the measure as `parent` (Node: across awaits) |
| `glint.pause(reason)` | sends `ctl.pause`, waits (long-poll `GET /v1/ctl/{id}`) and returns the value given with Continue. A Stop raises or throws `GlintStop`. It returns at once when Glint is off, unreachable or on CI. |
| `glint.exception(error)` | `core.exception {class, message, frames: [{file, line, fn?, class?, vendor?}], cause?}` |
| `glint.measure(name, fn)` | runs `fn`, sends `core.measure {name, duration_ms}`, returns `fn`'s result |
| `glint.send(kind, payload, meta?)` | any kind, unchanged; returns an Entry |
| `glint.flush(timeout?)` | blocks until queued events are sent or the timeout passes |
| Entry `.label(s)` `.color(c)` | `core.update {target, meta: {label}}` / `{meta: {color}}` |
| Entry `.red()` `.orange()` `.green()` `.blue()` `.purple()` `.gray()` | shorthand for `.color()` |
| Entry `.small()` `.large()` | `meta.size` `sm` / `lg` |
| Entry `.hide()` `.remove()` | `core.update {target, hidden: true}` / `{removed: true}` |

Requirements (`docs/PROTOCOL.md` §8):

1. Every event has a client-assigned `id` (unique; ULID recommended) so an Entry can target it.
2. Every event has `origin.lang`, `origin.sdk` (`<name>/<semver>`), and the **user's** call site
   in `origin.file` / `origin.line`. It is never a frame inside the SDK.
3. One `run` per process, shared by all its events.
4. Values are encoded faithfully. Objects keep their fields and report their type name in
   `payload.type`. Cycles become `{"$ref": "<path>"}`. NaN and ±Infinity become `"NaN"`,
   `"Infinity"` and `"-Infinity"`. Bytes become `{"$bin": "<base64>"}`. Anything deeper than the depth
   limit (default 10) becomes `{"$truncated": true}`.
5. Never throws into, crashes or noticeably slows the host program. If Glint is unreachable, the SDK gives up
   quickly (connect timeout ≤ 1 s), stops trying for a while, and lets the program exit normally.
6. Delivers events queued before a normal exit (flush on exit, bounded by a timeout).
7. `GLINT_ENABLED=0` disables the SDK entirely: no network calls, no output. It is also disabled by
   default in the ecosystem's production mode (`NODE_ENV=production`, `APP_ENV=production`, …)
   unless `GLINT_ENABLED=1`.
8. `GLINT_URL` (default `http://127.0.0.1:23600`) selects the server.

## The conformance program

It runs every scenario in `scenarios.json`, in file order. Before each one, it sends a marker:

```
glint.send("x-conformance.begin", {"scenario": "<name>"})
```

It then performs the scenario's `do` steps with the SDK. The program must exit 0. Each
scenario's `do` field says exactly what to call. `expect` is what the runner checks.

## How expectations match

- Events between two markers belong to the first marker's scenario. `core.update` events are
  not compared directly. They are folded into the effective `meta` (plus `hidden` and `removed`)
  of their target, which is what `expect` describes.
- Expected objects match if every expected key matches. Extra keys are fine. Arrays match
  element by element and must have the same length.
- Placeholders: `"$any"`, `"$string"` (non-empty), `"$int"`, `"$number"`, `"$bool"`,
  `"$gte:<n>"`, `"$contains:<text>"`, `"$has-key-deep:<key>"` (the key appears anywhere inside),
  `"$nonempty-array"`.
- Global checks on every event: same non-empty `run`, unique `id`, `origin.lang` set,
  `origin.sdk` matches `^[A-Za-z0-9@._/-]+/\d+\.\d+\.\d+`, and every `core.update` targets an id that
  was sent.
- The `origin` scenario also reads the line `origin.file:origin.line` from disk and
  requires it to contain the text `glint:origin`. Put that marker in a comment on the line of
  the call.
