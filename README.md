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
npm run dev 2>&1 | glint pipe
```

## Why

- **Every language.** Tiny native protocol (POST JSON) plus what you already emit: OpenTelemetry,
  Ray-compatible payloads, stdin, log files.
- **AI-native.** LLM calls and agent steps are first-class events. MCP tools let agents query runs,
  read the last error, wait for an event and resume a paused program.
- **Local and private.** One daemon, one SQLite file, loopback only.
- **Small and fast.** Rust core, Tauri app.

## Status

Design phase. Start with [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) and
[docs/PROTOCOL.md](docs/PROTOCOL.md).
