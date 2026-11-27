# CLAUDE.md — working on Glint

Glint is a local, language-agnostic debug and observability desk for developers and their AI agents:
any program sends events (values, logs, SQL, exceptions, LLM calls, agent steps) to a local daemon;
Glint shows them as a live feed and exposes them to agents over MCP.

Read before non-trivial work:
- `docs/ARCHITECTURE.md` — the shape of the system and why.
- `docs/PROTOCOL.md` — wire protocol v1 (normative).
- `docs/adr/` — decisions already made. Don't relitigate them; write a new ADR to supersede one.

## Status

Pre-code. Docs and architecture exist; the Cargo workspace is not scaffolded yet. See `docs/ROADMAP.md`
for the next milestone.

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
5. **SDKs never hurt the host program**: no throws, no blocking network calls (except `pause`),
   bounded queues, disabled in production by default.
6. **Local-first and private.** Loopback by default. Nothing goes to a remote model unless the user
   enables `glint-ai`, and redaction runs first.
7. **MCP output is token-budgeted.** Tools return compact projections first; full payloads on request.

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

## Commands (once scaffolded)

```bash
cargo check --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo run -p glintd
cd ui && pnpm dev
cd app && pnpm tauri dev
```

## Conventions

- Tests next to code; golden/fixture tests for adapters; conformance tests for SDKs.
- Benchmarks for ingest and store paths in `benches/`; keep within budgets in ARCHITECTURE §12.
- Commit messages: imperative, explain why. One concern per commit.
- Docs are part of the change: protocol, kind or adapter changes update `docs/` in the same commit.
