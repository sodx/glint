# Glint

A local debug desk for developers **and their AI agents**.

Send anything from any program — values, logs, SQL queries, exceptions, timings, LLM calls, agent
steps — and watch it land in a live, searchable feed on your desktop. Your coding agent sees the same
feed over MCP, so it can read what your program actually did instead of guessing.

```php
glint($order)->label('before save')->orange();
```
```python
glint(user, request.headers).pause()
```
```bash
npm run dev 2>&1 | glint pipe --label web     # output still prints; Glint gets levels, JSON logs, stack traces
glint send '{"user":{"id":7}}' --label checkout
```

## Why

- **Every language.** Tiny native protocol (POST JSON) plus what you already emit: OpenTelemetry,
  Ray-compatible payloads, stdin, log files.
- **AI-native.** LLM calls and agent steps are first-class events. MCP tools let agents query runs,
  read the last error, wait for an event and resume a paused program.
- **Local and private.** One daemon, one SQLite file, loopback only.
- **Small and fast.** Rust core, Tauri app.

## Try it

```bash
cargo run -p glintd                     # UI at http://127.0.0.1:23600
cargo run -q -p glint-cli -- send 42 hello
```

Existing Ray client libraries work unchanged: glintd also listens on Ray's port `23517`.

## Status

M0 and part of M1 are done (daemon, live UI, Ray-compatible ingest, `glint send` / `glint pipe`). Start with [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) and
[docs/PROTOCOL.md](docs/PROTOCOL.md).
