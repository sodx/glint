# Product

<!-- impeccable:product-schema 1 -->

## Platform

web

The UI is one web app, served by `glintd` in a browser tab and embedded in a Tauri desktop shell
(planned, M4) with a tray icon.

## Users

Developers debugging their own programs on their own machine, and the coding agents working
alongside them. Both scenes matter equally:

- **Hand debugging.** The developer runs code, glances at Glint for a few seconds to see what
  arrived and where it failed, goes back to the editor.
- **Watching an agent.** An agent (Claude Code and others) edits and runs the program; the
  developer follows in Glint what the program did and what the agent did, and sometimes answers
  a paused program.

Agents are users too, through MCP; they never see this UI, but what the human sees and what the
agent reads must be the same events.

## Product Purpose

A local, language-agnostic debug desk: any program sends values, logs, SQL, exceptions, timings,
LLM calls and agent steps; Glint shows them as one live, searchable timeline. Success, in order
of weight from the user:

1. See the failure fast, and what led up to it in the same run.
2. Read values as structured data (search, filter, expand), not as pre-rendered HTML.
3. See what the agent and the LLM did: steps, tool calls, prompts, tokens.
4. Control pauses: stop a program, inspect, continue with a value.

## Positioning

Ray (myray.app) is the reference neighbour: closed, paid, PHP-first, values arrive as HTML, its
MCP lets agents write into the window. Glint differs on: structured data, an open protocol with a
conformance suite, any language (native SDKs, Ray-compatible clients, OpenTelemetry, stdin), LLM
and agent tracing in the same timeline, and pauses that a human or an agent can answer.

## Operating Context

- Window sizes: a narrow column docked beside the editor (~400–600 px) **and** a full browser tab
  or window; the layout must work in both. A tray icon with an error badge when the desktop shell
  ships.
- Sessions are long-lived: the window stays open for hours while events stream in, from several
  projects and runs at once.
- Events arrive in bursts (a request, a test run, an agent turn) and can be large (dumps, stack
  traces, LLM transcripts).
- Keyboard-first users; open-in-editor links (`vscode://` and others) are part of the flow.

## Capabilities and Constraints

- Event kinds the UI must render: `core.value/log/text/json/html/markdown/mermaid/table/exception/
  trace/measure/separator/screen/clear/update/span`, `sql.query`, `llm.call`,
  `agent.session/step/tool_call/tool_result/permission`, `ctl.pause/resume`, and any unknown
  `x-*` kind as a JSON tree.
- Grouping concepts: **project** (repo), **run** (process / request / test / agent session),
  **parent** (span trees, agent step → tool call). `core.update` changes an earlier event's
  colour, label, size, visibility or adds an agent note.
- Untrusted HTML renders sandboxed; no scripts.
- Data comes from the daemon's API (`/v1/events`, `/v1/runs` via MCP, `/v1/stream` WebSocket,
  `/v1/ctl`). The UI today is a single dependency-free HTML file; the roadmap moves it to Vite +
  TypeScript.
- Local-only, loopback; nothing leaves the machine.

## Brand Commitments

Name: **Glint** ("a brief flash of light": a value flashing up in its own window). No logo,
palette or typeface has been committed yet.

Visual-clarity reference named by the user: **Ray's app** ("what Ray has is pretty good") — a calm
vertical list of entries, each with a coloured edge, a small label, the value, and file:line
quietly at the right; little chrome; screens to start fresh. Match that ease of reading; do not
copy its look.

## Evidence on Hand

Real event shapes from verified runs: spatie/ray 1.48.1 dumps, Node and Python SDK output, a real
Claude Code session, OpenTelemetry Python 1.45.2 GenAI spans. No users, testimonials, benchmarks
or pricing exist; do not invent them.

## Product Principles

0. **Answer first.** The first thing on screen answers one question in one line: is everything
   fine, and if not, what failed, where, and how long ago — plus any program waiting for a human.
   Everything else (other runs, filters, densities, payloads) is one click or one key away,
   never on screen by default. A first viewport that makes the user read a dashboard to learn
   whether something broke has failed. (Confirmed by the user, 2026-11-27, after a round of
   overloaded sketches.)
1. The failure is the headline: errors and pauses outrank everything else on screen.
2. One timeline for program and agent: never separate tools for "my code" and "the AI".
3. Data, not pictures: every value is searchable, collapsible and copyable.
4. Glanceable in seconds, explorable in minutes: dense when scanning, deep on demand.
5. The window is a guest beside the editor: it must not demand attention it hasn't earned.

## Accessibility & Inclusion

Keyboard-complete operation (developers live on the keyboard); WCAG AA contrast in both light
and dark; meaning never by colour alone (level and status also in text).
