# Framework examples

A small app per framework, with nothing in it about Glint: the global shims hook in on their own.
`check.py` runs each one against a sandbox glintd and checks what arrives: a run per request with
its summary, SQL with the app line, the exception with its request, logs, and failed outgoing calls.

```bash
python3 examples/frameworks/check.py                  # all whose toolchain is installed
python3 examples/frameworks/check.py laravel express  # some
python3 examples/frameworks/check.py --keep /tmp/fw   # keep the installs and reuse them next time
```

| Folder | Needs | Installs into the work folder |
| --- | --- | --- |
| `laravel/` | php, composer | a fresh `laravel/laravel` app with these files copied over it |
| `django/`, `flask/`, `fastapi/` | python3 | a venv with the framework |
| `express/` | node, npm (PostgreSQL optional, for the pg check) | express, pg |
| `nextjs/` | node, npm | next, react |
| `vite/` | node, npm | vite |

It needs a glintd binary (`cargo build --release -p glintd`, or `--glintd PATH`), and never
touches your own glintd, `~/.glint` or shell profile.
