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

Set it up once and forget it: `glint install` starts glintd at login (launchd / systemd) and makes
`glint()` a global in every PHP, Python and Node program on the machine — no package, no import,
no config. SDKs stay off when `NODE_ENV` / `APP_ENV` is `production`.

```bash
cargo build --release -p glintd -p glint-cli && target/release/glint install
glint doctor        # is glintd up, does glint() work in each language?
glint uninstall     # removes everything install wrote; keeps ~/.glint
```

PHP gets it through `auto_prepend_file` (an existing prepend file keeps working), Python through a
`.pth` file in the user site-packages of the `python3` on PATH, Node through `NODE_OPTIONS=--import`
in your shell profile. `--only php,node` and `--dry-run` narrow it down. A project's own copy of the
SDK always wins over the global one. One gap: PHP never prepends to a `php -S` *router* script
(`php -S host:port router.php`, also what `artisan serve` runs); serve a document root instead
(`php -S host:port -t public`) or require the SDK in that project.

`python3 examples/e2e/run.py` drives a PHP shop, a Python worker and a Node API (with a pause)
through the global `glint()` and checks what glintd stored.

The desktop app (macOS for now) puts Glint in the menu bar: the spark turns red when something
fails and amber when a program waits for you, a notification tells you which, and ⌥⌘G opens the
window from anywhere.

```bash
app/bundle-macos.sh --install     # builds Glint.app (with glintd inside) into ~/Applications
```

Existing Ray client libraries work unchanged: glintd also listens on Ray's port `23517`.

Connect your coding agent:

```bash
claude mcp add --transport http glint http://127.0.0.1:23600/mcp   # or: claude mcp add glint -- glint mcp
```

It can then read the last error with its context, wait for the next run after an edit, resume
paused programs and leave notes next to events.

Trace LLM calls with zero Glint code: point any OpenTelemetry exporter at Glint and GenAI spans
(OpenLLMetry, vendor instrumentors, the GenAI semantic conventions) become LLM cards with the
transcript, tokens and latency:

```bash
OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:4318 python app.py
```

See what the agent does, next to what your program does:

```bash
glint hook --print-config   # Claude Code hooks → paste into .claude/settings.json
```

## Status

M0 and part of M1 are done (daemon, live UI, Ray-compatible ingest, `glint send` / `glint pipe`). Start with [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) and
[docs/PROTOCOL.md](docs/PROTOCOL.md).
