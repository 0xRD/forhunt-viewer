# Performance

Every number here comes with the command, the input and the machine. No
number without a measurement. Newest sections last.

## Machines

| Id | CPU | Cores | RAM | Disk | OS |
|----|-----|-------|-----|------|----|
| M1 | Intel Xeon @ 2.10GHz (cloud VM) | 4 | 15 GiB | virtio block device (cloud, not local NVMe) | Linux 6.18 |

## Phase 1: virtual grid spike (2026-10-22)

Machine M1, Xvfb 1920x1080, Mesa llvmpipe (software OpenGL, no GPU), window
1600x1000 (~47 visible rows x 13 columns), vsync off, release build.
The benchmark scrolls 3 rows per frame for 40 frames, then jumps to a far
position (like a scrollbar drag) for 10 frames, over 1000 frames (first 10
dropped). `ui_ms` = time in `App::ui` (layout of the grid, cell formatting,
text shaping); `cpu_usage_ms` = eframe's frame CPU time including tessellation
and software rendering.

```
DISPLAY=:99 target/release/forhunt-viewer --spike-rows N --bench-frames 1000
```

| Rows | ui_ms p50 | ui_ms p99 | cpu_usage_ms p50 | cpu_usage_ms p99 |
|---:|---:|---:|---:|---:|
| 1 000 | 0.68 | 4.82 | 5.94 | 11.56 |
| 100 000 000 | 0.68 | 8.04 | 6.53 | 15.02 |
| 1 000 000 000 | 0.69 | 8.15 | 6.25 | 15.25 |

Frame cost is independent of the row count, as designed. p99 is the jump
frames (all ~600 cells need new text layouts). Even on a software renderer
every frame is below the 16.7 ms budget of 60 fps; on a GPU the rendering
part shrinks further.

## Phase 2: row index (2026-10-22)

Corpora (generated, not committed):

```
csv-gen gen --layout hayabusa --size 5G --seed 1 -o bench-data/hayabusa-5g.csv   # 5 371 041 931 B, 20 332 544 rows, 9 cols
csv-gen gen --layout forhunt  --size 5G --seed 1 -o bench-data/forhunt-5g.csv    # 5 372 004 722 B,  8 667 136 rows, 26 cols
```

Baseline on M1: `cat hayabusa-5g.csv > /dev/null` takes 2.41 s right after
`echo 3 > /proc/sys/vm/drop_caches` and 0.79 s warm. In this VM, "cold"
still benefits from the host's cache; this is not a local NVMe measurement.

Command: `fhv-bench index FILE --access pread|mmap`. `first_rows_ms` = from
`Table::open` to 100 decoded rows on screen while indexing continues;
`index_s` = open + full index. RSS is from `/proc/self/status`.

Byte-at-a-time state machine (first version), warm, second run:

| File | Access | first rows | index | throughput | peak RSS |
|---|---|---:|---:|---:|---:|
| hayabusa-5g | mmap | 45 ms | 4.04 s | 1.33 GB/s | 5 207 MiB (5 125 file-backed) |
| hayabusa-5g | pread | 55 ms | 4.19 s | 1.28 GB/s | 124 MiB |
| forhunt-5g | mmap | 49 ms | 3.83 s | 1.40 GB/s | 5 162 MiB |
| forhunt-5g | pread | 55 ms | 4.03 s | 1.34 GB/s | 80 MiB |

With SWAR skipping of non-special bytes (D-009), three warm runs and one cold
run each:

| File | Access | first rows | index warm (3 runs) | index cold | peak RSS | index size |
|---|---|---:|---|---:|---:|---:|
| hayabusa-5g | pread | 20-35 ms | 1.86 / 1.43 / 1.33 s | 2.57 s | 124 MiB | 77.7 MiB (1.5 % of file) |
| hayabusa-5g | mmap | 12-21 ms | 1.24 / 1.31 / 1.42 s | 3.33 s | 5 207 MiB | 77.7 MiB |
| forhunt-5g | pread | 26-75 ms | 5.52* / 1.90 / 1.68 s | 2.67 s | 80 MiB | 33.1 MiB (0.65 %) |
| forhunt-5g | mmap | 16-22 ms | 1.49 / 1.45 / 1.73 s | 3.54 s | 5 162 MiB | 33.1 MiB |

\* first run after switching files: part of the file had been evicted from
the page cache (the two corpora together are 10.7 GB on a 15 GiB machine).

Reading: cold pread indexing (2.6 s) is within 7 % of `cat` (2.4 s), so it is
I/O-bound. The index is ~4 B/row. pread is the default (D-008).

### Reopen with the index cache

`fhv-bench index FILE` three times with an empty cache dir (first run builds,
then two cache hits):

| File | build | reopen (index loaded, 100 rows decoded) | cache file |
|---|---:|---:|---:|
| hayabusa-5g (20.3 M rows) | 10.64 s* | 0.488 s / 0.448 s | 78 MiB |
| forhunt-5g (8.7 M rows) | 2.26 s | 0.058 s / 0.136 s | 34 MiB |

\* page cache evicted by the previous run on the other 5 GB file; the cloud
disk then delivered ~0.5 GB/s. The reopen time is reading + BLAKE3-checking
+ decoding the cache file; target < 1 s met.

## Phase 3: column indexes, filters, sorts (2026-10-22)

### Column pass

`fhv-bench index FILE` with an empty cache (rows ready = row index done;
columns = second pass building dictionaries, primary time ticks/permutation
and ragged-row detection):

| File | rows ready | column pass, first version | column pass, final | column index size |
|---|---:|---:|---:|---:|
| hayabusa-5g (20.3 M rows, 9 cols) | 1.57 s | 19.1 s | 3.78 s | 284 MiB (7 dictionaries + ticks) |
| forhunt-5g (8.7 M rows, 26 cols) | 2.15 s | 27.6 s | 3.96 s | 169 MiB (15 dictionaries + ticks) |

Profile of the first version (callgrind, 60 k rows, 1 thread):
- `split_row`, byte-at-a-time: 31 %.
- Roaring `push`, whose `max()` scans the bitmap store backwards on every insert: 20 %.

Fixes:
- `split_row` now uses the same SWAR special-byte skipping as the row index.
- Bitmaps are built per block with `from_sorted_iter` in the parallel phase and OR-ed in the sequential merge.

Total index (rows + columns) is 362 MiB for hayabusa-5g, i.e. 6.7 % of the
file, and 202 MiB (3.8 %) for forhunt-5g. Reopen from cache:
0.66 s / 0.30 s.

### Filters and sorts

`fhv-bench query FILE --ops bench/{hayabusa,forhunt}.ops` (ops files are in
the repo; index loaded from cache). Times are per operation, 4 threads, warm
page cache.

hayabusa-5g, 20 332 544 rows:

| Operation | Time | Matches | Target |
|---|---:|---:|---|
| `Level in [high, crit]` (dictionary) | 12 ms | 967 984 | < 100 ms ✓ |
| `Level not in [info]` | 17 ms | 3 387 916 | < 100 ms ✓ |
| `EventID in [4624, 4625]` | 4 ms | 5 079 313 | < 100 ms ✓ |
| `Details contains "198.51.100.77 "` (scan, regex prefilter) | 547 ms | 20 341 | < 2 s ✓ |
| same, again (cached) | 0.1 ms | | cached ✓ |
| `Details contains "TgtUser: o'brien"` | 1 021 ms | 1 269 813 | < 2 s ✓ |
| any column contains `mimikatz` | 618 ms | 242 380 | reported |
| any column contains `LID: 0x1234` | 819 ms | 308 | reported |
| `Timestamp between` (time index) | 124 ms | 800 000 | |
| `RecordID > 4900000` (number scan) | 987 ms | 407 225 | |
| `Level in [...] AND Computer begins with DC-` | 22 ms | 88 244 | |
| sort Timestamp / desc (time index) | 170 / 317 ms | | "instant" ✓ |
| sort Computer (dictionary) | 201 ms | | ✓ |
| sort RecordID (number) | 2 247 ms | | < 3 s ✓ |
| sort Computer, RecordID desc | 3 411 ms | | ~ |
| sort Details (long free text, 20 M rows) | 11 435 ms | | < 3 s ✗ (D-013) |
| sort Details, 968 k filtered rows | 808 ms | | ✓ |

Peak RSS 6.3 GiB; anonymous memory 653 MiB. The peak is the transient
read-only map used while resolving text ties, which counts as file-backed
RSS (D-013). Without text sorts, peak RSS stays at ~1.3 GiB anonymous
(first `query` run, before the window refinement existed).

forhunt-5g, 8 667 136 rows:

| Operation | Time | Matches |
|---|---:|---:|
| `EventId in [4624, 4625]` | 1.4 ms | 1 359 628 |
| `EventId not in [...]` | 5.5 ms | 7 307 508 |
| `Channel equals security` | 0.6 ms | 4 194 304 |
| `Payload contains whoami` (last column, quoted JSON) | 2 001 ms | 180 545 |
| same, again | 0.1 ms | |
| `Payload contains DisableRealtimeMonitoring` | 501 ms | 3 283 |
| any column contains `198.51.100.7` | 423 ms | 40 392 |
| `Payload regex -enc\s+[A-Za-z0-9+/=]{40,}` (no prefilter) | 5 232 ms | 181 144 |
| `TimeCreated between` | 81 ms | 576 722 |
| sort TimeCreated / desc | 23 / 81 ms | |
| sort EventId | 76 ms | |
| sort RecordNumber | 1 057 ms | |
| sort PayloadData2 (free text) | 7 214 ms | |
| sort Computer, TimeCreated desc | 646 ms | |

Sort iterations on hayabusa `Details`:
- 8-byte prefix with per-row positional reads for ties: 32.6 s.
- 24-byte inline key, window refinement on a transient map: 14.5 s.
- Blocks return flat key buffers, and MSD pair sort: 12.1 s.
- Tried and reverted:
  - a plain comparison sort of borrowed cells: 26.8 s;
  - cell reads in file order: no change.
