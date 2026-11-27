# Glint Wire Protocol — v1

Status: **draft, normative**. Machine-readable source of truth: `spec/envelope.schema.json` and
`spec/kinds/*.schema.json`. If this document and the schemas disagree, the schemas win and this
document is a bug.

The key words MUST, SHOULD and MAY are used as in RFC 2119.

---

## 1. Transport

- Base URL: `http://127.0.0.1:23600` (override with `GLINT_URL`).
- All bodies are UTF-8 JSON. `Content-Type: application/json` or `application/x-ndjson`.
- No auth on loopback. When the server binds beyond loopback it requires
  `Authorization: Bearer <token>` (token from `GLINT_TOKEN`).
- Requests carrying an `Origin` header not on the server allowlist are rejected with `403`.

## 2. Sending events

```
POST /v1/events
Content-Type: application/json

[ <envelope>, <envelope>, ... ]
```

- A single envelope object (not wrapped in an array) MUST also be accepted.
- With `application/x-ndjson`, one envelope per line; the server MAY process lines as they stream.
- Response: `202 Accepted` with `{"accepted": n, "rejected": [{"index": i, "error": "..."}]}`.
  A batch is never rejected as a whole because one envelope is invalid.
- Max body: 16 MiB. Max envelope: 1 MiB (larger → `413` for that envelope; use blobs, §6).

## 3. Envelope

Only `kind` and `payload` are required. Everything else is filled by the server when absent.

| Field | Type | Req | Default / meaning |
|-------|------|-----|-------------------|
| `v` | int | no | `1` |
| `id` | string (ULID) | no | server-assigned. Clients SHOULD set it if they need to refer to the event later (`parent`, `ctl`). |
| `ts` | RFC 3339 string, µs precision | no | server receive time |
| `run` | string | no | server derives from `origin.pid` + process start; SDKs SHOULD set one per request/test/CLI run |
| `parent` | string (event id) | no | `null` |
| `project` | string | no | git root name of `origin.file`, else `"default"` |
| `origin` | object | no | see §3.1 |
| `kind` | string | **yes** | namespaced kind, §4 |
| `payload` | any JSON | **yes** | kind-specific |
| `meta` | object | no | see §3.2 |

### 3.1 `origin`

`lang`, `runtime`, `framework`, `sdk` (`"<name>/<semver>"`), `file`, `line`, `col`, `fn`, `host`,
`pid`, `thread`, `cwd`. All optional strings/ints. `file` SHOULD be absolute or relative to `cwd`.

### 3.2 `meta`

| Field | Type | Meaning |
|-------|------|---------|
| `level` | `trace\|debug\|info\|warn\|error\|fatal` | default `debug` (`error` for `core.exception`) |
| `color` | `gray\|green\|orange\|red\|blue\|purple` | display hint |
| `label` | string | short caption shown next to the event |
| `screen` | string | named sub-feed; switching screen clears the view, not the store |
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
| `core.value` | `{ "value": any, "type"?: string }` — the `dump()` of a value; `type` is the source-language type name |
| `core.log` | `{ "message": string, "fields"?: object, "logger"?: string }` |
| `core.text` / `core.html` / `core.json` | `{ "content": string \| any }` |
| `core.table` | `{ "columns"?: string[], "rows": any[][] \| object[] }` |
| `core.exception` | `{ "class": string, "message": string, "frames": Frame[], "cause"?: <exception> }` |
| `core.trace` | `{ "frames": Frame[] }` |
| `core.measure` | `{ "name": string, "duration_ms": number, "memory_bytes"?: int }` |
| `core.separator`, `core.clear` | `{}` |
| `sql.query` | `{ "sql": string, "bindings"?: any[], "duration_ms"?: number, "conn"?: string, "rows"?: int }` |
| `http.request` | `{ "method", "url", "status"?, "headers"?, "body"?, "duration_ms"? }` |
| `llm.call` | `{ "provider", "model", "messages", "tools"?, "output", "usage": {input, output, cache_read?, cache_write?}, "cost_usd"?, "latency_ms"?, "stop_reason"? }` |
| `agent.step` | `{ "agent"?, "session", "input"?, "output"?, "status" }` |
| `agent.tool_call` | `{ "tool": string, "args": any, "result"?: any, "error"?: string, "duration_ms"? }` |

`Frame = { "file"?, "line"?, "col"?, "fn"?, "class"?, "vendor"?: bool, "snippet"?: {start, lines[]} }`

## 5. Control plane

### 5.1 Pause

1. Client sends an envelope `{"id": "<ulid>", "kind": "ctl.pause", "payload": {"reason"?: string, "timeout_s"?: int}}`.
2. Client long-polls `GET /v1/ctl/<id>` (server holds up to 30 s, returns `204` on hold expiry; client re-polls).
3. When resumed, server returns `200 {"action": "continue", "value"?: any}`.
4. Clients MUST give up after `timeout_s` (default 300) and continue. Clients SHOULD skip pauses when
   `CI=true` unless `GLINT_FORCE_PAUSE=1`.

### 5.2 Wait for (server-side, used by MCP and CLI)

`GET /v1/wait?filter=<expr>&timeout_s=60` → first matching envelope or `204`.

## 6. Blobs

`PUT /v1/blobs` with raw bytes → `{"ref": "blob://sha256:<hex>"}`. Payload fields may hold the ref;
renderers fetch `GET /v1/blobs/sha256:<hex>`.

## 7. Reading

| Endpoint | Purpose |
|----------|---------|
| `GET /v1/info` | server version, supported `v`, known kinds, enabled adapters |
| `GET /v1/runs?project=&limit=` | recent runs |
| `GET /v1/events?run=&kind=&level=&q=&since=&limit=` | query |
| `GET /v1/events/<id>` | one envelope |
| `GET /v1/stream?filter=` (WebSocket) | live envelopes |

Filter expression (shared by `/v1/stream`, `/v1/wait`, MCP): `kind:sql.* level>=warn project:shop-api "free text"`.

## 8. SDK requirements (conformance)

A client is conformant when it:

1. Never throws into or crashes the host program because Glint is down; logs once, then stays quiet.
2. Never blocks the caller on network I/O, except `pause`. Uses a bounded queue (drop oldest + count drops, reported as `meta.dropped`).
3. Captures `origin.file`/`line`/`fn` of the *user* call site, skipping its own frames.
4. Sets `origin.lang`, `origin.sdk`, and a stable `run` per process (or per request in frameworks).
5. Serialises non-JSON values faithfully: cyclic refs as `{"$ref": "..."}`, binary as base64 with
   `{"$bin": ...}`, special floats as strings, depth/size limits with `{"$truncated": true}`.
6. Is disabled by default in production (`GLINT_ENABLED` / framework env check).
7. Passes `glint conformance run -- <test program>` for every scenario in `spec/conformance/`.

## 9. Foreign protocols

Accepted on their own ports and translated to envelopes; mapping tables in `docs/mappings/`:
Ray (`:23517`), OTLP/HTTP logs & traces incl. `gen_ai.*` semconv (`:4318`), NDJSON stdin (`glint pipe`).
