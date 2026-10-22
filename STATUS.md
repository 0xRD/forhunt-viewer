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
- v0.1.0: blocked on GitHub Actions billing (B-005). Code is release-ready
  at f247151 (local gate green); next: dispatch CI, then Release `tag=v0.1.0`

## Not verifiable here
- macOS / Windows packaging and the macOS Open With delegate run only in
  the release workflow (no Mac / Windows machine in the build container).
