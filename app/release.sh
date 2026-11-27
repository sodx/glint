#!/bin/sh
# Makes a Glint release for the updater and, with --publish, puts it on GitHub.
#
#   app/release.sh 0.1.1 --notes "What changed"            # build into target/release/dist-0.1.1
#   app/release.sh 0.1.1 --notes-file notes.md --publish   # … and create the GitHub release
#
# Steps: set the version in Cargo.toml, app/Cargo.toml and app/tauri.conf.json; build Glint.app
# and its disk image (app/bundle-macos.sh --dmg); pack Glint.app into a .app.tar.gz; sign it with
# the updater key; write latest.json. --publish then runs `gh release create` on the public
# releases repo, and every installed Glint sees the update within six hours.
#
# Environment:
#   GLINT_UPDATER_KEY       private key file   (default ~/.glint-release/updater.key)
#   TAURI_SIGNING_PRIVATE_KEY_PASSWORD         (default empty: the key has no password)
#   GLINT_RELEASES_REPO     GitHub repo        (default sodx/glint-releases)
#   GLINT_RELEASE_BASE_URL  where the archive is downloaded from
#                           (default https://github.com/$GLINT_RELEASES_REPO/releases/download/v$version)
#   TAURI                   the Tauri CLI      (default `npx --yes @tauri-apps/cli@2`)
#   GLINT_SKIP_BUILD=1      reuse the binaries already built
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

usage() { sed -n '2,6p' "$0" | sed 's/^# \{0,1\}//' >&2; exit 2; }

version=""
notes=""
publish=""
while [ $# -gt 0 ]; do
  case "$1" in
    --notes) notes="$2"; shift 2 ;;
    --notes-file) notes=$(cat "$2"); shift 2 ;;
    --publish) publish=1; shift ;;
    -h|--help) usage ;;
    -*) echo "unknown option $1" >&2; usage ;;
    *) version="$1"; shift ;;
  esac
done
echo "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$' || { echo "give a version like 0.1.1" >&2; usage; }
[ -n "$notes" ] || notes="Glint $version"

key="${GLINT_UPDATER_KEY:-$HOME/.glint-release/updater.key}"
[ -f "$key" ] || { echo "no updater key at $key (see docs/RELEASING.md)" >&2; exit 1; }
repo="${GLINT_RELEASES_REPO:-sodx/glint-releases}"
base="${GLINT_RELEASE_BASE_URL:-https://github.com/$repo/releases/download/v$version}"
tauri="${TAURI:-npx --yes @tauri-apps/cli@2}"

# 1. The version, everywhere it is written.
sed -i '' -E "/^\[workspace\.package\]/,/^\[/ s/^version = \".*\"/version = \"$version\"/" Cargo.toml
sed -i '' -E "1,/^version = / s/^version = \".*\"/version = \"$version\"/" app/Cargo.toml
sed -i '' -E "1,/\"version\":/ s/\"version\": \"[^\"]*\"/\"version\": \"$version\"/" app/tauri.conf.json
grep -q "^version = \"$version\"" Cargo.toml && grep -q "^version = \"$version\"" app/Cargo.toml \
  && grep -q "\"version\": \"$version\"" app/tauri.conf.json || { echo "could not set the version" >&2; exit 1; }
echo "version $version"

# 2. Glint.app and the disk image.
app/bundle-macos.sh --dmg

# A shared target folder can get another checkout's binaries between build and copy: make sure
# the bundle carries this version before anything is signed.
built="$root/target/release/Glint.app/Contents"
for tool in "$built/Resources/bin/glint" "$built/MacOS/glintd"; do
  have=$("$tool" --version 2>/dev/null | awk '{print $2}')
  [ "$have" = "$version" ] || { echo "$tool is version '${have:-unknown}', not $version; build again" >&2; exit 1; }
done

# 3. The updater archive, signed. Tauri's updater on macOS replaces the whole Glint.app with it,
# so the new glintd and glint come along.
arch=$(uname -m)
case "$arch" in arm64) target=darwin-aarch64 ;; x86_64) target=darwin-x86_64 ;; *) echo "unknown arch $arch" >&2; exit 1 ;; esac
dist="$root/target/release/dist-$version"
rm -rf "$dist"
mkdir -p "$dist"
archive="Glint-$version-macos-$arch.app.tar.gz"
COPYFILE_DISABLE=1 tar -C "$root/target/release" -czf "$dist/$archive" Glint.app
mv "$root/target/release/Glint-$version-macos-$arch.dmg" "$dist/"
TAURI_SIGNING_PRIVATE_KEY_PASSWORD="${TAURI_SIGNING_PRIVATE_KEY_PASSWORD:-}" \
  $tauri signer sign -f "$key" "$dist/$archive" >/dev/null
[ -s "$dist/$archive.sig" ] || { echo "signing failed" >&2; exit 1; }

# 4. latest.json, the file every installed Glint asks for.
NOTES="$notes" VERSION="$version" TARGET="$target" URL="$base/$archive" SIG="$dist/$archive.sig" python3 - > "$dist/latest.json" <<'EOF'
import datetime, json, os
print(json.dumps({
    "version": os.environ["VERSION"],
    "notes": os.environ["NOTES"],
    "pub_date": datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
    "platforms": {os.environ["TARGET"]: {"signature": open(os.environ["SIG"]).read().strip(), "url": os.environ["URL"]}},
}, indent=2))
EOF
printf '%s\n' "$notes" > "$dist/notes.md"
echo "release files in $dist:"
ls -1 "$dist" | sed 's/^/  /'

# 5. Publish.
if [ -n "$publish" ]; then
  gh release create "v$version" --repo "$repo" --title "Glint $version" --notes-file "$dist/notes.md" \
    "$dist/$archive" "$dist/$archive.sig" "$dist/Glint-$version-macos-$arch.dmg" "$dist/latest.json"
  echo "published https://github.com/$repo/releases/tag/v$version"
  echo "now commit the version bump: git commit -am \"Release $version\" && git tag v$version"
else
  echo "not published; run again with --publish, or upload these files to a release v$version on $repo"
fi
