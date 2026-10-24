# Performance review (2026-10-22)

Scope: column pass, scan filters, text sort, memory. Nothing in the repository
sources was changed. All prototypes ran in a scratch copy (`/tmp/claude-0/perf/repo`);
the diff is in `the prototype diff (git history, removed in 0.3.0)`. Only numbers that were run are
listed. Machine M1 (4 cores Xeon 2.1 GHz, 15 GiB, virtio disk).

**Measurement noise.** M1 is a shared VM. The same binary varied by 25-40 % between
quiet and busy periods (column pass 0.55 s vs 0.76 s on the same 1 GB file; hayabusa
`sort Details` 1.9-3.0 s). Every A/B below is interleaved, min or median of 3-5
runs, and gains under ~10 % are called "not measurable".

## 1. Method validation (unmodified engine)

Inputs: `f1g.csv` = `csv-gen gen --layout forhunt --size 1G --seed 1` (1 081 651 371 B,
1 654 784 rows, 26 cols); `h1g.csv` = `--layout hayabusa --size 1G --seed 1`
(1 077 627 434 B, 4 079 616 rows, 9 cols); `bench-data/forhunt-5g.csv` (8 667 136 rows).
Warm page cache unless noted. Commands: `fhv-bench index FILE --no-cache`,
`fhv-bench query FILE --ops bench/forhunt.ops --cache DIR`.

| Measurement | Result here | PERF.md |
|---|---|---|
| f1g rows ready / column pass (4 runs) | 0.41-0.55 s / 0.51-0.59 s (a busy period gave 0.70-0.78 / 0.95-1.15) | 5g: 2.15 / 3.96 |
| h1g rows ready / column pass | 0.35-0.54 s / 0.45-1.03 s, col index 57 MiB | n/a |
| forhunt-5g rows ready / column pass | 1.69 s / 2.73 s (2nd run; 1st run 5.1 s, cache evicted); col index 168.7 MiB | 2.15 / 3.96 s, 169 MiB |
| first rows on screen (5g) | 19-47 ms | 26-75 ms |
| reopen with cache: f1g / 5g (warm) | 84 ms / 0.32-0.36 s | 0.30 s |
| reopen 5g after `drop_caches` | 0.444 s | |
| 5g `EventId in` / `not in` / `Channel equals` | 0.9 / 3.9 / 0.4 ms | 1.4 / 5.5 / 0.6 ms |
| 5g `Payload contains whoami`, warm | 0.72-0.92 s | 2.0 s |
| same after `echo 3 > drop_caches` | 2.32 s | |
| 5g `Payload regex -enc...` (no prefilter) | 4.38-4.96 s | 5.2 s |
| 5g sort TimeCreated / EventId / RecordNumber | 22-31 / 73-79 / 0.92-1.12 s | 23 / 76 / 1.06 |
| 5g sort PayloadData2 (free text, 8.7 M rows) | 5.4-6.1 s | 7.2 s |

Reading: the method reproduces. The documented "Payload contains at ~2 s" is
disk-bound: warm it is 0.7-0.9 s, cold 2.3 s, which equals reading 5.3 GB from this
disk (`cat` cold: 2.4 s in PERF.md). The engine is at the I/O floor for prefiltered
contains scans. The two real misses are the unprefiltered regex and text sorts.

## 2. Hotspots (callgrind, 60 k-row forhunt file, `RAYON_NUM_THREADS=1`, `profiling` build)

Instruction counts (Ir). Per-op numbers are minus the 58.6 M fixed cost of loading
from cache. Files: `/tmp/claude-0/perf/cg/*.cg` (scratch, not kept).

**Index + column pass** (`index --no-cache`, 1.648 G Ir total):
- row index `scan_chunk`: 339 M (20.6 %); `Source::read` zero-fill `memset`: 110 M (6.7 %).
- `columns::build`: 634 M (38.5 %). Its full pass is 338 M (20.5 % of total), of which
  `scan::split_row` is **287 M = 85 %** (4 780 Ir/row), `time::parse` 31.6 M (9 %), everything
  else (hashing, local dict vectors) ~5 %. The 20 k-row profile sample is another 99 M and
  is a fixed cost that vanishes on big files.
- Wall-clock split of the column pass on f1g (instrumented `par_block_list`): map 0.50 s,
  **serial `consume` 0.12-0.14 s (~20 %)** during which the pool is idle.

**Scan filters** (Ir above baseline; 46.7 MB of rows):

| Op | Ir | Where |
|---|---|---|
| `Payload contains whoami` (prefilter) | 163 M (3.5 Ir/B) | memset in `Source::read` 48.5 M (30 %), teddy prefilter ~52 M (32 %), `nth_field` 36 M |
| `any contains 198.51.100.7` | 66 M | memset 48.5 M |
| `RecordNumber > 4900` (no prefilter, col 0) | 104 M | memset 48.5 M, `scan_chunk` is load cost |
| **`Payload regex -enc\s+...` (no prefilter)** | **1 523 M (32.6 Ir/B)** | **`nth_field` slow path 1 003 M self (63 %)** + ~190 M inlined slice/iter lines of the same loop + `unquote` ~145 M. The regex engine itself is <1 % (it already uses an inner "-enc" literal on the cell). |

Root cause: `scan.rs:nth_field` takes the memchr fast path only if the row has **no quote
byte at all**. In forhunt layout every row has a quoted JSON `Payload`, so every
`nth_field` runs the byte-at-a-time state machine from byte 0 to the target column
(~33 Ir/byte). The same applies to sort key extraction on any column of such files.

**Text sort** (`sort.rs`):
- `sort PayloadData2` (col 15): 1 200 M Ir; `nth_field` (+ copy inlined into the key
  closure) is ~70 %.
- `sort Payload` (col 24, quoted JSON): 3 663 M Ir; `nth_field` 1 003 M self + 1 166 M in the
  key-extraction closure, `unquote` ~190 M, `Resolve::run` compare closure 145 M.
- Hayabusa `sort Details` (no quotes): flat profile, no instruction hotspot (top entries
  memset 6 %, sort_by 4.7 %, memchr 4 %). It is memory-latency bound. Thread-seconds inside
  `Resolve::run` on h1g (4.08 M rows, wall: keys 0.43 s, sort 0.29 s, resolve 1.0 s):
  cells extraction 2.28 (58 %), window levels 1.14 (29 %), final group compares 0.52 (13 %);
  7 116 level passes re-key 8.67 M rows.

**New finding: allocator lock contention in `Resolve::run`.** `sort Payload` on f1g in a
fresh process took 7.0-7.5 s (user 13.8 s, **sys 6.7 s**); the identical sort after a regex
scan in the same process took 2.3 s (sys 1.2 s). `strace -f -c`: 344 687 `futex` calls (120 k
EAGAIN). `gdb` stack samples (8): every worker blocked in
`futex_wait <- lll_lock_wait_private <- realloc <- Resolve::cell / unquote`.
`unquote` pushes byte by byte into a `Vec::new()`, so each 700 B cell does ~7 reallocs
and rayon workers fight over arena locks. `MALLOC_MMAP_THRESHOLD_=33554432` alone gave
7.1 -> 3.9 s, confirming it is an allocator-state problem. Results therefore depend on what ran
before in the process. This is the likely reason sort timings in PERF.md vary.

## 3. Findings, ranked by impact

Gains are measured on the scratch prototype unless marked "unmeasured".

### F1. Blocks are sized by rows (32 768), not bytes: no parallelism and unbounded memory on wide rows
- Evidence: `wide.csv` (4 000 rows x 100 KB, 400 MB). Original: `index` peak RSS **818 MiB
  (2x file)**, `contains` 2 186 ms, regex 1 565 ms, `sort blob` 1 530 ms; the whole file is
  one block, so scans run on **one thread** and `profile()` copies every cell (`to_vec`).
  Same cause on normal rows: 16 blocks in flight x 20 MiB = 258 MiB anon (f1g) and 398 MiB (5g)
  on 4 cores, 640 MiB on 8 cores; the 20 MiB blocks also exceed the 8 MiB L2.
- Root cause: `table.rs:par_blocks` / `par_block_list`, callers `columns.rs` (BLOCK_ROWS),
  `filter.rs` (SCAN_BLOCK), `sort.rs` (BLOCK).
- Change: split each requested block into sub-blocks of at most N bytes (prototype: 4 MiB,
  at least one row) inside `par_block_list`; consumers need no change except
  `columns.rs`, which added a constant `BLOCK_ROWS` to the progress counter (now uses the
  block's row count).
- Measured (cap 4 MiB): wide.csv `index` rows ready 1.69 -> 0.22 s, peak RSS 818 -> 269 MiB;
  `contains` 2 186 -> 202 ms; regex 1 565 -> 144 ms; `sort blob` 1 530 -> 47 ms. f1g column
  pass peak RSS 258 -> 92 MiB. Cost: smaller blocks make the serial merge heavier (column
  pass +10 % with 4 MiB; 16 MiB cap showed no loss) which F4 removes.
- Risk: low. Consumers already get blocks in order and handle arbitrary row ranges; a record
  over 4 MiB still forms its own block. All fhv-engine tests pass.

### F2. `nth_field` / `split_row` / `unquote` are slow for any row that contains a quote
- Evidence: section 2 (63 % of the unprefiltered regex scan, ~70 % of text-key extraction).
- Root cause: `scan.rs:nth_field` (slow path), `scan.rs:split_row` (visits every delimiter
  inside quoted JSON), `scan.rs:unquote` (byte-by-byte `push`, reallocating).
- Change (prototype): one `field_end` helper. Unquoted field: `memchr(delim)`. Field starting
  with a quote: jump with `memchr(quote)` over `""` pairs (delimiters inside quotes are
  skipped), then `memchr(delim)` after the closing quote. `split_row`, `nth_field`,
  `count_fields` use it; `unquote` copies between quotes in bulk with `reserve(raw.len())`.
  Equivalence is fuzz-tested against `split_row_simple` (new test: alphabet `ab,;"\r\n`,
  lengths 0-79, 400 rows per length, both dialects, also `count_fields`/`nth_field`).
- Measured on f1g (1 GB), baseline -> F1+F2+F3 build, min of 5: unprefiltered numeric scan
  `RecordNumber > 4900` 127 -> 91 ms; `sort PayloadData2` 959 -> 552 ms; `sort Payload`
  (warm allocator) 2 143 -> 1 089 ms; **fresh-process `sort Payload` 7.0-7.5 s -> 1.9 s** and
  `sort ExecutableInfo,Payload` 6.3-7.5 -> 1.7 s (the lock contention disappears with the
  one-allocation `unquote`). 5g: `sort PayloadData2` 5.4-5.8 s -> 2.9-3.5 s. Instructions:
  `split_row` in the column pass 287 M -> 204 M (-29 %), full index run 1.648 G -> 1.483 G
  (-10 %). Wall-clock effect on the column pass alone was not measurable (<5 %).
- Risk: medium-low (parsing semantics, D-010). Fuzz + existing proptests pass; clippy
  `-D warnings` clean. Hayabusa `sort Details` is unchanged (no quotes).
- Remaining: `split_row` still costs ~3 400 Ir/row, dominated by the `""` pairs in the JSON
  payload (one `memchr` call per pair). Next step (unmeasured): scan quoted fields with a
  pair-aware SWAR/AVX2 loop (mask of quotes, accept quote pairs with shifted masks), or have
  the column pass skip splitting after the last needed column when it can count fields
  another way.

### F3. User regex has no block prefilter (known miss, 5.2 s on 5g)
- Evidence: 5g `Payload regex -enc\s+[A-Za-z0-9+/=]{40,}` 4.38-4.96 s (every row goes through
  `nth_field` + regex).
- Root cause: `filter.rs:Compiled::prefilter` returns `None` for `Op::Regex`.
- Change: `regex_prefilter(pattern, ci)`: parse with `regex_syntax` (same case flag), extract
  prefix literals with `Extractor::kind(Prefix).limit_total(64)`, keep them if there are at
  most 64, each is >= 2 bytes and has no `"`, CR or LF, and use an alternation regex
  over raw block bytes exactly like the existing literal prefilter. New dependency:
  `regex-syntax = "0.8"` (already in the lock file as a dependency of `regex`).
- Measured: 5g regex **4.38-4.46 s -> 0.54-0.60 s** (181 144 matches both). f1g: regex
  1 025 -> 119 ms; `PayloadData2 regex zzz\d+` 465 -> 88 ms. Patterns with a common literal
  (`(mimikatz|procdump|rundll32)\.exe`, `\\Temp\\[a-z]+\.exe`: 1 289 / 1 268 -> 636 / 549 ms with F2, no
  extra gain from the prefilter because 6 % of rows contain the literal); `[a-z]+@[a-z]+` has no
  literal and is unchanged by F3 (1 233 -> 516 ms with F2 only).
- Risk: medium. It is a superset filter, so a wrong literal set means missed matches.
  Same caveat as the existing contains prefilter (text after a closing quote is
  decoded without its quote, so a match spanning `"x"ab` is not in the raw bytes). Needs a
  proptest with a richer regex generator (alternation, classes, `(?i)`, anchors, `\b`,
  non-ASCII) before shipping. The current proptest only has a few patterns and passes.

### F4. Serial merge idles the pool; windows are barriers
- Evidence: column pass on f1g: map 0.50 s wall, `consume` 0.12-0.14 s serial (~20 %).
- Root cause: `table.rs:par_block_list` maps a window of 4 x threads blocks, then consumes it
  on the calling thread before starting the next window.
- Change: producer thread runs the windows, bounded `sync_channel`, the caller consumes in
  order concurrently (`std::thread::scope`, so `consume` needs no `Send`).
- Measured (with F1): column pass 0.585-0.62 s -> 0.49-0.54 s (median 0.53) versus the
  original 0.505-0.557 (median 0.55) with 3x less memory. So the net effect of F1+F4 is
  neutral time, -65 % memory.
- Risk: medium. First attempt without `move` deadlocked (sender kept alive); the final one
  passes all tests. Cancellation is checked in the producer; a consumer error drops the
  receiver and ends the producer. Calling from inside a rayon pool thread with a 1-thread
  pool needs a test.

### F5. `Source::read` zero-fills the buffer on every block (low)
- Evidence: callgrind `memset` is 6.7 % of the index run, 30-39 % of the instructions in
  cheap scans.
- Change: `buf.truncate(n); if buf.len() < n { buf.resize(n, 0) }` (only growth is zeroed).
- Measured wall: **no measurable change** at 1 GB (4-5 interleaved runs of each scan; the
  scans run at 6-10 GB/s and are bound by the kernel copy). Cheap and safe, but do not
  expect more than a few percent. Risk: none.

### F6. Full sort of long free text (D-013): not fixed, now characterised
- 5g `PayloadData2` 8.7 M rows: 5.8 s -> 2.9-3.5 s with F2 (quoted column). Hayabusa `Details`
  (20 M rows target) is unaffected: 1 GB / 4 M rows takes 1.9-3.0 s, scaling to the
  documented ~11 s. It is memory-latency bound: 58 % of `Resolve::run` is fetching cells
  from the map in key order, 29 % re-keying (7 116 passes for 4 M rows).
- Tried and not useful (measured): extracting cells in file order (`cells` thread-seconds
  2.28 -> 2.19, within noise); `MmapOptions::populate()` (2.6 s / 1.8 s in two runs of the same
  binary, no signal); `madvise(DONTNEED)` between groups of runs for RSS (peak 1 195 MiB
  vs 1 190 MiB, because one large run touches the whole file).
- Unmeasured proposals: (a) store a second 24-byte window for rows still tied after the first
  `msd_sort` instead of re-reading the file, using a compact side buffer only for tied rows
  (their count was 4.08 M of 4.08 M here, so this is a full 2 x key array: ~100 MB per 4 M rows,
  roughly 0.5 GB at 20 M rows); (b) write the sort column to a temp file/spill buffer in row
  order (as D-013 suggests) so tie resolution reads 5-10x denser pages; (c) choose the window
  start after the sampled common prefix of the column (Details shares "Type: N | TgtUser: ").
  Expected gain not claimed.

### F7. Memory (measured, see section 4)
- Peak is dominated by (1) in-flight blocks (F1) and (2) file-backed pages touched by the
  sort map (`table.rs:map_data`), which D-013 already accepts. Roaring dictionary bitmaps are
  a good trade (169 MiB total column index on 5g, of which ticks 8 B x 8.7 M = 69 MB and the
  time permutation 35 MB); per-row codes would be larger (15 columns x 1 B x 8.7 M = 130 MB for
  the dictionaries alone), so no change proposed there.

### Not measured
- mmap vs pread for random row reads in sorted views: no tool in `fhv-bench` reads rows at
  random; PERF.md phase 4 covers scrolling with pread (p99 frame < 16.7 ms). Not re-measured.
- Windows/macOS behaviour, true NVMe, 8 cores, mimalloc (not available offline).

## 4. Memory

Command `fhv-bench ... ` prints VmHWM / RssAnon / RssFile from `/proc/self/status`.

| Case | Peak RSS | Anon at end | File-backed |
|---|---:|---:|---:|
| f1g index + column pass, original | 255-268 MiB | 252 | 4 |
| same, prototype (4 MiB blocks) | 90-99 MiB | | 4 |
| h1g index + columns | 147 MiB | 143 | 4 |
| forhunt-5g index + columns | 398-401 MiB | 395 | 3 |
| f1g reopen from cache | 83 MiB | 40 | 3 |
| wide.csv (400 MB, 100 KB rows) index, original / prototype | 818 / 269 MiB | | |
| f1g full forhunt.ops (filters + sorts) | 1 348-1 415 MiB | 243-330 | 3-4 (peak is the sort map) |
| h1g sort Details/Computer,RecordID | 1 316 MiB | 153 | |
| forhunt-5g full ops | 5 977-6 013 MiB | 446-458 | |

Per phase (sort PayloadData2 then Payload on f1g, `/proc` read at each stage): keys extracted
anon 123 MiB, sorted 149 MiB, **resolve: VmHWM jumps to 1 195 MiB** (file pages of the
transient map), final anon 186-236 MiB. So anonymous memory stays at 0.2-0.45 GiB even
on 5 GB; the headline RSS is reclaimable page cache. Index build and scans: transient
memory is `threads x 4 x block bytes`, i.e. proportional to row width x cores (F1).

## 5. Acceptance targets on M1 (4 cores, not NVMe), original engine

| Target | Result |
|---|---|
| first screen < 1 s | 19-47 ms |
| index I/O bound | rows ready 1.7 s warm / 2.6 s cold (PERF.md) vs `cat` 2.4 s cold |
| reopen with cache < 1 s | 0.32-0.44 s (5g) |
| in / not in on dictionary < 100 ms | 1-4 ms |
| contains on one text column < 2 s | 0.7-0.9 s warm, 2.3 s cold (disk-bound) |
| time sort instant | 22-31 ms (5g) |
| arbitrary column sort < 3 s | numbers 0.9-1.1 s ok; free text 5.4-6.1 s (quoted), ~11 s (hayabusa 20 M) miss |
| peak memory a fraction of file | anon 0.4-0.45 GiB of 5.3 GB; headline RSS 6 GiB (file-backed); wide-row files 2x file size (F1) |
| regex without literal | 4.4-5 s -> 0.54-0.60 s with F3 |

## 6. Suggested order

1. F1 (+F4 so it is time-neutral): fixes the only real memory/parallelism failure.
2. F2 incl. one-allocation `unquote`: removes the allocator contention that makes sort times
   depend on process history; large gains on every quoted file.
3. F3 with a bigger regex proptest.
4. F5 (free), then continue F2's pair-aware quote scanning, then F6 experiments.

Prototype diff (scan.rs, source.rs, filter.rs, columns.rs, table.rs + `regex-syntax`
dependency): `the prototype diff (git history, removed in 0.3.0)`. All fhv-engine tests (lib 16 incl. the new
fuzz test, integration incl. the filter-vs-naive proptest) and `cargo clippy -p fhv-engine
-D warnings` pass on it.
