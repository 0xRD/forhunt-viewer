#!/usr/bin/env bash
# Packages a Linux x86_64 release build: .tar.gz and (when appimagetool is
# given) an AppImage. usage: scripts/package-linux.sh VERSION [APPIMAGETOOL]
set -euo pipefail
VER="$1"; TOOL="${2:-}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"; cd "$ROOT"
BIN=target/release/forhunt-viewer
mkdir -p dist stage
NAME="forhunt-viewer-$VER-linux-x86_64"
rm -rf "stage/$NAME" && mkdir -p "stage/$NAME"
cp "$BIN" LICENSE-MIT LICENSE-APACHE README.md THIRD-PARTY.md packaging/linux/forhunt-viewer.desktop "stage/$NAME/"
cp crates/fhv-app/assets/fonts/OFL-*.txt "stage/$NAME/"
cp packaging/icons/icon-256.png "stage/$NAME/forhunt-viewer.png"
tar -C stage -czf "dist/$NAME.tar.gz" "$NAME"
if [ -n "$TOOL" ]; then
  APPDIR=stage/AppDir; rm -rf "$APPDIR"
  mkdir -p "$APPDIR/usr/bin" "$APPDIR/usr/share/icons/hicolor/256x256/apps" "$APPDIR/usr/share/applications"
  cp "$BIN" "$APPDIR/usr/bin/"
  cp packaging/linux/forhunt-viewer.desktop "$APPDIR/"
  cp packaging/linux/forhunt-viewer.desktop "$APPDIR/usr/share/applications/"
  mkdir -p "$APPDIR/usr/share/doc/forhunt-viewer"
  cp LICENSE-MIT LICENSE-APACHE THIRD-PARTY.md crates/fhv-app/assets/fonts/OFL-*.txt "$APPDIR/usr/share/doc/forhunt-viewer/"
  cp packaging/icons/icon-256.png "$APPDIR/forhunt-viewer.png"
  cp packaging/icons/icon-256.png "$APPDIR/usr/share/icons/hicolor/256x256/apps/forhunt-viewer.png"
  ln -sf usr/bin/forhunt-viewer "$APPDIR/AppRun"
  ARCH=x86_64 APPIMAGE_EXTRACT_AND_RUN=1 "$TOOL" --no-appstream "$APPDIR" "dist/forhunt-viewer-$VER-linux-x86_64.AppImage"
fi
(cd dist && for f in *; do case "$f" in *.sha256) ;; *) sha256sum "$f" > "$f.sha256";; esac; done)
ls -la dist
