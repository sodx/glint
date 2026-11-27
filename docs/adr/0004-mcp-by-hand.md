# ADR 0004 — MCP server written by hand, served over HTTP, bridged to stdio

Status: accepted · 2026-11-27 (supersedes the `rmcp` choice in CLAUDE.md "Stack")

## Context
Agents need to read Glint (runs, events, the last error), wait for new events, and answer
paused programs. MCP is how coding agents (Claude Code, Cursor, Codex, …) take tools. The Rust
SDK crate for MCP has changed its API repeatedly; Glint only needs `initialize`, `ping`,
`tools/list` and `tools/call`.

## Decision
- `crates/glint-mcp` implements those JSON-RPC methods directly, transport-agnostic
  (`McpServer::handle(Value) -> Option<Value>`).
- glintd serves it as Streamable HTTP at `POST /mcp`, answering with plain JSON (no SSE
  stream); `GET /mcp` is 405. The loopback Host/Origin guard applies, as the MCP spec asks.
- `glint mcp` bridges stdio to that endpoint for agents that launch servers as processes, and
  answers with a clear JSON-RPC error when glintd is not running.
- Tools return compact text: one line per event, full payloads only from `glint_event`.

## Consequences
- No dependency churn; the whole server is a few hundred lines with tests.
- No server-initiated messages (progress, sampling, list-changed). `glint_wait_for` is a long
  request instead of a stream. Revisit if a tool needs streaming.
- Protocol revisions are negotiated from a fixed list (`PROTOCOL_VERSIONS`); new revisions need a
  line of code and a test.
