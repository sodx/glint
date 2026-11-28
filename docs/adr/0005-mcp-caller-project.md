# 0005: MCP tools read the caller's project, learned from its workspace roots

## Status
Accepted. Amends 0004 ("no server-initiated messages") for one request.

## Context
Tools that take a `project` read every project when an agent leaves it out, so an agent working in
one repository could be handed another repository's error. `glint mcp` (stdio) knows its folder
and sends `X-Glint-Project`, but Claude Code, Cursor and VS Code connect over HTTP with one
user-level URL and send nothing about their folder. Claude Code and VS Code declare the MCP
`roots` capability, which exists for this: the server asks, the client answers with its workspace
folders. Per-workspace URLs would need a config entry per project, and guessing from recent hook
activity breaks with two sessions open.

## Decision
- The first tool call of a session with no known project is answered over SSE when the client
  accepts it: a `roots/list` request, then the reply. The client answers by POST; the first root's
  git repository is the session's project.
- Before that come the tool's `project` argument, `X-Glint-Project`, and `/mcp?project=`.
- Every reply names its project and how it is known. With none known and events from several
  projects, read tools list the projects instead of mixing them. `project="*"` reads all.

## Consequences
- One extra round trip per session, only when nothing else names the project.
- The roots answer is kept in memory; after a glintd restart the session is asked again.
- A client that ignores server requests costs one 2 s wait per session, then falls back.
- `GET /mcp` stays 405: the request rides on the tool call's own response stream.
