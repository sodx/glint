# ADR 0003 — Rust core, one daemon, Tauri shell, web UI

Status: accepted · 2026-11-27

## Context
Dev tools that sit open all day must be light. Electron apps cost 150+ MB and a lot of RAM. We also
want a headless mode (servers, remote dev boxes) and an MCP server that works without the GUI.

## Decision
- One Rust daemon (`glintd`) owns ingest, store, bus, control plane, MCP and serves the UI.
- The desktop app is a Tauri 2 shell embedding the same server; the UI is a web app usable both in
  Tauri and in a browser.
- SQLite (WAL, FTS5) is the only store, behind a `Store` trait.

## Consequences
- Small binary, low idle memory, one code path for desktop and headless.
- UI work is TypeScript, not Rust; contributors need both.
- Single-file store is easy to inspect and back up but not built for long retention — fine for a
  dev-time tool (see non-goals).
