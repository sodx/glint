# CLAUDE.md — working on Glint

Glint is a local, language-agnostic debug and observability desk for developers and their AI agents:
any program sends events (values, logs, SQL, exceptions, LLM calls, agent steps) to a local daemon;
Glint shows them as a live feed and exposes them to agents over MCP.

Read before non-trivial work:
- `docs/ARCHITECTURE.md` — the shape of the system and why.
- `docs/PROTOCOL.md` — wire protocol v1 (normative).
- `docs/adr/` — decisions already made. Don't relitigate them; write a new ADR to supersede one.

## Status

M0 done: `glintd` ingests the native protocol, stores in SQLite + FTS5, streams over WebSocket and
serves a single-file live-feed UI (`ui/index.html`, no build step yet; Vite + TS arrives with M1).
Next: M1 in `docs/ROADMAP.md`.

M1 in progress: Ray-compatible ingest on `:23517` is done (`docs/mappings/ray.md`).

Crates that exist: `glint-protocol`, `glint-core`, `glint-store-sqlite`, `glint-ingest` (native, ray),
`glint-server`, `glintd`. The rest of the layout in ARCHITECTURE §10 is planned, not present.

## Stack

- Rust (stable, edition 2024) workspace in `crates/`. Async: `tokio`. HTTP/WS: `axum`. Store:
  `rusqlite` (bundled, FTS5) behind the `Store` trait. MCP: `rmcp`. Errors: `thiserror` in libraries,
  `anyhow` only in binaries. Logging: `tracing`.
- Desktop shell: Tauri 2 in `app/`, embedding `glint-server`.
- UI: TypeScript + Vite in `ui/`, served by the daemon and embedded in Tauri. Same build for both.
- SDKs in `sdks/<lang>/` use each ecosystem's native tooling and zero or near-zero dependencies.

## Invariants — do not break

1. **The protocol is the product.** `spec/` (JSON Schemas) is the source of truth. Change a kind's
   schema first, then Rust types, renderer, SDKs. Kinds change additively; a breaking change is a
   new kind name.
2. **The core knows no language or framework.** Nothing in `glint-core` / `glint-protocol` may
   mention PHP, Laravel, Django, etc. Language knowledge lives in `glint-ingest` adapters, `sdks/`
   and `integrations/`.
3. **Unknown is fine.** Unknown kinds and unknown `meta` keys are stored and rendered generically,
   never rejected.
4. **Dependency direction:** `glint-protocol ← glint-core ← {store-sqlite, ingest, mcp, ai} ←
   glint-server ← {glintd, app, glint-cli}`. `glint-core` must not depend on axum, Tauri, SQLite or
   any LLM client.
5. **Read-your-writes.** `POST /v1/events` answers only after the events are stored; the writer
   still batches concurrent requests into one transaction. Events are stored before they are
   published on the bus.
6. **SDKs never hurt the host program**: no throws, no blocking network calls (except `pause`),
   bounded queues, disabled in production by default.
7. **Local-first and private.** Loopback by default. Nothing goes to a remote model unless the user
   enables `glint-ai`, and redaction runs first.
8. **MCP output is token-budgeted.** Tools return compact projections first; full payloads on request.

## Adding things

- **New kind:** `spec/kinds/<kind>.schema.json` → type in `glint-protocol` → renderer in
  `ui/src/renderers/` → MCP projection in `glint-mcp` → conformance fixture → docs table in
  `PROTOCOL.md`.
- **New ingest adapter:** module in `crates/glint-ingest/src/<name>/`, translates foreign bytes to
  `Envelope` only, mapping table in `docs/mappings/<name>.md`, golden tests with recorded input.
- **New language SDK:** `sdks/<lang>/`, must pass `glint conformance`. Update
  `spec/support-matrix.yaml`.
- **New framework integration:** `integrations/<framework>/`, builds on the language SDK, emits only
  existing kinds or `x-<framework>.*` kinds, ships a manifest of the kinds it emits.

## Commands

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo run -p glintd                      # http://127.0.0.1:23600, DB at ~/.glint/glint.db
GLINT_DB=/tmp/g.db GLINT_ADDR=127.0.0.1:23700 GLINT_RAY_ADDR=off cargo run -p glintd
curl -d '{"kind":"core.value","payload":{"value":42}}' http://127.0.0.1:23600/v1/events
```

`ui/index.html` is compiled into the binary (`include_str!`): rebuild `glintd` after editing it.

## Conventions

- Tests next to code; golden/fixture tests for adapters; conformance tests for SDKs.
- Benchmarks for ingest and store paths in `benches/`; keep within budgets in ARCHITECTURE §12.
- Commit messages: imperative, explain why. One concern per commit.
- Docs are part of the change: protocol, kind or adapter changes update `docs/` in the same commit.
