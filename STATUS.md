# Status

Last update: 2026-10-22 (session 1)

## Done
- Owner decisions: unsigned macOS/Windows builds, Homebrew later (D-001, D-002)
- Phase 0: PLAN/DECISIONS/BLOCKERS/STATUS/PERF, workspace, CI, cargo deny,
  csv-gen (5 layouts + 33-file edge suite, checksummed)
- Phase 1: virtual grid spike, 100 M-1 B rows under the 60 fps budget (D-005)
- Phase 2: dialect/encoding detection, parallel quote-aware row index
  (I/O-bound: 5 GiB in 2.6 s cold), pread access (D-008), index cache
  (reopen 0.3-0.7 s), golden + differential tests
- Phase 3: column indexes (dictionary bitmaps, time ticks, numbers, ragged
  rows), filter engine (16 operators, AND/OR/NOT, any-column), multi-column
  sort; proptests vs naive evaluation; acceptance benchmarks in docs/PERF.md
  (one target missed: full sort of long free-text columns, D-013)
- CI green on Linux/macOS/Windows at 9201626
- CI is manual-dispatch only from now on (Actions minutes, D-017); macOS
  release arm64 only (D-018)

- Phase 4: app MVP (all brief items; see README), egui_kittest smoke tests,
  real-size check under Xvfb (60 fps scrolling on 20 M rows, PERF.md)
- Phase 5: icons, macOS .app/.dmg + Open With delegate, Windows MSI/zip,
  Linux tarball/AppImage, release workflow (tag / dispatch only), README,
  THIRD-PARTY.md, callgrind perf gate (+0.02 % noise)

- Phase 6: fresh-eyes review (REVIEW.md, resolution table) and perf
  analysis (docs/PERF-REVIEW.md) done; fixes merged (D-030, perf gate
  -22.75 %)

## In progress
- v0.2.1 (2026-10-24): forhunt 0.2.2, empty-window help. v0.2.0: open EVTX / $MFT / $J / folders directly (D-033). v0.1.1 released 2026-10-24 (toolbar labels); v0.1.0 the same day: Linux/Windows from the Release workflow,
  macOS .dmg built locally (D-031); all files verified against SHA256SUMS.
  Owner to test a browser-downloaded .dmg (Gatekeeper, Finder double-click).

## Not verifiable here
- The callgrind perf gate (needs valgrind; not available on macOS arm64).
- Gatekeeper first-launch flow of a quarantined download (needs a downloaded
  .dmg), Finder double-click default-handler behaviour, Windows packaging.
- `fhv-bench` reports peak_rss_mib=0 on macOS (reads /proc only).
