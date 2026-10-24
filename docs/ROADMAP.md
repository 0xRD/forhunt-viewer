# Roadmap

Open work, roughly by priority.

## Features
- Group-by with counts; column statistics and a time histogram.
- Colour rules.
- Per-tool column layouts.
- Merged timeline of several CSV files (evidence files already merge, D-033).
- JSONL input.

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
