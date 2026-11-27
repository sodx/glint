# glint-sdk (JavaScript / TypeScript)

Send values, logs, exceptions and timings from Node (≥ 18) or the browser to a local
[Glint](../../README.md) daemon. No dependencies, no build step, types included.

```bash
npm install /path/to/glint/sdks/js     # not published yet
```

```js
import glint from "glint-sdk";

glint(order).label("before save").orange();       // any value, structured
glint(user, req.headers);                         // several values, one event each
glint.log("payment retry", { attempt: 2 }, "warn");
glint.table(rows);
glint.exception(err);                             // class, message, frames, cause
const total = glint.measure("price", () => calc()); // sync or async; returns the result
glint.send("x-shop.cart", { items: 3 });          // any kind
await glint.flush();                              // before process.exit(), if you call it
```

Each call returns an Entry; `.label()`, `.color()` / `.red()` … `.gray()`, `.small()` / `.large()`,
`.hide()` and `.remove()` change how the events are shown.

## Pause

```js
const answer = await glint.pause(`refund ${order.total} EUR?`);
if (answer?.approve) await refund(order);
```

`pause` stops here until someone presses Continue in the Glint window, or a coding agent answers
through MCP, and resolves to the value they gave (or `undefined`). Stop rejects with `GlintStop`.
It resolves at once when Glint is off or unreachable, and on CI unless `GLINT_FORCE_PAUSE` is set.
After 5 minutes it gives up and carries on.

What `measure`'s callback sends nests under the measure, across awaits (AsyncLocalStorage), so the
window shows the run as a call tree.

## Configuration

| Env | Default | |
|-----|---------|--|
| `GLINT_URL` | `http://127.0.0.1:23600` | where glintd listens |
| `GLINT_ENABLED` | on, except `NODE_ENV=production` | `0` / `1` force it |
| `GLINT_PROJECT` | from the daemon (cwd name) | project name |

Or at runtime: `glint.configure({ url, enabled, project })`.

## Behaviour

- Calls never throw and never wait for the network. Events made in one synchronous block go
  out in one request.
- Objects keep their fields and type name; cycles, NaN/Infinity, BigInt, Map/Set, Dates,
  typed arrays and very deep values are encoded as `docs/PROTOCOL.md` §8 describes.
- If Glint is down, sending stops for 5 s at a time and events are dropped; the next event
  that gets through carries `meta.dropped`.
- Pending sends keep Node alive for at most 2 s at exit. `process.exit()` skips them; call
  `await glint.flush()` first.

## Conformance

```bash
glint conformance -- node sdks/js/conformance.mjs   # 20/20
```
