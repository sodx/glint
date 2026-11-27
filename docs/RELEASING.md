# Releasing Glint

Glint for Mac updates itself with the Tauri updater plugin. The code stays in the private repo;
the builds go to the public repo [sodx/glint-releases](https://github.com/sodx/glint-releases),
which holds only release files. Every installed Glint asks for
`https://github.com/sodx/glint-releases/releases/latest/download/latest.json` a few seconds after
it starts and every six hours. When that file names a newer version, Glint shows
"Update to x.y.z…" in the menu-bar menu and a banner in the window. One click downloads the
archive, checks its signature, swaps Glint.app and restarts it.

## How an update reaches glintd and the CLI

glintd and `glint` live inside Glint.app, so the new bundle brings new copies. When the new Glint
starts, it:

- copies its glintd and `glint` over the ones in `~/.glint/bin` when `glint install` put older ones
  there (through a new file and a rename, like `glint install`, so macOS never sees a binary
  changed in place);
- restarts glintd when the running one reports an older version in `/v1/info`: through launchd
  when it runs as the login service, otherwise by stopping it and starting the bundled copy.

It never replaces a newer glintd or CLI, so a developer's own build is left alone. The same
happens after an update made by hand from a disk image.

## The signing key

Updates are signed with a minisign key pair made by `tauri signer generate`:

- the private key is `~/.glint-release/updater.key` (mode 600, no password), outside the repo;
- the public key is in `app/tauri.conf.json` under `plugins.updater.pubkey`.

Back up the private key somewhere safe, such as a password manager. Without it no installed Glint
accepts a new build, and the only way forward is a new key pair and a manual reinstall for every
user. Never commit it.

## What you need once

- the Tauri CLI, which the script runs through `npx --yes @tauri-apps/cli@2` (set `TAURI` to use
  another copy);
- `gh`, signed in with write access to `sodx/glint-releases`.

## Make a release

```bash
app/release.sh 0.1.1 --notes "What changed, in a sentence or two"
```

The script:

1. sets the version in `Cargo.toml` (the workspace), `app/Cargo.toml` and `app/tauri.conf.json`;
2. builds Glint.app and the disk image with `app/bundle-macos.sh --dmg`;
3. packs Glint.app into `Glint-<version>-macos-<arch>.app.tar.gz` and signs it into a `.sig`;
4. writes `latest.json` with the version, the notes, the signature and the download URL.

Before it signs anything, the script checks that the bundled `glint --version` and
`glintd --version` print the new version. Several checkouts sharing one `CARGO_TARGET_DIR` can
overwrite each other's binaries mid-build; if the check fails, run the script again.

Everything lands in `target/release/dist-<version>/`. Look it over, then publish:

```bash
app/release.sh 0.1.1 --notes "What changed" --publish
```

`--publish` creates the GitHub release `v<version>` on `sodx/glint-releases` with the archive, its
signature, the disk image and `latest.json`. GitHub then serves that `latest.json` as the latest
release, and installed copies pick it up within six hours (or at once from "Check for Updates…").
Commit the version change afterwards: `git commit -am "Release 0.1.1" && git tag v0.1.1`.

The build follows the usual settings: `CARGO_TARGET_DIR` and the release profile variables work,
and `GLINT_SKIP_BUILD=1` reuses binaries that are already built.

## Test a release before publishing

Build the old version, then the new one pointed at a local server, and let the old one update
itself. Nothing here touches `/Applications`, `~/.glint` or a running glintd.

```bash
S=$(mktemp -d)
app/bundle-macos.sh && cp -R target/release/Glint.app "$S/Glint.app"       # the old version
GLINT_RELEASE_BASE_URL=http://127.0.0.1:23790 app/release.sh 0.1.1          # the new one
git checkout Cargo.toml app/Cargo.toml app/tauri.conf.json                 # undo the bump
(cd target/release/dist-0.1.1 && python3 -m http.server 23790) &
mkdir -p "$S/home" && HOME="$S/home" TMPDIR="$S" GLINT_APP_ID=local.glint.app.test \
  GLINT_URL=http://127.0.0.1:23711 GLINT_ADDR=127.0.0.1:23711 GLINT_DB="$S/g.db" \
  GLINT_RAY_ADDR=off GLINT_OTLP_ADDR=off \
  GLINT_UPDATE_URL=http://127.0.0.1:23790/latest.json GLINT_UPDATE_NOW=1 \
  "$S/Glint.app/Contents/MacOS/Glint"
```

The test variables:

| variable | effect |
|---|---|
| `GLINT_UPDATE_URL` | ask this `latest.json` instead of GitHub; plain http works on loopback only |
| `GLINT_UPDATE_NOW=1` | install an update as soon as it is found, without a click |
| `GLINT_APP_ID` | another app identifier, so this copy runs beside your own Glint |

After a few seconds the copy downloads the update, restarts as the new version, and the glintd on
port 23711 reports the new version in `/v1/info`. Leave out `GLINT_UPDATE_NOW` to try the menu
item and the banner by hand.
