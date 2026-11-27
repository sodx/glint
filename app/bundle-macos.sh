#!/bin/sh
# Builds Glint.app: the desktop shell plus glintd and glint inside it, ad-hoc signed.
#
#   app/bundle-macos.sh                 # → target/release/Glint.app
#   app/bundle-macos.sh --install       # … and copy it to ~/Applications
#
# No tauri-cli needed: the bundle is a plain folder with an Info.plist.
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

cargo build --release -p glintd -p glint-cli
(cd app && cargo build --release)

out="$root/target/release/Glint.app"
rm -rf "$out"
mkdir -p "$out/Contents/MacOS" "$out/Contents/Resources"
cp app/target/release/glint-app "$out/Contents/MacOS/Glint"
cp target/release/glintd "$out/Contents/MacOS/"
# Not next to "Glint": macOS file systems are case-insensitive, so `glint` would replace it.
mkdir -p "$out/Contents/Resources/bin"
cp target/release/glint "$out/Contents/Resources/bin/glint"
cp app/icons/icon.icns "$out/Contents/Resources/Glint.icns"
version=$(sed -n 's/^version = "\(.*\)"/\1/p' app/Cargo.toml | head -1)
cat > "$out/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleName</key><string>Glint</string>
  <key>CFBundleDisplayName</key><string>Glint</string>
  <key>CFBundleIdentifier</key><string>local.glint.app</string>
  <key>CFBundleExecutable</key><string>Glint</string>
  <key>CFBundleIconFile</key><string>Glint</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$version</string>
  <key>CFBundleVersion</key><string>$version</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSAppTransportSecurity</key><dict><key>NSAllowsLocalNetworking</key><true/></dict>
</dict></plist>
EOF
codesign --force --deep --sign - "$out" >/dev/null 2>&1 || echo "warning: ad-hoc codesign failed" >&2
echo "built $out"

if [ "${1:-}" = "--install" ]; then
  mkdir -p "$HOME/Applications"
  rm -rf "$HOME/Applications/Glint.app"
  cp -R "$out" "$HOME/Applications/Glint.app"
  echo "installed $HOME/Applications/Glint.app"
fi
