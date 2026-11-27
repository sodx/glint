# Glint — Architecture

Glint is a local, language-agnostic debug and observability desk for developers **and their AI agents**.
You send values, logs, queries, exceptions, timings and LLM calls from any program; Glint shows them as a
live, searchable feed on your desktop — and exposes the same feed to coding agents over MCP, so an agent can
read what your program actually did instead of guessing.

Think "`dd()` / `console.log` that lands in a dedicated window", plus "local Langfuse", plus "a debugger
an agent can drive".

---

## 1. Design goals

| # | Goal | Consequence |
|---|------|-------------|
| G1 | **Every language, every framework — eventually.** | The *protocol* is the product. Clients are thin and replaceable; the core never knows about a language. |
| G2 | **Zero-install for common stacks.** | Accept protocols people already emit: OpenTelemetry (OTLP), Ray-compatible payloads, plain stdin/NDJSON, log files. |
| G3 | **AI-native, not AI-bolted-on.** | Agents are first-class consumers (MCP) *and* producers (LLM-call and agent-step event kinds). |
| G4 | **Local-first, private by default.** | Binds to loopback, data in one SQLite file, nothing leaves the machine unless a user turns on a remote model. |
| G5 | **Fast and small.** | Rust core, Tauri shell, single binary daemon. Ingest ≥ 50k events/s, UI stays live at 1k events/s. |
| G6 | **Unknown is fine.** | Any payload kind renders (generic JSON tree). New kinds never break old UIs or old clients. |
| G7 | **Data, not pictures.** | Clients send structured values; the viewer renders. Never pre-rendered HTML as the primary form (Ray's main limitation — see `docs/research/ray.md`), so events stay searchable, diffable and readable by agents. |
| G8 | **Open and specified.** | Open source, normative spec + conformance suite. Ray has neither; its app is closed and its protocol is learned by proxying traffic. |

Positioning note (2026-11): Ray 4 is a native Rust app and Ray 3 ships an MCP server, so "fast" and
"has MCP" are table stakes, not differentiators. Glint competes on G1, G3 (tracing LLMs and agents,
not just showing their output), G7 and G8.

Non-goals (for now): production APM, multi-tenant cloud, long-term retention, distributed tracing UI at
Jaeger scale. Glint is a *dev-time* tool.

---

## 2. The big picture

```
 ┌──────────────────────────── producers ────────────────────────────┐
 │  glint SDKs        Ray clients      OTel SDKs / OpenLLMetry        │
 │  (php, js, py,     (spatie/ray,     (any lang, any LLM framework)  │
 │   go, rust, ...)    node-ray, ...)                                 │
 │  `glint` CLI / pipe     log-file tail     Claude Code / agent hooks│
 └───────┬──────────────────┬───────────────────┬──────────────┬─────┘
         │ Glint native     │ Ray protocol      │ OTLP/HTTP    │ stdin, files, hooks
         ▼                  ▼                   ▼              ▼
 ┌──────────────────────── glint-ingest (adapters) ───────────────────┐
 │  native  │  ray-compat  │  otlp  │  pipe  │  tail  │  agent-hooks  │
 │      every adapter does one thing: foreign bytes  →  Envelope      │
 └───────────────────────────────┬────────────────────────────────────┘
                                 ▼
 ┌────────────────────────────── glint-core ──────────────────────────┐
 │  Envelope model · normalisation · redaction · enrichment           │
 │  Bus (tokio broadcast)  ──►  Store (SQLite + FTS5)                 │
 │  Control plane: pause/resume, breakpoints, "wait for event"        │
 └──────┬─────────────────────────┬─────────────────────────┬─────────┘
        ▼                         ▼                         ▼
 ┌─────────────┐          ┌──────────────┐          ┌────────────────┐
 │  glint-ui   │          │  glint-mcp   │          │  glint-ai       │
 │  web UI in  │          │  MCP server  │          │  summarise,     │
 │  Tauri app  │          │  for agents  │          │  group, explain │
 │  or browser │          │  (stdio/HTTP)│          │  (pluggable LLM)│
 └─────────────┘          └──────────────┘          └────────────────┘
```

One process (`glintd`) owns ingest, store, bus, control plane and MCP. The desktop app (`Glint.app`)
embeds the same daemon; a headless user can run `glintd` and open the UI in a browser. There is exactly
one source of truth: the store, fed by the bus.

---

## 3. Core concept: the Envelope

Everything that enters Glint becomes an **Envelope**. Adapters translate *into* it; UI, MCP and AI read
*from* it. This is the only internal contract that matters.

```jsonc
{
  "v": 1,                          // protocol major version
  "id": "01JD3Z8Q...",             // ULID, sortable, client- or server-assigned
  "ts": "2026-11-27T10:15:03.123456Z",
  "run": "01JD3Z7A...",            // one process run / request / test / agent session
  "parent": null,                  // nest events: span → children, agent step → tool calls
  "project": "shop-api",           // derived from cwd/git root if not given
  "origin": {
    "lang": "php", "runtime": "8.3", "framework": "laravel",
    "sdk": "glint-php/0.1.0",
    "file": "app/Http/Controllers/OrderController.php", "line": 42, "fn": "store",
    "host": "mbp", "pid": 81234, "thread": "main"
  },
  "kind": "sql.query",             // namespaced payload kind (see §4)
  "payload": { "sql": "select ...", "bindings": [1], "duration_ms": 3.4, "conn": "mysql" },
  "meta": {
    "color": "orange", "label": "slow?", "screen": "checkout",
    "tags": ["orders"], "level": "debug"
  }
}
```

Rules:

- `kind` + `payload` is open. The core validates the envelope, **not** the payload (except for known
  kinds, which get typed parsing and richer rendering).
- `run` is the grouping primitive. A web request, a CLI invocation, a test case, an agent session —
  all are runs. The UI and MCP are run-oriented ("show me the last run that threw").
- `parent` gives trees: OTel spans, agent steps → tool calls → LLM calls, `measure` blocks.
- Events are **mutable by reference**: a later `core.update { target, meta?, hidden?, removed? }`
  changes how an earlier event is shown (color, label, size, hide, remove) without creating a feed
  item. This is how fluent chains (`glint(x)->red()->label('a')`) and Ray's same-uuid commands map in.
  Updates are stored, so history stays truthful; the UI shows the folded result.
- `core.html` is untrusted: rendered sanitised in an isolated frame, no scripts, no `file://`.
- Size cap per envelope (default 1 MiB); large blobs go to a content-addressed side store and the
  payload carries a `blob://sha256` reference.

---

## 4. Payload kinds — the extension surface

Kinds are namespaced strings. The namespace says who owns the schema.

| Namespace | Owner | Examples |
|-----------|-------|----------|
| `core.*` | Glint, stable | `core.value`, `core.log`, `core.text`, `core.html`, `core.markdown`, `core.mermaid`, `core.table`, `core.json`, `core.exception`, `core.trace`, `core.measure`, `core.separator`, `core.clear`, `core.screen`, `core.update` |
| `sql.*` | Glint | `sql.query`, `sql.transaction` |
| `http.*` | Glint | `http.request`, `http.response`, `http.client_call` |
| `llm.*` | Glint, AI | `llm.call` (model, messages, tools, usage, cost, latency), `llm.embedding`, `llm.stream_chunk` |
| `agent.*` | Glint, AI | `agent.session`, `agent.step`, `agent.tool_call`, `agent.tool_result`, `agent.decision` |
| `ctl.*` | Glint | `ctl.pause`, `ctl.breakpoint`, `ctl.ack` — control plane, never stored as feed items |
| `x-<vendor>.*` | anyone | `x-laravel.job`, `x-django.signal`, `x-mycompany.order` |

Each known kind has:

1. a **JSON Schema** in `spec/kinds/<kind>.schema.json` (single source of truth),
2. a **Rust type** generated from or checked against that schema,
3. a **renderer** in the UI (`ui/src/renderers/<kind>.tsx`),
4. optional **MCP projection** — how the kind is summarised for an agent in few tokens.

Unknown kinds fall back to: JSON tree renderer, generic MCP projection, full-text indexing of string
leaves. That is what makes G6 true and lets frameworks invent kinds before Glint knows them.

**Renderer plugins.** A vendor can ship `x-laravel.*` renderers as a small ES module dropped into
`~/.glint/renderers/`. The UI loads them into a sandboxed iframe with a fixed props contract. No Rust
change is needed to add a kind end-to-end.

---

## 5. Ingest: how "all languages" actually happens

Supporting every language is not "write 30 SDKs". It is three layers, cheapest first:

### Layer 0 — speak what they already speak (no SDK)

| Adapter | Port / source | What you get for free |
|---------|---------------|-----------------------|
| `otlp` | OTLP/HTTP `:4318` (`/v1/logs`, `/v1/traces`) | Every language with an OpenTelemetry SDK. Spans → trees, logs → `core.log`. **`gen_ai.*` semantic conventions → `llm.call`**, so OpenLLMetry, LangChain, LlamaIndex, Vercel AI SDK, OpenAI/Anthropic instrumentors light up with zero Glint code. |
| `ray-compat` | HTTP `:23517` | Existing Ray client libraries (PHP/Laravel, JS, Python, Ruby, Go, Bash…) work by pointing them at Glint. Data payloads map onto `core.*` / `sql.*`; same-uuid commands become `core.update`. Full mapping and quirks: `docs/research/ray.md` §9. |
| `pipe` | `stdin` of `glint pipe` | `my-app 2>&1 \| glint pipe` — each line becomes `core.log`; JSON lines are parsed; level/timestamp sniffed. Works for literally anything. |
| `tail` | files / globs | `storage/logs/laravel.log`, `*.log`. Multiline stack traces folded into `core.exception`. |
| `agent-hooks` | HTTP + small hook scripts | Claude Code / Codex / Cursor hook events → `agent.*`. Glint becomes a live view of what the agent is doing next to what the app is doing. |

### Layer 1 — the native protocol (tiny SDKs)

The native protocol (see `PROTOCOL.md`) is deliberately trivial: **POST a JSON array of envelopes to
`http://127.0.0.1:23600/v1/events`**. Everything except `kind` and `payload` is optional. A
conforming client is ~50–150 lines in any language, so:

- SDKs are **thin**: build envelope, capture caller file/line, fire-and-forget POST, never throw,
  never block the host program for more than a few ms (bounded queue + background flush).
- A **conformance suite** (`spec/conformance/`) — fixtures + a runner (`glint conformance <cmd>`)
  that starts a recording server, executes the SDK's test program and diffs received envelopes
  against expected ones. A language is "supported" when it passes conformance. This is how
  community SDKs stay honest.
- **Generated types**: `spec/` JSON Schemas generate typed builders per language
  (`quicktype`/`typify`/custom templates) so kinds stay in sync.

### Layer 2 — framework integrations (on top of SDKs)

Integrations live in `integrations/<framework>/` and only *emit* existing kinds:

- Laravel / Symfony: query listener → `sql.query`, exception handler → `core.exception`, job events → `x-laravel.job`.
- Django / FastAPI / Flask: DB execute wrapper, middleware → `http.request`, logging handler.
- Express / Nest / Next.js: middleware, Prisma/Drizzle query hooks, `console` patch (opt-in).
- Rails, Spring, ASP.NET, Phoenix, Gin/Echo, Axum/Actix: same pattern.

Each integration declares which kinds it emits in a manifest, so the UI can offer "framework-aware"
filters ("show me N+1 candidates in this request").

**Support matrix is data, not prose.** `spec/support-matrix.yaml` lists language × layer × status and
is rendered into docs and into the app's "connect your project" screen.

---

## 6. Store

- **SQLite** (WAL mode) at `~/.glint/glint.db`, one file, copyable, inspectable.
- Tables: `runs`, `events` (envelope columns + `payload` as JSONB), `events_fts` (FTS5 over extracted
  text), `blobs` (content-addressed), `annotations` (user/agent notes, pins).
- Retention by count and age (defaults: 500k events / 7 days), per-project overrides, pinned runs
  never expire.
- Writes are batched from the bus (one transaction per ≤ 5 ms or ≤ 1000 events). Readers never block
  ingest.
- `Store` is a trait in `glint-core`; SQLite is the only implementation for now. (A DuckDB backend for
  heavy analytics is a possible later addition, not a goal.)

---

## 7. Control plane: making the program wait

Ray-style `pause()` and debugger-ish breakpoints, generalised so an **agent** can use them too:

1. SDK sends `ctl.pause { id, reason }` and long-polls `GET /v1/ctl/<id>`.
2. UI shows "paused at file:line" with *Continue* / *Continue with value*.
3. MCP exposes the same: an agent can see that a program is paused, inspect the events of that run,
   and resume it — optionally injecting a value (e.g. a feature flag, a mocked response).
4. Timeouts on the SDK side (default 5 min) so a forgotten pause never hangs CI. Pauses are disabled
   when `CI=true` unless forced.

`wait_for` is the agent-facing twin: "block until an event matching this filter arrives in project X"
— the key primitive for agentic edit → run → observe loops.

---

## 8. AI layer

Two directions, deliberately separate crates so the core never depends on an LLM.

### 8.1 Agents *read* Glint — `glint-mcp`

MCP server (Rust `rmcp`), stdio for local agents and streamable HTTP on `/mcp`. Tools are designed for
**low token cost**: every tool returns compact projections first, full payloads only on request.

| Tool | Purpose |
|------|---------|
| `glint_runs` | List recent runs (project, status, error count, duration). |
| `glint_events` | Query events: filter by run/kind/level/text/time; returns one-line projections. |
| `glint_event` | Full envelope by id (payload truncated to budget, `more` cursor). |
| `glint_last_error` | Most recent exception with stack, request context and the 20 events before it. |
| `glint_wait_for` | Block until a matching event arrives (with timeout). |
| `glint_pauses` / `glint_resume` | See paused programs, resume them, optionally inject a value. |
| `glint_llm_calls` | LLM calls with tokens, cost, latency; diff two calls' prompts. |
| `glint_annotate` | Pin or note an event/run ("root cause here") — visible to the human in the UI. |
| `glint_clear` | Mark a fresh start ("everything after this is my experiment"). |

MCP **resources** expose `glint://run/<id>` and `glint://event/<id>` so agents can attach them as
context. A generated `AGENTS.md` snippet tells agents how to use Glint in a project.

### 8.2 Glint *uses* AI — `glint-ai` (optional)

Off by default. Provider trait with Anthropic, OpenAI-compatible and local (Ollama/llama.cpp)
backends. Features:

- **Explain this exception** — stack + surrounding run events → probable cause and fix location.
- **Group** — cluster similar exceptions/log lines across runs (embeddings, local by default).
- **Summarise run** — "what happened in this request" for long feeds.
- **Prompt diff** — highlight what changed between two `llm.call`s that behaved differently.

Redaction (`glint-core::redact`) runs before anything is sent to a remote model: secrets patterns,
configurable field denylist, and a per-project "never send" switch.

### 8.3 LLM and agent observability

`llm.call` and `agent.*` are first-class kinds with dedicated renderers: chat transcript view, tool
calls inline, token/cost/latency bar, streaming reassembly. Cost is computed from a bundled,
updatable price table. Combined with `run`/`parent` this gives a local, zero-setup equivalent of an
LLM tracing platform — next to your app's SQL and exceptions in the same timeline.

---

## 9. UI

- Web stack (Solid or Svelte + TypeScript + Vite), served by `glintd` at `http://127.0.0.1:23600/` and
  embedded in the Tauri shell. One UI codebase for both.
- Live feed over WebSocket (`/v1/stream`) with server-side filters; virtualised list; keyboard-first.
- Views: **Feed** (default, Ray-like), **Runs** (request/test/session list), **Tree** (spans,
  agent steps), **LLM** (calls table + transcript), **Search** (FTS + structured filters).
- Click `file:line` → open in editor (configurable URL scheme: `vscode://`, `phpstorm://`, `zed://`,
  `cursor://`, `idea://`).
- Desktop extras via Tauri: tray icon with error badge, always-on-top mini feed, global hotkey,
  native notifications for exceptions and pauses.

---

## 10. Repository layout

```
glint/
├── CLAUDE.md                 # instructions for coding agents working on Glint
├── README.md
├── docs/
│   ├── ARCHITECTURE.md       # this file
│   ├── PROTOCOL.md           # wire protocol v1 — normative
│   ├── ROADMAP.md
│   └── adr/                  # architecture decision records
├── spec/                     # language-neutral truth; everything else derives from it
│   ├── envelope.schema.json
│   ├── kinds/*.schema.json
│   ├── conformance/          # fixtures + expected envelopes per SDK scenario
│   └── support-matrix.yaml
├── crates/
│   ├── glint-protocol/       # Envelope + known kinds as Rust types, schema checks, versioning
│   ├── glint-core/           # bus, store trait, normaliser, redaction, control plane
│   ├── glint-store-sqlite/   # SQLite + FTS5 implementation of Store
│   ├── glint-ingest/         # adapters: native, ray, otlp, pipe, tail, agent-hooks
│   ├── glint-mcp/            # MCP server
│   ├── glint-ai/             # optional LLM features (feature-gated)
│   ├── glint-server/         # axum app wiring: HTTP, WS, static UI
│   ├── glintd/               # daemon binary
│   └── glint-cli/            # `glint` binary: pipe, send, tail, conformance, mcp, open
├── app/                      # Tauri shell embedding glint-server
├── ui/                       # web UI (TS)
├── sdks/                     # first-party thin clients: php, js, python, go, rust, ruby, ...
└── integrations/             # framework packages built on sdks/
```

Dependency direction is strictly downward:
`glint-protocol ← glint-core ← {store, ingest, mcp, ai} ← glint-server ← {glintd, app, cli}`.
`glint-core` never depends on axum, Tauri, SQLite or any LLM client.

---

## 11. Ports and security

| Port | Purpose | Default |
|------|---------|---------|
| `23600` | native API, UI, WS stream, MCP-over-HTTP | on |
| `23517` | Ray-compatible ingest | on (off if busy) |
| `4318` | OTLP/HTTP ingest | on (off if busy — a real collector may own it) |

- Loopback only by default. Binding to LAN (for Docker/VMs/phones) requires explicit config and
  enables a bearer token.
- Docker helper: SDKs try `host.docker.internal` when `127.0.0.1` fails.
- Browser CSRF guard: native API rejects requests with an `Origin` header not in the allowlist, so a
  random web page cannot write to or read from Glint.
- MCP tools that mutate (resume, inject value, clear) can be set to "ask in UI first".

---

## 12. Performance budget

| Path | Budget |
|------|--------|
| SDK call overhead on host program | < 50 µs enqueue, never blocks on network |
| Ingest → visible in UI | < 50 ms p99 |
| Sustained ingest | ≥ 50k envelopes/s on a laptop |
| Daemon idle RSS | < 40 MB |
| App bundle | < 20 MB |

Benchmarks live in `crates/*/benches` and run in CI; regressions over 10% fail the build.

---

## 13. Versioning and compatibility

- Protocol is versioned by `v` on every envelope; the server accepts `v` = current and current-1.
- Kinds evolve additively. Breaking a kind means a new kind name (`llm.call` → `llm.call2`) — never
  a silent shape change.
- `GET /v1/info` advertises server version, supported `v`, known kinds and enabled adapters so SDKs
  can degrade gracefully.
