# Working on Glint

```bash
cargo test --release --workspace                 # unit and integration tests
python3 examples/e2e/run.py                      # a PHP shop, a Python worker and a Node API through the global glint()
python3 examples/frameworks/check.py             # each framework example against a sandbox glintd
scripts/clean-mac-check.sh                       # a new tester on a clean Mac, in a sandbox HOME
app/bundle-macos.sh --install                    # builds Glint.app, with glintd inside, into ~/Applications
```

`app/release.sh` builds, signs and publishes a release that installed apps pick up
([docs/RELEASING.md](RELEASING.md)). `node docs/assets/shoot.mjs` remakes the pictures in this
README from a running glintd.

How it is built: [docs/ARCHITECTURE.md](ARCHITECTURE.md). The wire format and API:
[docs/PROTOCOL.md](PROTOCOL.md). Adding a language: the Docs view, and
[spec/conformance/README.md](../spec/conformance/README.md).

[Back to the README](../README.md)
