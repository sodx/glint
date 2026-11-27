# Building from source

You need Rust 1.88 or newer. The zero-setup `glint()` works with PHP 8.0+, Python 3.9+ and Node 18+
(the `php`, `python3` and `node` on your PATH). glintd and the CLI run on macOS and Linux; the
desktop app is macOS only for now.

```bash
cargo build --release -p glintd -p glint-cli && target/release/glint install
glint doctor      # is glintd up, does glint() work in each language, which agents know about Glint
glint uninstall   # removes what install wrote and keeps your data in ~/.glint
```

`glint install` starts glintd at login (launchd or systemd) and makes `glint()` a global in every
PHP, Python and Node program on the machine, with no package, import or configuration. It also
reports their uncaught errors. SDKs send nothing when `NODE_ENV` or `APP_ENV` is `production`.

PHP gets the global through `auto_prepend_file` (an existing prepend file keeps working), Python
through a `.pth` file in the user site-packages, and Node through `NODE_OPTIONS=--import` in your
shell profile. A project that vendors its own copy of the SDK uses that copy. `--only php,node`
and `--dry-run` narrow the install down.

PHP skips the prepend file for a `php -S` router script (`php -S host:port router.php`). Serve a
document root instead (`php -S host:port -t public`), or require the SDK in that project.

To try it without installing:

```bash
cargo run -p glintd                       # the window at http://127.0.0.1:23600
cargo run -q -p glint-cli -- send 42 hello
```

[Back to the README](../README.md)
