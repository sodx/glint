# Roadmap

Each milestone ends with something a person can use. Order is a guide, not a contract.

## M0 — Skeleton (current target)
- Cargo workspace with `glint-protocol`, `glint-core`, `glint-store-sqlite`, `glint-ingest` (native
  only), `glint-server`, `glintd`.
- `spec/envelope.schema.json` + schemas for `core.value`, `core.log`, `core.exception`.
- `POST /v1/events`, `GET /v1/events`, `/v1/stream` WebSocket.
- Minimal UI: live feed, JSON tree renderer, exception renderer.
- Done when: `curl -d '{"kind":"core.value","payload":{"value":42}}' :23600/v1/events` shows up live.

## M1 — Useful daily
- `glint` CLI: `send`, `pipe`, `tail`.
- SDKs: JS/TS, Python, PHP (+ conformance suite v1 and runner).
- Renderers: table, SQL, measure, colors/labels/screens; open-in-editor.
- Ray-compatible ingest adapter.

## M2 — Agents
- `glint-mcp`: runs, events, event, last_error, wait_for, annotate, clear.
- Control plane: pause/resume (UI + MCP).
- Agent-hooks adapter (Claude Code first) → `agent.*` kinds and tree view.

## M3 — LLM observability
- OTLP adapter with `gen_ai.*` mapping → `llm.call`.
- LLM view: transcript, tools, tokens, cost, latency, prompt diff.
- SDK helpers for wrapping Anthropic/OpenAI clients directly.

## M4 — Desktop
- Tauri app: tray badge, notifications, mini feed, global hotkey, auto-update, signed macOS build.

## M5 — Breadth
- SDKs: Go, Rust, Ruby, Java/Kotlin, C#, Elixir.
- Integrations: Laravel, Django, FastAPI, Express/Next, Rails, Spring.
- Renderer plugin loading for `x-*` kinds.
- `glint-ai`: explain exception, group, summarise run (local model by default).
