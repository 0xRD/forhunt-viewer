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
