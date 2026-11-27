# Glint wire protocol: v1

Status: draft, normative. The machine-readable source of truth is `spec/envelope.schema.json` and
`spec/kinds/*.schema.json`. If this document and the schemas disagree, the schemas win and this
document has a bug.

The key words MUST, SHOULD and MAY are used as defined in RFC 2119.

---

## 1. Transport

- Base URL: `http://127.0.0.1:23600` (override with `GLINT_URL`).
- All bodies are UTF-8 JSON, sent as `Content-Type: application/json` or `application/x-ndjson`.
- No auth on loopback. When the server binds beyond loopback, it requires
  `Authorization: Bearer <token>` (token from `GLINT_TOKEN`).
- Requests with an `Origin` header that is not on the server allowlist are rejected with `403`.

## 2. Sending events

```
POST /v1/events
Content-Type: application/json

[ <envelope>, <envelope>, ... ]
```

- A single envelope object (not wrapped in an array) MUST also be accepted.
- With `application/x-ndjson`, send one envelope per line. The server MAY process lines as they stream.
- Response: `202 Accepted` with `{"accepted": n, "ids": [...], "rejected": [{"index": i, "error": "..."}]}`,
  sent after the accepted envelopes are stored, so a read issued afterwards sees them. `ids` lists the final
  ids of accepted envelopes in body order. A batch is never rejected as a whole because one envelope is
  invalid.
- A repeated `id` is treated as a client retry. The first stored copy wins, and later ones are ignored.
- Loopback servers only answer requests whose `Host` is `127.0.0.1:<port>`, `localhost:<port>` or
  `[::1]:<port>` (DNS-rebinding guard).
- Max body: 16 MiB. Max envelope: 1 MiB. A larger envelope gets `413`; use blobs (§6) instead.

## 3. Envelope

Only `kind` and `payload` are required. The server fills in everything else when it is absent.

| Field | Type | Req | Default / meaning |
|-------|------|-----|-------------------|
| `v` | int | no | `1` |
| `id` | string (ULID) | no | server-assigned. Clients SHOULD set it if they need to refer to the event later (`parent`, `ctl`). |
| `ts` | RFC 3339 string, µs precision | no | server receive time |
| `run` | string | no | server derives it from `origin.pid` and process start. SDKs SHOULD set one per request, test or CLI run. |
| `parent` | string (event id) | no | `null` |
| `parent` | string | no | the id of the event this one happened inside. SDKs set it to the enclosing `glint.measure()`, and OTLP sets it to the parent span. The UI nests by it. |
| `project` | string | no | the git repository of `origin.file`, else of `origin.cwd`. Linked worktrees name their main repository and add `origin.worktree` and `origin.branch`. Otherwise the cwd's folder name, else `"default"`. |
| `origin` | object | no | see §3.1 |
| `kind` | string | yes | namespaced kind, §4 |
| `payload` | any JSON | yes | kind-specific |
| `meta` | object | no | see §3.2 |

### 3.1 `origin`

`lang`, `runtime`, `framework`, `sdk` (`"<name>/<semver>"`), `file`, `line`, `col`, `fn`, `host`,
`pid`, `thread`, `cwd`. All are optional strings or ints. `file` SHOULD be absolute or relative to `cwd`.

`path_map` is for code that runs in a container or on another machine, where file paths are not
the paths on the machine running the server: comma-separated `<container path>:<host path>` pairs
(`/var/www/html:/Users/me/shop`), split at the first colon, so a host path may be a Windows path.
SDKs send the `GLINT_PATH_MAP` environment variable here. The server rewrites `origin.file`,
`origin.cwd` and each `payload.frames[].file` that starts with a container path (the longest
match wins), then drops `path_map`; the project is then derived from the host paths.

### 3.2 `meta`

| Field | Type | Meaning |
|-------|------|---------|
| `level` | `trace\|debug\|info\|warn\|error\|fatal` | default `debug` (`error` for `core.exception`) |
| `color` | `gray\|green\|orange\|red\|blue\|purple` | display hint |
| `label` | string | short caption shown next to the event |
| `screen` | string | named sub-feed. Switching screen clears the view, not the store. |
| `tags` | string[] | free-form |
| `size` | `sm\|md\|lg` | display hint |
| `hidden` | bool | stored, collapsed by default |

Unknown `meta` keys MUST be preserved and ignored.

## 4. Kinds

Kind names match `^(core|sql|http|llm|agent|ctl|x-[a-z0-9-]+)\.[a-z0-9_.]+$`.
Unknown kinds MUST be accepted and stored. Each kind's payload schema lives in
`spec/kinds/<kind>.schema.json`. Summary of v1 core kinds:

| Kind | Payload (abridged) |
|------|--------------------|
| `core.value` | `{ "value": any, "type"?: string }`: the `dump()` of a value. `type` is the source-language type name. |
| `core.log` | `{ "message": string, "fields"?: object, "logger"?: string }` |
| `core.text` / `core.html` / `core.json` | `{ "content": string \| any }` |
| `core.table` | `{ "columns"?: string[], "rows": any[][] \| object[] }` |
| `core.exception` | `{ "class": string, "message": string, "frames": Frame[], "cause"?: <exception> }` |
| `core.trace` | `{ "frames": Frame[] }` |
| `core.measure` | `{ "name": string, "duration_ms": number, "memory_bytes"?: int }` |
| `core.markdown` / `core.mermaid` | `{ "content": string }` |
| `core.separator`, `core.clear` | `{}` |
| `core.screen` | `{ "name": string, "color"?: string }` |
| `core.update` | `{ "target": <event id>, "meta"?: object, "hidden"?: bool, "removed"?: bool }`. It modifies an earlier event and is not a feed item. The target may arrive later than the update. The server buffers unresolved updates for 5 s. |
| `sql.query` | `{ "sql": string, "bindings"?: any[], "duration_ms"?: number, "conn"?: string, "rows"?: int }` |
| `http.request` | `{ "method", "url", "status"?, "headers"?, "body"?, "duration_ms"? }` |
| `llm.call` | `{ "provider", "model", "messages", "tools"?, "output", "usage": {input, output, cache_read?, cache_write?}, "cost_usd"?, "latency_ms"?, "stop_reason"? }` |
| `core.span` | `{ "name", "kind", "duration_ms", "status": "ok\|error\|unset", "status_message"?, "attributes"?, "events"? }`: a generic trace span (OTLP) |
| `agent.step` | `{ "agent"?, "session", "input"?, "output"?, "status" }` |
| `agent.tool_call` | `{ "tool": string, "args": any, "result"?: any, "error"?: string, "duration_ms"? }` |

`Frame = { "file"?, "line"?, "col"?, "fn"?, "class"?, "vendor"?: bool, "snippet"?: {start, lines[]} }`

## 5. Control plane

Control events (`ctl.pause`, `ctl.resume`) are stored like any other event, so history shows who paused what
and who answered. Pause state lives in the daemon's memory. After a restart, a waiting client gets `404` and
continues.

### 5.1 Pause

1. The client sends an envelope `{"id": "<ulid>", "kind": "ctl.pause", "payload": {"reason"?: string, "timeout_s"?: int}}`.
2. The client long-polls `GET /v1/ctl/<id>`. The server holds the request up to 30 s and returns `204` when
   the hold expires. The client re-polls.
3. When resumed, the server returns `200 {"action": "continue", "value"?: any}` or `200 {"action": "stop"}`.
   `404` means the pause is unknown (the daemon restarted), so the client continues.
4. Anyone may answer: the UI, an agent (MCP `glint_resume`), or
   `POST /v1/ctl/<id>/resume {"value"?: any, "stop"?: bool}` (`409` if already answered).
   The answer is recorded as `ctl.resume {target, action, value?, by}`.
   `GET /v1/ctl` lists pauses still waiting.
5. Clients MUST give up after `timeout_s` (default 300) and continue. Clients SHOULD skip pauses when
   `CI=true`, unless `GLINT_FORCE_PAUSE=1` is set.

### 5.2 Wait for (server-side, used by MCP and CLI)

`GET /v1/wait?filter=<expr>&timeout_s=60` returns the first matching envelope, or `204`.

## 5.3 MCP

`POST /mcp` speaks the Model Context Protocol (Streamable HTTP, JSON responses). Tools:
`glint_runs`, `glint_events`, `glint_event`, `glint_last_error`, `glint_wait_for`,
`glint_pauses`, `glint_resume`, `glint_annotate`, `glint_show`, `glint_clear`. `glint mcp` serves
the same tools over stdio. Agent annotations are `core.update` events with `meta.note`.

## 6. Blobs

`PUT /v1/blobs` with raw bytes returns `{"ref": "blob://sha256:<hex>"}`. Payload fields may hold the ref.
Renderers fetch `GET /v1/blobs/sha256:<hex>`.

## 7. Reading

| Endpoint | Purpose |
|----------|---------|
| `GET /v1/info` | server version, supported `v`, known kinds, enabled adapters |
| `GET /v1/runs?project=&limit=` | recent runs, newest first: `{run, project, first, last, events, errors, last_id, file?, head?}` (`file`: the first event's calling file; `head`: its first log line) |
| `GET /v1/source?event=&frame=` | the lines around an event's call site, or around `payload.frames[frame]`: `{file, line, start, lines}`. Only files an event names. |
| `POST /v1/open?event=&frame=&editor=` | opens that place in the editor (vscode, cursor, windsurf, zed, phpstorm, idea, webstorm, pycharm, sublime; default `GLINT_EDITOR`, else the first installed) |
| `POST /v1/expect` | `{expect: [..], ordered?, no_errors?, project?, run?, after?, lookback_s?, timeout_s?}` returns `{pass, steps: [{expect, met, matched, needed, events}], failure?, events_seen, report}`. It waits until decided or timeout (syntax: `crates/glint-core/src/expect.rs`). |
| `GET /v1/editors` | editors found on this machine, the default first |
| `GET /v1/events?run=&kind=&level=&q=&since=&limit=` | query |
| `GET /v1/events/<id>` | one envelope |
| `GET /v1/stream?filter=` (WebSocket) | live envelopes |

Filter expression (shared by `/v1/stream`, `/v1/wait` and MCP): `kind:sql.* level>=warn project:shop-api "free text"`.

## 8. SDK requirements (conformance)

A client is conformant when it:

1. Never throws into or crashes the host program because Glint is down. It logs once, then stays quiet.
2. Never blocks the caller on network I/O, except for `pause`. It uses a bounded queue. When the queue is full,
   it drops the oldest event and counts the drops, reported as `meta.dropped`.
3. Captures `origin.file`, `line` and `fn` of the *user* call site, skipping its own frames.
4. Sets `origin.lang`, `origin.sdk`, and a stable `run` per process (or per request in frameworks).
5. Serialises non-JSON values faithfully: cyclic refs as `{"$ref": "..."}`, binary as base64 with
   `{"$bin": ...}`, special floats as strings, and depth or size limits with `{"$truncated": true}`.
6. Is disabled by default in production (`GLINT_ENABLED` / framework env check).
7. Passes `glint conformance -- <command running its conformance program>`: every scenario in
   `spec/conformance/scenarios.json`, plus the unreachable-server and `GLINT_ENABLED=0` runs.
   The SDK surface and exact rules are in `spec/conformance/README.md`.
8. Assigns a client `id` (ULID) to every event, so fluent calls (`.label()`, `.red()`, `.hide()`) can target it
   with `core.update`.

## 9. Foreign protocols

These are accepted on their own ports and translated to envelopes. Mapping tables are in `docs/mappings/`:
Ray (`:23517`), OTLP/HTTP logs and traces including `gen_ai.*` semconv (`:4318`), and NDJSON stdin (`glint pipe`).
