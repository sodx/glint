# Research: how Ray (myray.app) works

Date: 2026-11-27. Sources: `spatie/ray`, `spatie/laravel-ray`, `spatie/ray-skills`, `permafrost-dev/node-ray`,
and `spatie/myray.app` (docs + blog sources). Purpose: build a protocol-compatible ingest adapter and
find where Glint must be different, not just "also".

## 1. Wire protocol (client side)

- `POST http://localhost:23517/`, JSON, `User-Agent: Ray 1.0`, 2 s timeout, errors swallowed.
- Availability probe: `GET /_availability_check`; the PHP client treats **any HTTP error status
  (≈404) as "up"**, connection failure as "down", cached 30 s per host:port. node-ray probes
  `locks/__availabilty_check` (sic).
- Other GETs: `/locks/{name}?hostname=&project_name=` → `{"active": bool, "stop_execution": bool}`,
  `/windows` → JSON array, `/theme` → JSON object.
- Config: PHP looks for `ray.php` walking up from cwd; Laravel uses `config/ray.php` + `RAY_ENABLED`,
  `RAY_HOST`, `RAY_PORT`, `RAY_REMOTE_PATH`, `RAY_LOCAL_PATH`. Docker: `host.docker.internal`;
  `remote_path`/`local_path` rewrite file paths for editor links.

Body:

```json
{
  "uuid": "6f1c…",
  "payloads": [
    { "type": "log",
      "content": { "values": ["hello"], "meta": [{ "clipboard_data": "hello" }] },
      "origin": { "file": "/app/x.php", "line_number": 12, "hostname": "mbp", "function_name": "optional" } }
  ],
  "meta": { "php_version": "8.3.4", "php_version_id": 80304, "project_name": "", "ray_package_version": "1.41.0" }
}
```

Non-scalar values arrive as **pre-rendered Symfony HtmlDumper HTML** (with `<script>`), not data.

## 2. Payload types

Data: `log`, `custom` (`{content, label}` — also used for Text/HTML/XML/Bool/Null/Image/File, told
apart only by `label`), `json_string`, `table`, `carbon`, `exception`
(`{class, message, frames:[{file_name, line_number, class, method, vendor_frame}]}`), `trace`,
`caller`, `measure`, `application_log`.

Commands on the entry with the same uuid: `color`, `label`, `size`, `remove`, `hide`, `expand`.
App-level: `new_screen`, `clear_all`, `screen_color`, `show_app`, `hide_app`, `notify`, `confetti`,
`separator`, `create_lock`.

Laravel adds: `executed_query {sql, bindings, connection_name, time}`, `event`, `job_event`,
`mailable`, `eloquent_model`, `view`, `response`. Watchers: exceptions, mails, logs, dumps on by
default; queries (all/duplicate/slow/by verb), jobs, events, cache, views, requests, HTTP client,
deprecations opt-in via `send_*_to_ray` / env.

Agent types (only in `spatie/ray-skills`): `ai_custom_html {html, label?, iterationOf?}`,
`ai_custom_markdown`, `ai_custom_mermaid`.

## 3. uuid = mutable entry

Each `ray()` makes a new UUIDv4; every chained call (`->color()->label()`) is a **separate HTTP
POST with the same uuid** carrying one command payload. The app merges them into one entry.
Not batched.

## 4. Pause

`pause()` sends `create_lock {name: md5(time())}` then polls `/locks/{name}` every 1 s with no
overall timeout. `stop_execution: true` throws in the client. Two pauses in the same second collide.

## 5. Caller detection

Reverse backtrace, skip frames of the Ray package (and Laravel helpers), take the next frame.
PHP origin omits `function_name`.

## 6. Clients

All use the same protocol. Spatie (MIT): php, laravel, wordpress, craft, drupal, yii, symfony,
ruby, rails, rust (tiny), global-ray. permafrost-dev (MIT): node-ray, vue-ray, node-ray-cli, ray-proxy.
Community: go-ray (stale since 2022), ray-python, ray-dbg (Rust), ray_dart. There is no formal spec;
library authors are told to reverse-engineer `spatie/ray` with `ray-proxy`.

## 7. App and AI

- Closed source, paid (yearly license; trial = 20 messages per session).
- Ray 3 (2026-01): built-in **MCP server on :2411**, message archive. Known tools include
  `send_custom_html_output`, `send_custom_markdown_output`, `send_mermaid_diagram`, `show_app`,
  `hide_app`, `clear_all`, plus tools that read window contents.
- **Ray 4 (2026-11-24): rewritten in Rust on GPUI** (Zed's UI engine); fast and small. Electron is gone.
- Remote servers over SSH, multiple windows/screens, IDE links, themes, search, filters.

## 8. What this means for Glint

Things Ray already has, so Glint cannot win on them alone: a native fast app (Ray 4 is Rust), MCP
for agents (Ray 3), and broad PHP-centred client coverage.

Gaps Glint should own:

| Ray today | Glint |
|-----------|-------|
| Values arrive as pre-rendered HTML; the viewer cannot search, diff or re-render them | **Structured data** (`core.value` = JSON + type info); rendering happens in the viewer |
| No formal spec; behaviour learned by proxying traffic | **Normative spec + JSON Schemas + conformance suite** |
| PHP-first; other languages community-run, some stale | Language-neutral core; **OTLP ingest** covers every OTel language with no SDK |
| No spans, nesting, runs, or LLM/agent tracing | `run` + `parent` trees, **`llm.*` and `agent.*` kinds**, `gen_ai.*` mapping |
| MCP lets agents write HTML into Ray and read the window | MCP built around **querying runs/events, waiting, resuming pauses**, token-budgeted |
| Pause: 1 s polling, no timeout, name collisions | Long-poll, ULID ids, timeouts, CI-aware; agents can resume |
| No auth; any local process can inject HTML with `<script>` | Origin check, optional token, **sanitised rendering** of untrusted HTML |
| Closed, paid, 20-message trial | Open source |
| Archive only, no query/export | SQLite + FTS, query API, export |

## 9. Ray-compat adapter: requirements

- Listen on `:23517`; `POST /` with the body above. Accept `function_name` as optional.
- `GET /_availability_check` and `GET /locks/__availabilty_check` → `404` (that is what clients read as "up").
- `GET /locks/{name}` → `{"active", "stop_execution"}` backed by Glint's control plane.
- `GET /windows` → `[]`, `GET /theme` → `{}` (stubs are enough).
- Keep a per-uuid entry: data payloads create/append; command payloads (`color`, `label`, `size`,
  `hide`, `remove`, `expand`) become **meta updates on that entry**, not new feed items. Commands may
  arrive before the data; buffer briefly.
- `custom` with label `HTML`/`Image`/HtmlDumper output → `core.html` (sanitised); `Text` → `core.text`;
  `Boolean`/`Null` → `core.value`; `executed_query` → `sql.query`; `exception`/`trace` → `core.exception` /
  `core.trace` (frame field renames); `measure` → `core.measure`; `table` → `core.table`;
  `new_screen`/`clear_all` → `core.screen`/`core.clear`; `ai_custom_markdown`/`_mermaid` → `core.markdown`/`core.mermaid`.
- Map `meta.project_name` → `project`, `uuid` → stable envelope id, origin fields 1:1.

## Unverified

The MCP transport/endpoint on :2411 and its full tool list; whether Ray 4 kept every HTTP endpoint
byte-for-byte; exact app response bodies; how the app handles commands for unknown uuids; prices.
