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
serves the UI. The UI (`ui/index.html`, single file, no build step) follows PRODUCT.md principle 0
"answer first": one status line (newest unseen failure, waiting pause, or All quiet), then the
current run as one line per event. Design tokens are documented at the top of its CSS.
Next: M1 in `docs/ROADMAP.md`.

M1 in progress: the `glint` CLI
(`send`, `pipe`) is done, as are SDKs for JS/TS, Python and PHP with the conformance suite
(`spec/conformance/`, runner `glint conformance`).

M2 in progress: MCP server (`/mcp`, `glint mcp`), control plane (pause/resume from UI, MCP, API),
`glint.pause()` in the JS SDK and the Claude Code hooks adapter
(`glint hook`, `docs/mappings/claude-code.md`) are done.

`glint install` / `uninstall` / `doctor` (`crates/glint-cli/src/install.rs`, agents in `agents.rs`): glintd as a login
service and a zero-config global `glint()` for PHP (auto_prepend_file), Python (.pth → builtins)
and Node (NODE_OPTIONS --import); the SDKs are embedded with `include_str!`, so SDK edits ship with
the next `glint` build. Test it with `HOME=<tmp>` plus `--php-ini-dir` / `--shell-profile`, never
against the real home.

Desktop app in `app/` (Tauri 2, its own Cargo workspace so `cargo test --workspace` never builds
it): tray state, notifications for new failures and pauses, ⌥⌘G, glintd autostart; window shows
glintd's UI. `app/bundle-macos.sh` makes `target/release/Glint.app` without tauri-cli; icons come
from `app/icons/make.py`. The CLI lives in `Contents/Resources/bin` because `glint` and `Glint`
collide on case-insensitive file systems.

Agent activity log (`glint-core::agents`): every MCP tool call and hook action is recorded in the
store next to events (same retention); `GET /v1/agents/stats`, `glint stats`, the Agents view (g)
and "read by" marks in a run. `examples/agent-eval/` is an opt-in A/B experiment (costs money,
never run by tests).

`scripts/clean-mac-check.sh` (CI: `.github/workflows/clean-mac.yml`) installs the DMG like a new
tester (quarantine flag on), runs setup through glintd and checks PHP, Python, Node, Claude Code,
agent detection, `glint report` and uninstall; locally it runs in a sandbox HOME on a spare port.
"Send diagnostics" (`POST /v1/report`) and `glint report` share `glint-server::report`.

Framework integrations with no code, hooked in by the global shims: Laravel (`sdks/php/src/Laravel.php`,
via an `env()` defined before Laravel's), Django, Flask, FastAPI, SQLAlchemy, requests and httpx
(`sdks/python/glint/_frameworks.py`, via a `sys.meta_path` hook), Node servers, Express, Next.js,
fetch, pg, mysql2 and node:sqlite (`sdks/js/src/frameworks.js`), plus a Vite plugin
(`sdks/js/src/vite.js`). One run per request with a `core.measure` root; manifest in
`docs/mappings/frameworks.md`; `python3 examples/frameworks/check.py` checks them live.

`glint expect` / MCP `glint_expect` / `POST /v1/expect` share `glint-core::expect` (term syntax, evaluator, async runner).

M3 in progress: OTLP receiver on `:4318` mapping GenAI spans to `llm.call`
(`docs/mappings/otlp.md`), LLM card in the UI, MCP `glint_llm_calls`.

Crates that exist: `glint-protocol`, `glint-core`, `glint-store-sqlite`, `glint-ingest` (native, otlp, agent hooks),
`glint-server`, `glintd`, `glint-cli` (binary `glint`), `glint-mcp`; SDKs in `sdks/{js,python,php}`. The rest of the layout in ARCHITECTURE §10 is planned, not present.

## Stack

- Rust (stable, edition 2024) workspace in `crates/`. Async: `tokio`. HTTP/WS: `axum`. Store:
  `rusqlite` (bundled, FTS5) behind the `Store` trait. MCP: hand-written in `glint-mcp`
  (ADR 0004). Errors: `thiserror` in libraries,
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
- **New language SDK:** `sdks/<lang>/` with a `conformance.<ext>` program; must pass
  `glint conformance -- <runtime> sdks/<lang>/conformance.<ext>` (20/20) and be added to
  `crates/glint-cli/tests/sdks.rs`. `sdks/js` is the reference implementation. Update
  `spec/support-matrix.yaml`.
- **New framework integration:** `integrations/<framework>/`, builds on the language SDK, emits only
  existing kinds or `x-<framework>.*` kinds, ships a manifest of the kinds it emits.

## Commands

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo run -p glintd                      # http://127.0.0.1:23600, DB at ~/.glint/glint.db
GLINT_DB=/tmp/g.db GLINT_ADDR=127.0.0.1:23700 cargo run -p glintd
curl -d '{"kind":"core.value","payload":{"value":42}}' http://127.0.0.1:23600/v1/events
cargo run -q -p glint-cli -- send 42 '{"a":1}' hello --label demo
npm run dev 2>&1 | cargo run -q -p glint-cli -- pipe --label web
```

`glint pipe` must never break the user's pipeline: output is echoed byte for byte, and when
glintd is down lines are dropped with one warning. `glint send` fails loudly instead.

`ui/index.html` is compiled into the binary (`include_str!`): rebuild `glintd` after editing it.

Low disk? `CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0` keeps `target/` under 1 GB.

## Conventions

- Tests next to code; golden/fixture tests for adapters; conformance tests for SDKs.
- Benchmarks for ingest and store paths in `benches/`; keep within budgets in ARCHITECTURE §12.
- Commit messages: imperative, explain why. One concern per commit.
- Docs are part of the change: protocol, kind or adapter changes update `docs/` in the same commit.
