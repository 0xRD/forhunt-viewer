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
