# Mapping: OpenTelemetry (OTLP/HTTP) → Glint

Adapter: `crates/glint-ingest/src/otlp/` (decoding and mapping), `crates/glint-server/src/otlp.rs`
(receiver). Listens on `127.0.0.1:4318` (`GLINT_OTLP_ADDR`, `off` disables; skipped if a real
collector owns the port). Accepts `POST /v1/traces` and `POST /v1/logs`, protobuf
(`application/x-protobuf`, the SDK default) or JSON. Compressed bodies are refused with 415:
set `OTEL_EXPORTER_OTLP_COMPRESSION=none`.

```bash
OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:4318 python app.py   # any OTel SDK, any language
```

Verified on 2026-11-27 with the Python SDK and `opentelemetry-exporter-otlp-proto-http` 1.45.2
(protobuf): a GenAI chat span, a tool span, a DB span, an error span with a recorded exception,
a server span and a `logging` record all arrived and mapped as below.

## Envelope fields

| OTel | Glint |
|------|-------|
| trace id | `run = "otel-<trace_id>"` |
| span id / parent span id | `id = "otel-<span_id>"`, `parent = "otel-<parent>"` |
| start time | `ts` |
| resource `service.name` | `project` |
| resource `telemetry.sdk.language` / `.name` / `.version` | `origin.lang`, `origin.sdk = "<name>/<version>"` |
| resource `process.runtime.*`, `host.name`, `process.pid` | `origin.runtime`, `host`, `pid` |
| `code.file.path` / `code.filepath`, `code.line.number` / `code.lineno`, `code.function(.name)` | `origin.file`, `line`, `fn` |
| scope name | `origin.scope`; `origin.via = "otlp"` |

## Spans

| Span | Kind | Notes |
|------|------|-------|
| has `gen_ai.*` and operation `chat` / `text_completion` / `generate_content` / `embeddings` / none | `llm.call` | see below |
| `gen_ai.operation.name = execute_tool` | `agent.tool_call` | `tool`, `args` (JSON-parsed), `result`, `tool_use_id` |
| `invoke_agent` / `create_agent` | `agent.step` | `agent`, `model` |
| `db.query.text` / `db.statement` | `sql.query` | `conn` = `db.system(.name)` |
| anything else | `core.span` | `name`, `kind`, `duration_ms`, `status`, attributes (minus `code.*`), events |
| event `exception` on any span | extra `core.exception` | `exception.type/message`, `stacktrace`; frames parsed from Python and `at …` traces; `parent` = the span |
| status code 2 | level `error` | |

## `llm.call` from GenAI semantic conventions

| Attribute(s) | `payload` |
|--------------|-----------|
| `gen_ai.operation.name` | `operation` |
| `gen_ai.provider.name`, else `gen_ai.system` | `provider` |
| `gen_ai.request.model` (else response model) | `model`; also `meta.label` |
| `gen_ai.response.model`, `.id`, `.finish_reasons` | `response_model`, `response_id`, `stop_reason` |
| `gen_ai.usage.input_tokens` / `prompt_tokens` / `llm.usage.prompt_tokens` | `usage.input` |
| `gen_ai.usage.output_tokens` / `completion_tokens` / `llm.usage.completion_tokens` | `usage.output` |
| `gen_ai.usage.cache_read.input_tokens` / `cache_read_input_tokens` / `cached_tokens` | `usage.cache_read` |
| `gen_ai.usage.cache_creation.input_tokens` / `cache_creation_input_tokens` | `usage.cache_write` |
| `llm.usage.total_tokens` / `gen_ai.usage.total_tokens` | `usage.total` |
| `gen_ai.usage.cost` / `llm.usage.cost` / `gen_ai.cost` | `cost_usd` (only when the sender provides it) |
| other `gen_ai.request.*` | `params` |
| `gen_ai.system_instructions`, `gen_ai.tool.definitions` | `system`, `tools` |
| span duration | `latency_ms` |
| status error | `error` = status message |

Messages, first match wins:

1. `gen_ai.input.messages` / `gen_ai.output.messages` (current conventions; JSON strings are parsed).
2. Span events `gen_ai.<role>.message` (`content`) and `gen_ai.choice` (`message`, `finish_reason`).
3. OpenLLMetry attributes `gen_ai.prompt.<i>.<field>` / `gen_ai.completion.<i>.<field>`.

Glint does not ship model prices: cost appears only when the instrumentation reports it.

## Logs

`core.log`: body → `message` (non-strings as JSON), attributes (minus `code.*`) → `fields`,
`event_name` kept in fields and as the label, severity number → level (1–4 trace … 21–24 fatal,
else the severity text). Trace and span ids link the record to its span (`run`, `parent`).
