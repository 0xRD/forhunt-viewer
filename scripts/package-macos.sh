#!/usr/bin/env bash
# Packages the Apple Silicon build as forhunt-viewer.app inside a .dmg.
# Ad-hoc signed only (no Developer ID: B-001). usage: scripts/package-macos.sh VERSION
set -euo pipefail
VER="$1"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"; cd "$ROOT"
BIN=target/aarch64-apple-darwin/release/forhunt-viewer
mkdir -p dist stage
APP=stage/dmg/forhunt-viewer.app
rm -rf stage/dmg && mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/"
# Bundle versions must be numeric (x.y.z): drop a pre-release suffix.
BUNDLE_VER="${VER%%-*}"
sed "s/@VERSION@/$BUNDLE_VER/g" packaging/macos/Info.plist > "$APP/Contents/Info.plist"
cp packaging/icons/icon.icns "$APP/Contents/Resources/"
cp LICENSE-MIT LICENSE-APACHE THIRD-PARTY.md crates/fhv-app/assets/fonts/OFL-*.txt "$APP/Contents/Resources/"
plutil -lint "$APP/Contents/Info.plist"
codesign --force --deep --sign - "$APP"
ln -s /Applications stage/dmg/Applications
DMG="dist/forhunt-viewer-$VER-macos-arm64.dmg"
hdiutil create -volname forhunt-viewer -srcfolder stage/dmg -ov -format UDZO "$DMG"
(cd dist && shasum -a 256 "$(basename "$DMG")" > "$(basename "$DMG").sha256")
ls -la dist
