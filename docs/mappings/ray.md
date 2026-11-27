# Mapping: Ray to Glint

Adapter: `crates/glint-ingest/src/ray/` (translation), `crates/glint-server/src/ray.rs` (HTTP).
Background on the Ray protocol: `docs/research/ray.md`.

Pause verified end to end on 2026-11-27: `ray()->pause()` held a PHP script until an agent
called `glint_resume` through `glint mcp`.

Verified end to end with `spatie/ray` 1.48.1 (PHP 8.1) on 2026-11-27. Every call in
`newScreen, ray(string|object|array|int|bool|null), label, orange, table, text, html, toJson,
measure, separator, caller, exception, remove, hide, count, pause` arrived with the right kind,
file and line.

## Listener

| Ray client expects | Glint answers |
|--------------------|---------------|
| `POST /` | translate, store, `200` once stored |
| `GET /_availability_check` | `404`. The PHP client reads an HTTP error status as "app is up". |
| `GET /locks/{name}` | `{"active": true}` while the pause waits. Continue gives `{"active": false}`, and Stop gives `{"stop_execution": true}` (PHP throws `StopExecutionRequested`). Unknown lock: continue. |
| `GET /windows`, `GET /theme` | `[]`, `{}` |

Port `127.0.0.1:23517` (`GLINT_RAY_ADDR`; `off` disables it). If the port is busy, usually because
the Ray app is running, glintd starts without the adapter and says so. `/v1/info` lists the
adapters that are actually running. `Host: host.docker.internal:<port>` is accepted for clients in
containers.

## Ids: one uuid, one entry

- The `n`-th data payload for a uuid gets id `ray-<uuid>` (n = 1) or `ray-<uuid>-<n>`.
- Commands (`color`, `label`, `size`, `hide`, `remove`) become `core.update` envelopes, one per
  entry of that uuid. A command that arrives before any data targets `ray-<uuid>`.
- The adapter remembers the last 10 000 uuids. After a daemon restart, a reused uuid starts again
  at `ray-<uuid>`. The store ignores it as a duplicate id. Real clients generate a fresh uuid
  per `ray()` call, so this does not happen in practice.

## Envelope fields

| Ray | Glint |
|-----|-------|
| `payloads[].origin.file` / `line_number` / `hostname` / `function_name` | `origin.file` / `line` / `host` / `fn` |
| `meta.php_version` | `origin.lang = "php"`, `origin.runtime` |
| `meta.ray_package_version` | `origin.sdk = "spatie/ray/<v>"` |
| `meta.node_ray_package_version` | `origin.lang = "js"`, `origin.sdk = "node-ray/<v>"` |
| `meta.project_name` (non-empty) | `project` |
| — | `origin.via = "ray"` |

## Payload types

| Ray `type` | Glint kind | Notes |
|------------|------------|-------|
| `log`, all values scalar | `core.value` | one value → `value`; several → array |
| `log`, any HtmlDumper value | `core.text` | `content` = `meta[0].clipboard_data` (PlainTextDumper, depth 5, ≤ 20 000 chars); `html` keeps the original; the UI can toggle between them. With no clipboard text, the kind is `core.html`. |
| `custom` label `Boolean` / `Null` | `core.value` | |
| `custom` label `Text` / `XML` | `core.text` | `&nbsp;`, `<br />`, entities undone |
| `custom` label `HTML` / `Image` / `""` | `core.html` | rendered in a sandboxed iframe |
| `custom` other label | `core.text` or `core.html` | label kept as `meta.label` (e.g. `Count`) |
| `json_string` | `core.json` | `core.text` if the string is not JSON |
| `table` | `core.table` | columns `key`, `value`; HTML cells reduced to text; default label `Table` dropped |
| `carbon` | `core.value` | `type: "Carbon"` |
| `exception` | `core.exception` | frames: `file_name→file`, `line_number→line`, `method→fn`, `vendor_frame→vendor` |
| `trace` / `caller` | `core.trace` | `caller` labelled |
| `measure` | `core.measure` | `total_time→duration_ms`, `max_memory_usage_during_total_time→memory_bytes`, `time_since_last_call→since_last_ms`; first call labelled "timer started" |
| `application_log` | `core.log` | `context → fields` |
| `executed_query` | `sql.query` | `connection_name→conn`, `time→duration_ms` |
| `separator` | `core.separator` | |
| `new_screen` (named) / (empty name) | `core.screen` / `core.clear` | |
| `clear_all` | `core.clear` | |
| `notify` | `core.log` | level `info`, label `notify` |
| `create_lock` (`pause()`) | `ctl.pause` | id `ray-lock-<name>`; answered from the UI or MCP `glint_resume` |
| `ai_custom_markdown` / `ai_custom_mermaid` / `ai_custom_html` | `core.markdown` / `core.mermaid` / `core.html` | `iterationOf → iteration_of` |
| `color` `label` `size` `hide` `remove` | `core.update` | `yellow→orange`, `grey→gray` |
| `expand` `show_app` `hide_app` `confetti` `screen_color` | — | ignored: they drive Ray's window |
| anything else (`event`, `job_event`, `mailable`, `eloquent_model`, `view`, `response`, …) | `x-ray.<type>` | payload passed through; JSON-tree rendering |

## Not verified yet

- node-ray's availability probe (`locks/__availabilty_check`) gets the lock route's `200` JSON
  answer. Whether node-ray reads that as "up" has not been tested with the real client.
- Laravel watchers beyond `executed_query` were mapped from source, not from live traffic.
- Ray 4 may have changed endpoints. The clients tested here are the current open-source ones.
