# Roadmap

Each milestone ends with something a person can use. Order is a guide, not a contract.

## M0 — Skeleton ✅ done 2026-11-27
- Cargo workspace with `glint-protocol`, `glint-core`, `glint-store-sqlite`, `glint-ingest` (native
  only), `glint-server`, `glintd`.
- `spec/envelope.schema.json` + schemas for `core.value`, `core.log`, `core.exception`.
- `POST /v1/events`, `GET /v1/events`, `/v1/stream` WebSocket.
- Minimal UI: live feed, JSON tree renderer, exception renderer.
- Done when: `curl -d '{"kind":"core.value","payload":{"value":42}}' :23600/v1/events` shows up live.
- Shipped extras: `core.update` folding, sandboxed `core.html`, table/SQL/measure renderers,
  Host/Origin guard, read-your-writes ingest. UI is one dependency-free HTML file for now.

## M1 — Useful daily (current target)

- ✅ `glint` CLI: `send`, `pipe`. `tail` (log files) still to do.
- ✅ SDKs: JS/TS, Python, PHP, each 20/20 on conformance suite v1 (`glint conformance`).
- UI moves to Vite + TS; screens, configurable open-in-editor scheme, markdown rendering.
- Retention (count/age) and `glint clear`.
- ✅ Ray-compatible ingest adapter (`docs/mappings/ray.md`), verified with spatie/ray 1.48.1.

## M2 — Agents (in progress)
- ✅ `glint-mcp`: runs, events, event, last_error, wait_for, pauses, resume, annotate, show,
  clear — over HTTP `/mcp` and stdio `glint mcp` (ADR 0004).
- ✅ Control plane: pause/resume from UI, MCP and API; Ray `pause()`; JS `glint.pause()`.
- `glint.pause()` in the Python and PHP SDKs (Ray's pause already covers PHP).
- ✅ Agent-hooks adapter (Claude Code) → `agent.*` kinds (`docs/mappings/claude-code.md`).
- Tree view of agent steps → tool calls in the UI (events carry `parent` already).

## M3 — LLM observability (in progress)
- ✅ OTLP/HTTP receiver (protobuf + JSON) with `gen_ai.*` → `llm.call` (`docs/mappings/otlp.md`).
- ✅ LLM card in the UI (transcript, tokens, latency, cost when reported); MCP `glint_llm_calls`.
- Prompt diff between two calls; user price table for cost; gzip bodies.
- SDK helpers for wrapping Anthropic/OpenAI clients directly.

## M4 — Desktop
- Tauri app: tray badge, notifications, mini feed, global hotkey, auto-update, signed macOS build.

## M5 — Breadth
- SDKs: Go, Rust, Ruby, Java/Kotlin, C#, Elixir.
- Integrations: Laravel, Django, FastAPI, Express/Next, Rails, Spring.
- Renderer plugin loading for `x-*` kinds.
- `glint-ai`: explain exception, group, summarise run (local model by default).
