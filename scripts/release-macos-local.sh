#!/usr/bin/env bash
# Builds the Apple Silicon .dmg on this Mac and adds it to an existing GitHub
# release (D-031): the tag's Release run publishes Linux and Windows only.
# usage: scripts/release-macos-local.sh vX.Y.Z
set -euo pipefail
TAG="${1:?usage: release-macos-local.sh vX.Y.Z}"
VER="${TAG#v}"
REPO=0xRD/forhunt-viewer
ROOT="$(cd "$(dirname "$0")/.." && pwd)"; cd "$ROOT"
[ "$(uname -sm)" = "Darwin arm64" ] || { echo "run on an Apple Silicon Mac" >&2; exit 1; }
[ -z "$(git status --porcelain --untracked-files=no)" ] || { echo "working tree has changes" >&2; exit 1; }
[ "$(git rev-parse HEAD)" = "$(git rev-parse "$TAG^{commit}")" ] || { echo "check out $TAG first" >&2; exit 1; }
gh release view "$TAG" -R "$REPO" >/dev/null

rm -rf dist/forhunt-viewer-*-macos-arm64.dmg*
MACOSX_DEPLOYMENT_TARGET=11.0 cargo build --release --locked -p fhv-app --target aarch64-apple-darwin
target/aarch64-apple-darwin/release/forhunt-viewer --version
scripts/package-macos.sh "$VER"
DMG="forhunt-viewer-$VER-macos-arm64.dmg"
codesign --verify --deep --strict "stage/dmg/forhunt-viewer.app"

# SHA256SUMS: the release's list plus the .dmg.
work="$(mktemp -d)"; trap 'rm -rf "$work"' EXIT
gh release download "$TAG" -R "$REPO" -p SHA256SUMS -D "$work"
{ grep -v -- '-macos-arm64\.dmg$' "$work/SHA256SUMS" || true; cat "dist/$DMG.sha256"; } | sort -k2 > "$work/SUMS"
mv "$work/SUMS" "$work/SHA256SUMS"
gh release upload "$TAG" -R "$REPO" --clobber "dist/$DMG" "dist/$DMG.sha256" "$work/SHA256SUMS"
gh release view "$TAG" -R "$REPO" --json assets -q '.assets[].name'
