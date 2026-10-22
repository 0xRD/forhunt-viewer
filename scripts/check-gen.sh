#!/usr/bin/env bash
# Checks that csv-gen output is deterministic and platform-independent:
# SHA-256 of small files for every layout (and the edge-case suite) must match
# tools/csv-gen/checksums.txt, with 1 thread and with all threads.
# usage: scripts/check-gen.sh [--update]
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
OUT="${OUT:-$ROOT/target/check-gen}"
rm -rf "$OUT" && mkdir -p "$OUT"
cargo build --release --locked -q -p csv-gen
GEN="$ROOT/target/release/csv-gen"

sha() { if command -v sha256sum >/dev/null; then sha256sum "$1"; else shasum -a 256 "$1"; fi | cut -d' ' -f1; }

{
  for layout in forhunt mft hayabusa plaso generic; do
    for t in 1 0; do
      RAYON_NUM_THREADS=$t "$GEN" gen --layout "$layout" --rows 40000 --seed 7 -o "$OUT/$layout.$t.csv" 2>/dev/null
    done
    cmp "$OUT/$layout.1.csv" "$OUT/$layout.0.csv"
    echo "$(sha "$OUT/$layout.1.csv")  $layout"
  done
  "$GEN" gen --layout forhunt --rows 20000 --seed 7 --crlf --delim ';' -o "$OUT/crlf.csv" 2>/dev/null
  echo "$(sha "$OUT/crlf.csv")  forhunt-crlf-semicolon"
  "$GEN" edge --out "$OUT/edge" 2>/dev/null
  for f in $(cd "$OUT/edge" && ls | LC_ALL=C sort); do
    echo "$(sha "$OUT/edge/$f")  edge/$f"
  done
} > "$OUT/checksums.txt"

if [ "${1:-}" = "--update" ]; then
  cp "$OUT/checksums.txt" tools/csv-gen/checksums.txt
  echo "updated tools/csv-gen/checksums.txt"
else
  diff -u --strip-trailing-cr tools/csv-gen/checksums.txt "$OUT/checksums.txt"
  echo "csv-gen output matches checksums.txt"
fi
