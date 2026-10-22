#!/usr/bin/env bash
# Performance regression gate: counts the instructions fhv-bench executes
# (valgrind/callgrind, 1 thread) on a fixed generated corpus, for the
# current checkout and a base revision built in the same job. Fails when
# head is more than THRESHOLD_PCT percent above base. Instruction counts are
# stable on shared runners, unlike wall-clock time.
#
# usage: scripts/bench-gate.sh [BASE_REV]
set -euo pipefail
THRESHOLD_PCT="${THRESHOLD_PCT:-3}"
BASE="${1:-}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
WORK="${WORK:-$ROOT/target/bench-gate}"
mkdir -p "$WORK"
cd "$ROOT"
command -v valgrind >/dev/null || { echo "valgrind is required" >&2; exit 2; }

cargo build --release --locked -q -p csv-gen -p fhv-bench
CORPUS="$WORK/corpus.csv"
ROWS=40000
# Generated, then checked by content (a cache could restore an empty or
# truncated file, and the gate would silently measure nothing).
if [ ! -s "$CORPUS" ]; then
  target/release/csv-gen gen --layout forhunt --rows $ROWS --seed 7 -o "$CORPUS" 2>/dev/null
fi
LINES=$(wc -l < "$CORPUS")
BYTES=$(wc -c < "$CORPUS")
if [ "$BYTES" -lt $((20 << 20)) ] || [ "$LINES" -lt "$ROWS" ]; then
  echo "benchmark corpus is incomplete ($BYTES bytes, $LINES lines)" >&2
  exit 2
fi

count() { # bench-binary label
  local total=0 n
  for args in "index $CORPUS --no-cache" "query $CORPUS --ops bench/gate.ops --cache $WORK/cache-$2"; do
    rm -rf "$WORK/cache-$2"
    # shellcheck disable=SC2086
    RAYON_NUM_THREADS=1 valgrind --tool=callgrind --callgrind-out-file="$WORK/$2.cg" "$1" $args >"$WORK/$2.out" 2>"$WORK/$2.log"
    n=$(grep -E '^summary:' "$WORK/$2.cg" | awk '{print $2}')
    total=$((total + n))
  done
  # The query run must have matched rows (else the corpus/ops are broken).
  grep -q "matches=[1-9]" "$WORK/$2.out" || { echo "no matches in $2 run" >&2; cat "$WORK/$2.out" >&2; exit 2; }
  echo "$total"
}

cp target/release/fhv-bench "$WORK/fhv-bench-head"
HEAD_N=$(count "$WORK/fhv-bench-head" head)
echo "head: $HEAD_N instructions"
if [ -z "$BASE" ] || ! git cat-file -e "$BASE^{commit}" 2>/dev/null; then
  echo "no base revision to compare with; recording head only"
  exit 0
fi
rm -rf "$WORK/base-src"
git worktree prune
if ! git worktree add -f --detach "$WORK/base-src" "$BASE" >"$WORK/worktree.log" 2>&1; then
  echo "cannot check out base $BASE:" >&2; cat "$WORK/worktree.log" >&2; exit 2
fi
trap 'git worktree remove --force "$WORK/base-src" >/dev/null 2>&1 || true' EXIT
if ! (cd "$WORK/base-src" && cargo build --release -q -p fhv-bench --target-dir "$WORK/base-target" 2>/dev/null) \
   || ! grep -q "no_cache" "$WORK/base-src/tools/fhv-bench/src/main.rs"; then
  echo "base revision cannot run the gate; skipping comparison"
  exit 0
fi
BASE_N=$(count "$WORK/base-target/release/fhv-bench" base)
echo "base: $BASE_N instructions ($BASE)"
python3 - "$BASE_N" "$HEAD_N" "$THRESHOLD_PCT" <<'PY'
import sys
base, head, thr = int(sys.argv[1]), int(sys.argv[2]), float(sys.argv[3])
delta = (head - base) * 100.0 / base
print(f"change: {delta:+.2f}% (threshold +{thr}%)")
if delta > thr:
    print("performance regression detected", file=sys.stderr)
    sys.exit(1)
PY
