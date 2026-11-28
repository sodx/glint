# Building from source

You need Rust 1.88 or newer. The zero-setup `glint()` works with PHP 8.0+, Python 3.9+ and Node
18.19+ or 20.6+ (the `php`, `python3` and `node` on your PATH). An older PHP or Node is skipped,
not changed. glintd and the CLI run on macOS and Linux; the desktop app is macOS only for now.

```bash
cargo build --release -p glintd -p glint-cli && target/release/glint install
glint doctor      # is glintd up and run by the service, does glint() work in each language, which
                  # other PHP and Python versions are not covered (and how), which agents know Glint
glint uninstall   # removes what install wrote (listed in ~/.glint/installed.json), keeps your data
```

`glint install` starts glintd at login (launchd or systemd) and makes `glint()` a global in every
PHP, Python and Node program on the machine, with no package, import or configuration. It also
reports their uncaught errors. SDKs send nothing when `NODE_ENV` or `APP_ENV` is `production`.

PHP gets the global through `auto_prepend_file`, which points at a small `glint-prepend.php` next
to `99-glint.ini`; it loads `~/.glint/sdk/php` only while that exists and only on PHP 8.0+, and an
existing prepend file keeps working. Python gets it through a `.pth` file in the user
site-packages, and Node through `NODE_OPTIONS=--import` in your shell profile (`$ZDOTDIR/.zshrc`
when ZDOTDIR is set), set only while `~/.glint/sdk/js/global.mjs` exists. A project that vendors
its own copy of the SDK uses that copy. `--only php,node` and `--dry-run` narrow the install down.
To cover another PHP or Python, put it first on PATH and run `glint install --only php` (or
`python`) again; `glint doctor` prints the exact command for each one it finds.

The service step stops a glintd already on the port (it asks `/v1/info` for the pid), starts the
login service and checks that it answers with the new version. The service sets
`GLINT_WAIT_FOR_PORT=1`, so a glintd it starts while another holds the port waits for the port
instead of failing; a glintd you start by hand fails at once and names who holds the port.
Test install changes with a sandbox `HOME`, `--php-ini-dir`, `--shell-profile` and
`GLINT_SKIP_SERVICE_START=1`, never against your real home.

PHP skips the prepend file for a `php -S` router script (`php -S host:port router.php`). Serve a
document root instead (`php -S host:port -t public`), or require the SDK in that project.

To try it without installing:

```bash
cargo run -p glintd                       # the window at http://127.0.0.1:23600
cargo run -q -p glint-cli -- send 42 hello
```

[Back to the README](../README.md)
