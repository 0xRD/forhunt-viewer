# forhunt-viewer

A native, very fast timeline and CSV viewer for forensic analysts. It opens
multi-GB CSV files (5-20 GB, tens of millions of rows) in seconds and
filters them interactively. It is the graphical companion of
[forhunt](https://github.com/0xRD/forensic-hunter).

It reads:
- **Windows event logs (`.evtx`), NTFS `$MFT` / `$UsnJrnl:$J`, or a whole
  evidence folder**, parsed by forhunt's own engine (built in, nothing to
  install) into one timeline;
- forhunt CSV (EvtxECmd layout plus `ForhuntId`);
- Eric Zimmerman tools output (EvtxECmd, MFTECmd, PECmd, ...);
- Hayabusa, plaso/psort l2tcsv, and any other CSV / TSV.

![main window](docs/screenshots/main.png)

All data in the screenshots is synthetic (`tools/csv-gen`).

## Features

- **Open evidence directly.** Drop `.evtx` files, `$MFT` / `$J`, or a folder
  (File > *Open evidence folder…*): forhunt parses them in the background
  (cancellable) into a CSV identical to `forhunt parse -F csv`, which opens
  as one timeline sorted by `TimeCreated`. Files dropped together become one
  timeline. The CSV is cached (not written next to the evidence), so
  reopening is instant; File > *Export rows…* saves it, or a filtered part.
  The forhunt evidence path is set for you, so *Copy `forhunt show`
  command* works straight away. 56,930 events (67 MB, 12 logs) take 0.1 s.
- **Instant open.** The first rows show in well under a second, while a
  parallel, quote-aware pass indexes the file in the background. The index
  is cached, so reopening a 5 GB file takes under a second.
- **Bounded memory.** The file is never loaded as rows. The grid reads only
  the visible rows; the index is 2-7 % of the file size.
  - Sorting a long text column maps the file read-only while it resolves
    ties. The OS reports those pages as resident memory, but they are page
    cache and the OS can reclaim them; see D-013.
- **Filters.**
  - Per-column filter row ("contains").
  - Condition editor: equals, not equals, contains, not contains,
    begins/ends with, blank, not blank, in list / not in list (one value
    per line, e.g. a pasted IOC list), regex, `>` `>=` `<` `<=`, between
    (numbers and dates; `<=` and the end of `between` include the whole
    day, minute or second written, so `between 2024-03-01 and 2024-03-01`
    is that day).
  - Match all / any.
  - Excel-style value list with counts and search. It, and the cell menu's
    "= / ≠ this value", select exact values (case and spaces count).
  - Filter chips: click a chip to invert it (`Channel = Security` becomes
    `Channel ≠ Security`; operators without an opposite are wrapped in
    NOT), click it again to restore it, click its **×** to remove it.
    *Clear filters* removes all. Inversions are saved in sessions.
- **Query bar** (Ctrl+L): type the whole filter as text, e.g.
  `Channel = Security AND (EventID in (4624, 4625) OR Message contains failed)`,
  then Enter. The chips show what it became, and editing chips rewrites
  the text. Errors point at the character and say what was expected;
  nothing is applied until the whole query is valid. Tab completes column
  names and operators. Recent queries are under *History*. See
  [Query language](#query-language).
- **Global search** in every column, with match highlighting and next /
  previous (F3). *Only matches* (on by default; your last choice is
  remembered) hides the rows without the search text; *Clear filters* also
  empties the search.
- **Row numbers.** If the file already has a row-number column
  (`RecordNumber`, `#`, `No.`, `Index`, `Line`, `Ligne`... whose values are
  a counter from 0 or 1 following the row position), the viewer's own `#`
  column is hidden and that column is used by *Go to row* (Ctrl+G) and the
  status bar; hover the status-bar row to see the file row. A column with
  gaps or repeats (time-sorted `EventRecordId`) is not taken. View >
  *Row numbers*: Auto / Always show # / Never show #, remembered per file.
- **Sorting.** Multi-column (click, shift-click), deterministic (ties keep
  file order). Click the `#` header to go back to file order, again for
  reverse file order (newest first in an appended log), a third time to
  clear: no sort job, whatever the file size.
- **Columns.** Show/hide, reorder, resize. Drag a header to move the column (a line shows
  the drop place, Esc cancels, the grid scrolls near its edges; a click still sorts); dropping
  across the frozen edge moves the edge by one. Double-click a header's right edge, or use
  *Auto-size column* / *Auto-size all columns* in the header menu, to fit the width to the
  widest value (sample of visible + 256 spread rows, max 800 px). *Freeze N cols* (toolbar, or
  right-click a header > *Freeze up to here*) pins the first N columns at
  the left while you scroll sideways.
- **Selection.** Like a spreadsheet: click a cell, drag a rectangle (the
  grid scrolls near its edges), Shift+click or Shift+arrows to extend,
  Ctrl+click / Ctrl+drag to add or remove cells and ranges, Ctrl+A for all.
  The thin gutter at the left of each row selects whole rows (click,
  Shift+click, Ctrl+click, drag; the box in its header selects all).
  Cells and rows are one selection at a time: selecting cells drops the
  selected rows and the reverse. Selected rows behave like cells spanning
  every visible column (copy, tag, export, status bar size and count /
  sum of the numbers in small selections). A new filter, sort or column order clears the ranges and
  the row selection (positions change); the active cell follows its row.
- **Copy as a table.** Only the rows and columns that hold a selected cell
  are copied (Ctrl+click cells in columns A and D: A and D, not B and C;
  unselected cells of those rows and columns are left empty), in display
  order. *Copy with headers* (Ctrl+C) copies the column names, then the
  values; *Copy values* (Ctrl+Alt+C, Cmd+Option+C on macOS) leaves the
  names out. The default format is set in
  View > Settings: **Markdown** (default since 0.5), TSV (pastes cell by
  cell into Excel / LibreOffice / Google Sheets), CSV with `,` or `;`,
  JSON, JSONL or HTML. Without headers JSON / JSONL rows are arrays, a
  Markdown table gets an empty header row. **HTML** copies a table styled
  like the current theme and grid font, with the coloring rules (fill, text
  color, bold), all inline so it survives pasting into mail, Word,
  Confluence or a ticket; plain-text targets get tab-separated text. One
  cell is copied as its bare value. Edit menu or right-click: *Copy with
  headers*, *Copy values*, *Copy as ▸* (a format once, with or
  without headers), and in the cell menu *Tag selected rows*, *Export
  selection…*. Values are the file's own (UTC, as written). Up to 1 M
  cells; beyond, use *Export selection…*.
- **Detail pane.** Every field of the selected row, including the extra
  fields of malformed rows. JSON cells (e.g. `Payload`) are pretty-printed.
  The field of the active cell is highlighted and scrolled into view, and
  follows the arrow keys; click a field name to select that cell in the
  grid (a hidden column's field is highlighted all the same).
  Copy cell / row / column. Close it with its ×, Ctrl+D (View > Detail
  pane) or Esc over it; it reopens at the same width.
- **Coloring rules** (View > *Coloring rules…*, or right-click a cell >
  *Color rows like this…*): a condition in the [query language](#query-language)
  (`Info contains Critical`, `EventID in (4624, 4625)`, `* contains mimikatz`
  for any column; case is ignored unless the condition ends with `cs`) plus a
  color of the theme's rule palette (red, orange, yellow, green, teal, blue,
  purple, gray: readable in every theme), as a fill and / or text color,
  optionally bold, for the whole row or only the matching cells. Rules can
  be disabled, reordered and deleted; the first rule covering a cell gives
  its style. *Count rows* shows how many rows of the view match each
  condition. Rules are saved in the settings as you edit them (the dialog
  shows where) and in sessions. **Rule sets**: *Save as set* keeps the
  rules under a name ("Logon events", "Persistence"...), then load, save,
  rename or delete sets in the dialog, or switch from View > *Rule set*.
  *Export… / Import…* share a ruleset as JSON; *Import as new set…* keeps
  a file as a set. Sessions remember which set they used. Only the rows on screen are
  evaluated, so scrolling costs the same with or without rules. The
  selected row keeps its selection color.
- **Duplicate / unique values**: right-click a header > *Highlight duplicates
  / unique*: Off, duplicates (2+ times), unique values (once) or both, counted
  over the filtered view (default) or the whole file; several columns at
  once. Blank cells are ignored, case follows the query bar's Aa setting.
  Cells get the theme's duplicate / unique color and a small bar at their
  left edge; the tooltip gives the number of occurrences, the header tooltip
  the totals. Dictionary columns are counted instantly, others in the
  background (the header shows the progress). Coloring rules win over the
  fill (the bar stays). At most 2,097,152 distinct values are counted per
  column (the header says "capped" past that).
- **Grouping.** Right-click a header > *Group by this column*, then again
  on other columns for nested groups (first chosen = outermost, like
  `Channel > EventId > User`). Group header rows show the value and the
  number of rows after the active filters, search and query, at every
  level; blank cells form a `(blank)` group. The group bar above the grid
  shows the levels in order: click one to sort its groups by value or by
  count (ascending / descending, per level; rows inside a group keep the
  row sort), move it in or out, or remove it; *Expand all* / *Collapse
  all* / *Clear grouping*. A new level starts as an overview: every group
  collapsed, largest first (Settings change both). *Hide grouped columns* (group bar; default in
  Settings) leaves the grouped columns out of the grid, copy and export of
  the visible columns, since their value is in the group headers; they
  come back at their place when the grouping goes. Click a header's arrow (or double-click it, or
  Left / Right on a selected header) to close / open it. Right-click a
  group header: *Filter to this group* (exact-value chips) or *Exclude
  this group* (a chip for an outer group, a `NOT (...)` query term for a
  nested one). Selection, copy, tagging and export skip the header rows;
  *Export* writes the view's rows flattened in group order (collapsed
  groups included). Grouping is saved in sessions. Indexed (dictionary)
  columns are counted from their index; others are read in a parallel
  pass with progress (Esc cancels). Past 1,000,000 groups the grouping is
  refused with a warning in the group bar.
- **Tags and notes** on rows; "tagged only" filter.
- **Bad rows** (wrong field count, broken quotes) are counted and can be
  shown. A malformed file never crashes the viewer.
- **Export** of the current view, the tagged rows or the selection to CSV
  or JSONL (visible columns, display order; grouped: in group order).
- **Sessions.** Save and restore filters (query included), columns, sort,
  grouping, tags, coloring rules and duplicate highlights as JSON.

- **Files.**
  - Tabs, drag and drop, recent files.
  - Encodings: UTF-8 (with or without BOM), UTF-16LE/BE, Windows-1252.
  - Delimiters `,` `;` tab `|`, auto-detected. When the header row was only a
    guess, the status bar says so; File > *Reopen with options…* sets the
    header and delimiter by hand.
  - Dates: detected per column in many shapes: ISO 8601 / RFC 3339 (basic
    format, 3-9 fraction digits, offsets), RFC 2822, d/M and M/d (12 h
    clocks), month names in English and French, Apache CLF, year-less syslog
    (the year assumed from the file date is flagged), Unix s / ms / µs / ns,
    FILETIME, .NET ticks, Chrome/WebKit, and Excel / Cocoa / day counts when
    the header says so ([docs/DATE-FORMATS.md](docs/DATE-FORMATS.md), also
  in the app: Help > Date formats).
    Right-click a header > *Date format…* to pick a preset, type a custom
    `strftime` format (live preview, unparsable count), set the zone of
    times without an offset or the year of year-less dates; the choice is
    kept in sessions and per file. Epoch numbers show as times.
    A range filter shows on its chip how many cells it could
    not read as dates or numbers, so rows are never left out silently.
- **Display time zone.** Times are converted for display only; the data
  stays as in the file. In date filters (`>`, `<`, between), a time typed
  without an offset means the display zone, like the times shown (a day
  is 23 or 25 hours across a DST change); `Z` or `+02:00` is taken as
  written. Text matching on time columns (filter row, contains, global
  search) matches the file's values (UTC for forensic tools): the filter
  chip and the search box say so in orange when the zone is not UTC.
- **Themes.** View > Theme: Dark, Dark soft, Dark high contrast, Light,
  Light high contrast, Dracula, Solarized dark / light, Nord, Gruvbox dark,
  or System (follows the OS light / dark setting). Hovering an entry previews
  it. Every theme passes WCAG AA contrast for text, the high-contrast ones
  AAA, and buttons, toggles, fields and chips have an outline of at least
  3:1 against the toolbar (checked by tests; a theme may set
  `control_border`, otherwise it is derived). A theme is one JSON file
  (`crates/fhv-app/assets/themes/`); your own go in `<config>/themes/*.json`
  (config = `~/Library/Application Support/forhunt-viewer` on macOS,
  `%APPDATA%\forhunt-viewer` on Windows, `~/.config/forhunt-viewer` on Linux).
- **Settings** (View > Settings…): theme, grid font (bundled JetBrains Mono
  or Inter, any installed font, monospaced only by default, or a font file),
  grid font size 8-32 pt (rows follow), and a separate UI scale for menus,
  panels and dialogs, the default copy format, and whether new tabs hide
  grouped columns. Changes apply at once, to all tabs, and are saved;
  *Reset to default* restores them. A font file that cannot be read is
  refused and the grid keeps its font.
- **Idle means idle**: no repaint without input or running work (0 % CPU),
  10 repaints per second for progress while a job runs (1 per second in the
  background).
- **forhunt integration.** When a `ForhuntId` column is present:
  - copy `forhunt show <id> -p <evidence>` from a row's context menu;
  - export the tagged ids (one per line, TAB, note) for the forhunt skill
    report.

![filter value list](docs/screenshots/filter.png)

### Limits

- Line endings: LF and CRLF. Files with old Mac line endings (CR only) are
  read as one long row.
- Up to 4 294 967 295 rows per file.

### Forensically sound

- Inputs are opened read-only and never modified.
- Index caches, UTF-16 conversions, settings and sessions go to the OS
  cache/config directories or a location you choose, never next to the
  evidence:
  - Linux: `~/.cache/forhunt-viewer`;
  - macOS: `~/Library/Caches/forhunt-viewer`;
  - Windows: `%LOCALAPPDATA%\forhunt-viewer`.
- Converted UTF-16 / Windows-1252 files need disk space in the cache
  directory (UTF-16: up to the original size; Windows-1252 with many
  accented letters: up to twice).
- The cache stays under 20 GiB (oldest files are removed first), and
  *View > Clear index cache* empties it.
- A cached index is reused when the path, size, modification time and a
  sample of the content (first, middle and last 64 KiB) match. A file
  changed elsewhere in its middle, with size and mtime preserved, would
  need a cache clear.

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

- **macOS (Apple Silicon).** Drag the app to Applications. The app is not
  notarized, so the first launch is blocked ("Apple could not verify..."):
  open *System Settings > Privacy & Security*, click *Open Anyway* next to
  the forhunt-viewer message and confirm (macOS 15 and later no longer offer
  right-click > *Open* for this). Alternatively, after checking the SHA-256:
  `xattr -dr com.apple.quarantine /Applications/forhunt-viewer.app`.
  Intel Macs: build from source (below).
- **Windows.** SmartScreen may warn: *More info > Run anyway*.
- **Linux.** `chmod +x forhunt-viewer-*.AppImage`. The tarball holds the
  binary, a `.desktop` file and an icon.

Homebrew (Apple Silicon): `brew install --cask 0xrd/tap/forhunt-viewer`.

From source (Rust 1.97; the toolchain is pinned in `rust-toolchain.toml`):

```bash
cargo install --git https://github.com/0xRD/forhunt-viewer fhv-app
```

On Linux, building also needs `libxkbcommon-dev`, `libwayland-dev`,
`libx11-dev` and `libgl1-mesa-dev` (or your distribution's equivalents).

## "Open with"

forhunt-viewer adds itself to the *Open with* menu for CSV/TSV files and
Windows event logs (`.evtx`; also folders on macOS). It never becomes the
default application unless you choose it.

- **macOS.** Right-click a `.csv`, `.evtx` or folder > *Open With* >
  *forhunt-viewer*. (Finder learns about the app once it is in Applications
  and has been opened once.)
- **Windows (MSI).** Right-click a `.csv` / `.tsv` / `.evtx` > *Open with* >
  *forhunt-viewer*. To make it the default: *Choose another app* > tick
  *Always*.
- **Linux.** Install the `.desktop` file:

  ```bash
  install -Dm644 forhunt-viewer.desktop ~/.local/share/applications/forhunt-viewer.desktop
  install -Dm644 forhunt-viewer.png ~/.local/share/icons/hicolor/256x256/apps/forhunt-viewer.png
  ```

  Then use *Open With > forhunt-viewer* in your file manager. To make it the
  default: `xdg-mime default forhunt-viewer.desktop text/csv`.

Command line: `forhunt-viewer [PATH...]` (CSV/TSV files, `.evtx`, `$MFT`,
`$J` or evidence folders).

## Query language

The query bar above the chips holds the column filters as text. Applying a
query (Enter or *Apply*) replaces the column filters (the search box,
*tagged only*, *bad rows* and the sort are not touched): each top-level
`AND` term that one column's filter can hold (a condition, an `OR` of
conditions on one column, an exact `in` list) becomes that column's chip;
the rest (an `OR` across columns, `NOT` of a group...) becomes one
`query:` chip. Clicking a chip inverts it and the text follows; **×**
removes it. Esc in the bar goes back to the filters in force.

```ebnf
query     = [ or_expr ] ;
or_expr   = and_expr { "OR" and_expr } ;
and_expr  = unary { "AND" unary } ;
unary     = "NOT" unary | primary ;
primary   = "(" or_expr ")" | condition ;
condition = column test [ "cs" | "ci" ] ;
column    = name | "*" ;                      (* * = any column *)
test      = ( "=" | "==" | "!=" | "<>" | ">" | ">=" | "<" | "<=" ) value
          | ( "contains" | "startswith" | "endswith" | "regex" | "matches" ) value
          | "between" value "and" value
          | "in" "(" [ value { "," value } ] ")"
          | "is" [ "not" ] "blank"
          | "not" test ;           (* "not contains", "not in" are operators *)
name      = value ;
value     = bare | '"' { char | '""' } '"' | "'" { char | "''" } "'" ;
bare      = char { char } ;  (* no space ( ) , = ! < > " ; not AND / OR / NOT *)
```

- Keywords are case-insensitive. Precedence: `NOT` > `AND` > `OR`.
- Quote column names and values with spaces or symbols: `"Event ID" = 4624`,
  `Path = "C:\Program Files\x.exe"`. Inside quotes, a quote is doubled
  (`"say ""hi"""`); backslashes are plain characters (paths and regexes
  need no escaping).
- Column names match exactly, else ignoring case. An unknown column is an
  error that lists the closest names.
- Case: the **Aa** toggle sets whether conditions match case; `cs` / `ci`
  after a condition overrides it for that condition. Regexes also accept
  `(?i)`.
- `col not startswith x` inverts one condition (like clicking its chip);
  `NOT (...)` inverts a group. Both give the same rows.
- Dates: `>`, `>=`, `<`, `<=` and `between` follow the display time zone
  rules above (a date without an offset means the display zone; `<=` and
  the end of `between` include the whole day / minute / second written).
- At most 128 levels of `NOT` and parentheses, and 64 KiB of text.

Examples:

```text
Channel = Security AND (EventID in (4624, 4625) OR Message contains "failed") AND NOT User startswith SYSTEM
TimeCreated between "2024-03-11 09:00" and "2024-03-11 18:00" AND EventID = 4688
CommandLine regex '-enc\s+[A-Za-z0-9+/=]{40,}' OR CommandLine contains mimikatz
User in (Administrator, admin) cs AND LogonType not in (3, 5)
* contains 10.0.0.5
TargetUserName is not blank AND NOT (IpAddress = "-" OR IpAddress = "::1")
```

## Keyboard shortcuts

| Keys | Action |
|---|---|
| Ctrl+O | open files |
| Ctrl+W | close tab |
| Ctrl+F | global search |
| Ctrl+L | query bar (Enter applies, Tab completes, Esc reverts) |
| Ctrl+D | show / hide the detail pane (or its × button) |
| F3 / Shift+F3 | next / previous match |
| Ctrl+G | go to row (`#` column, or the file's own row-number column) |
| Arrows, PgUp/PgDn, Ctrl+Home/End | move the selection |
| click / drag, Shift+click, Ctrl+click | select a cell / a range, extend it, add or remove cells and ranges |
| Shift+arrows | extend the range |
| Ctrl+A | select all cells |
| row gutter (left edge): click, Shift+click, Ctrl+click, drag | select rows |
| Ctrl+C | copy the selection with its column names, in the default format (one cell: its value) |
| Ctrl+Alt+C | copy the selected values only, without column names |
| Ctrl+Shift+C | copy the active row as indented JSON (right-click a cell for tab-separated) |
| click / Shift+click a header | sort / add a sort column |
| Left / Right on a group header, click its arrow, double-click it | close / open the group |
| right click | header and cell menus |
| Esc | cancel the running operation |
| Ctrl+= / Ctrl+- / Ctrl+0, Ctrl+wheel | grid font larger / smaller / default size |

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
view of 1 M rows takes 0.8 s. See docs/DECISIONS.md D-013.

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
  grid. Its scroll position is a row index, so huge files scroll smoothly
  (measured: 20 M real rows, and 1 B synthetic rows in an early grid
  prototype).
- `tools/csv-gen` generates synthetic, seeded test data: forhunt / EvtxECmd,
  MFTECmd, Hayabusa, plaso layouts, and an edge-case suite.

More: [docs/DESIGN.md](docs/DESIGN.md),
[docs/DECISIONS.md](docs/DECISIONS.md) (every design decision, with
measurements), [docs/PERF.md](docs/PERF.md),
[docs/ROADMAP.md](docs/ROADMAP.md) and [CHANGELOG.md](CHANGELOG.md).

## Testing

Filters, sorts and exports are checked against an independent
implementation, not against the viewer itself: a Python oracle (its own CSV,
time and number parsing) recomputed about 105,000 queries on a real
two-host lab timeline (56,930 events, French-locale Windows) and on every
synthetic layout and edge case (quoted newlines, UTF-16, Windows-1252,
ragged rows): every operator on every column, with and without case
sensitivity, value lists, AND/OR/NOT trees, global search, sorts, tags and
export round trips, with no mismatch. Unit, property and UI tests
(egui_kittest) run in CI on Linux, macOS and Windows.

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
