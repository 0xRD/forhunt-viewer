# Roadmap

Open work, roughly by priority.

## Features
- Column statistics and a time histogram.
- Per-tool column layouts.
- Leftover from the 0.5.0 issues: screenshots of the header contrast
  audit (#21, ratios in D-087); needs an offscreen renderer for the tests
  (egui_kittest with wgpu) or a manual capture.
- Leftovers from the 0.4.0 issues: "only unique / only duplicate rows"
  filter (#13); row-number mode in Reopen with options (#9); modifier-click
  on a header to select the column (#15); drag to reorder group levels and
  per-group open/closed state in sessions (#5); NOT in the condition
  editor (#3); value autocomplete in the query bar (#1); column order per
  file outside sessions (#2); README theme screenshots (#12).
- Merged timeline of several CSV files (evidence files already merge, D-033).
- JSONL input.

## Input
- A hive's `.LOG1`/`.LOG2` or another binary file opened alone gives a garbage
  one-column CSV: refuse it with a message.
- Prefetch `ExecutableName` stops at 29 characters, as stored: say so in the UI.

## Platforms and packaging
- Signed and notarized macOS builds (needs an Apple Developer ID; until then
  users click Open Anyway once).
- Signed Windows builds (SmartScreen warns on the MSI and exe until then).
- Verify on Windows: MSI "Open with" registration (.csv, .tsv, .txt, .evtx)
  and the icon resource.
- `fhv-bench` peak memory on macOS (it reads /proc only).

## Engine
- Lossless dictionary values when two spellings of one value differ only in
  case (R-030).
- Full sort of long free-text columns (about 11 s on 20 M rows, D-013).
- More before/after benchmarks for performance changes.
