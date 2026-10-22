# forhunt-viewer

A native, very fast timeline and CSV viewer for forensic analysts. It opens
multi-GB CSV files (5-20 GB, tens of millions of rows) in seconds and
filters them interactively. It is the graphical companion of
[forhunt](https://github.com/0xRD/forensic-hunter).

It reads:
- forhunt CSV (EvtxECmd layout plus `ForhuntId`);
- Eric Zimmerman tools output (EvtxECmd, MFTECmd, PECmd, ...);
- Hayabusa, plaso/psort l2tcsv, and any other CSV / TSV.

![main window](docs/screenshots/main.png)

All data in the screenshots is synthetic (`tools/csv-gen`).

## Features

- **Instant open.** The first rows show in well under a second, while a
  parallel, quote-aware pass indexes the file in the background. The index
  is cached, so reopening a 5 GB file takes under a second.
- **Bounded memory.** The file is never loaded as rows. The grid reads only
  the visible rows; the index is 2-7 % of the file size.
- **Filters.**
  - Per-column filter row ("contains").
  - Condition editor: equals, not equals, contains, not contains,
    begins/ends with, blank, not blank, regex, `>` `>=` `<` `<=`, between
    (numbers and dates).
  - Match all / any.
  - Excel-style value list with counts and search.
  - Filter chips; clear all.
- **Global search** in every column, with match highlighting and next /
  previous (F3).
- **Sorting.** Multi-column (click, shift-click), deterministic (ties keep
  file order).
- **Columns.** Show/hide, reorder, resize, freeze the first N.
- **Detail pane.** Every field of the selected row, including the extra
  fields of malformed rows. JSON cells (e.g. `Payload`) are pretty-printed.
  Copy cell / row / column.
- **Tags and notes** on rows; "tagged only" filter.
- **Bad rows** (wrong field count, broken quotes) are counted and can be
  shown. A malformed file never crashes the viewer.
- **Export** of the current view or tagged rows to CSV or JSONL (visible
  columns, display order).
- **Sessions.** Save and restore filters, columns, sort and tags as JSON.
- **Files.**
  - Tabs, drag and drop, recent files.
  - Encodings: UTF-8 (with or without BOM), UTF-16LE/BE, Windows-1252.
  - Delimiters `,` `;` tab `|`, auto-detected.
- **Display time zone.** Times are converted for display only; the data
  stays as in the file.
- **Dark and light themes**, keyboard-friendly.
- **forhunt integration.** When a `ForhuntId` column is present:
  - copy `forhunt show <id> -p <evidence>` from a row's context menu;
  - export the tagged ids (one per line, TAB, note) for the forhunt skill
    report.

![filter value list](docs/screenshots/filter.png)

### Forensically sound

- Inputs are opened read-only and never modified.
- Index caches, UTF-16 conversions, settings and sessions go to the OS
  cache/config directories or a location you choose, never next to the
  evidence:
  - Linux: `~/.cache/forhunt-viewer`;
  - macOS: `~/Library/Caches/forhunt-viewer`;
  - Windows: `%LOCALAPPDATA%\forhunt-viewer`.
- Converted UTF-16 / Windows-1252 files need disk space in the cache
  directory up to the size of the original.

## Install

Downloads are on the [Releases](https://github.com/0xRD/forhunt-viewer/releases)
page. Each file has a `.sha256`, and `SHA256SUMS` lists them all.

| OS | Package |
|---|---|
| macOS (Apple Silicon) | `forhunt-viewer-<v>-macos-arm64.dmg` |
| Windows x86_64 | `forhunt-viewer-<v>-windows-x86_64.msi` (installer) or `.zip` (portable) |
| Linux x86_64 | `forhunt-viewer-<v>-linux-x86_64.AppImage` or `.tar.gz` |

The builds are **not code-signed** (no Apple Developer ID or Windows
certificate yet):

- **macOS.** Drag the app to Applications. The first time, right-click it
  and choose *Open*, then confirm. Alternatively:
  `xattr -dr com.apple.quarantine /Applications/forhunt-viewer.app`.
- **Windows.** SmartScreen may warn: *More info > Run anyway*.
- **Linux.** `chmod +x forhunt-viewer-*.AppImage`. The tarball holds the
  binary, a `.desktop` file and an icon.

A Homebrew cask (`0xrd/homebrew-tap`) will follow.

From source (Rust 1.95+):

```bash
cargo install --git https://github.com/0xRD/forhunt-viewer fhv-app
```

On Linux, building also needs `libxkbcommon-dev`, `libwayland-dev`,
`libx11-dev` and `libgl1-mesa-dev` (or your distribution's equivalents).

## "Open with"

forhunt-viewer adds itself to the *Open with* menu for CSV/TSV files. It
never becomes the default application unless you choose it.

- **macOS.** Right-click a `.csv` > *Open With* > *forhunt-viewer*. (Finder
  learns about the app once it is in Applications and has been opened once.)
- **Windows (MSI).** Right-click a `.csv` / `.tsv` > *Open with* >
  *forhunt-viewer*. To make it the default: *Choose another app* > tick
  *Always*.
- **Linux.** Install the `.desktop` file:

  ```bash
  install -Dm644 forhunt-viewer.desktop ~/.local/share/applications/forhunt-viewer.desktop
  install -Dm644 forhunt-viewer.png ~/.local/share/icons/hicolor/256x256/apps/forhunt-viewer.png
  ```

  Then use *Open With > forhunt-viewer* in your file manager. To make it the
  default: `xdg-mime default forhunt-viewer.desktop text/csv`.

Command line: `forhunt-viewer [FILE...]`.

## Keyboard shortcuts

| Keys | Action |
|---|---|
| Ctrl+O | open files |
| Ctrl+W | close tab |
| Ctrl+F | global search |
| F3 / Shift+F3 | next / previous match |
| Ctrl+G | go to row (`#` column) |
| Arrows, PgUp/PgDn, Ctrl+Home/End | move the selection |
| Ctrl+C / Ctrl+Shift+C | copy cell / copy row (tab-separated) |
| click / Shift+click a header | sort / add a sort column |
| right click | header and cell menus |
| Esc | cancel the running operation |

On macOS, Ctrl is Cmd.

## Performance

Measured on a 4-core cloud VM (Xeon 2.1 GHz, 15 GiB, virtio disk), warm
page cache unless noted. Details, commands and more results are in
[docs/PERF.md](docs/PERF.md).

| 5 GB file | hayabusa layout, 20.3 M rows | forhunt layout, 8.7 M rows |
|---|---:|---:|
| first rows on screen | 20-35 ms | 26-75 ms |
| row index (cold / warm) | 2.6 s / 1.3-1.9 s | 2.7 s / 1.7-1.9 s |
| column indexes | 3.8 s | 4.0 s |
| reopen with cached index | 0.45-0.66 s | 0.06-0.30 s |
| index size (rows + columns) | 362 MiB (6.7 %) | 202 MiB (3.8 %) |
| `in` / `not in` on an indexed column | 4-17 ms | 1-6 ms |
| `contains` on one text column | 0.55-1.0 s (then cached) | 0.5-2.0 s |
| global search | 0.6-0.8 s | 0.4 s |
| sort by time / indexed column | 0.17-0.32 s / 0.2 s | 0.02-0.08 s |
| sort by number | 2.2 s | 1.1 s |
| scrolling, frame CPU p99 (software OpenGL) | 11.3 ms | 10.1 ms |

Known slow case: a full sort of a long free-text column over 20 M rows takes
~11 s (it stays responsive and can be cancelled). The same sort on a filtered
view of 1 M rows takes 0.8 s. See DECISIONS.md D-013.

## How it works (short)

- `crates/fhv-engine` is a headless library.
  - **Row index.** A parallel row scanner runs a CSV state machine for
    every possible starting state at once. It packs the states into one
    byte and skips non-special bytes 8 at a time.
  - **Index storage.** About 4 bytes per row.
  - **Column indexes.**
    - roaring bitmaps per value of low-cardinality columns;
    - parsed times plus a sort permutation for the main time column.
  - **Filters.** Cached bitmaps, combined with bitmap algebra.
  - **Scans.** Text columns are scanned in parallel, behind a regex
    prefilter.
  - **Tests.** Property-tested against naive implementations.
- `crates/fhv-app` is the egui/eframe application with a custom virtual
  grid. Its scroll position is a row index, so 100 M+ rows scroll smoothly.
- `tools/csv-gen` generates synthetic, seeded test data: forhunt / EvtxECmd,
  MFTECmd, Hayabusa, plaso layouts, and an edge-case suite.

Design decisions with measurements: [DECISIONS.md](DECISIONS.md).

## Development

```bash
scripts/check.sh          # fmt, clippy -D warnings, all tests
scripts/check-gen.sh      # csv-gen output is deterministic (checksums)
cargo run --release -p csv-gen -- gen --layout forhunt --size 1G -o big.csv
cargo run --release -p fhv-bench -- index big.csv
cargo run --release -p fhv-bench -- query big.csv --ops bench/forhunt.ops
```

Test data is always generated. Never commit real evidence or logs.

## Licence

MIT OR Apache-2.0. Fonts: Inter and JetBrains Mono (SIL OFL 1.1). See
[THIRD-PARTY.md](THIRD-PARTY.md) for every bundled dependency.

forhunt-viewer is an independent project, inspired by the workflows of
timeline tools analysts already know. It contains no code or assets from
any of them.
