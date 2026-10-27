# Changelog

## 0.5.1 (unreleased)

- Ctrl+C copies the column names with the values again (the default in
  0.5.0 was values only); Ctrl+Alt+C (Cmd+Option+C on macOS) copies the
  values only. *Copy as ▸* has "with headers" on by default. One cell is
  still copied as its bare value.
- Help > Date formats and the date dialog's *Learn more* did nothing (the
  app is built without opening a browser): the documentation now shows in
  the Settings window, in a *Date formats* tab.
- Esc with the pointer over the detail pane closes it (unless an operation
  is running: Esc cancels it first) (#30).
- View > Settings: how new groupings start, collapsed or open and the
  order of their groups (default: collapsed, largest first) (#18).
- Sessions record the coloring rule set they used; loading one makes it
  the active set, and adds it on a machine that does not have it (#20).

## 0.5.0 (2026-10-27)

Feedback on 0.4.0: fourteen issues on copy, selection, grouping, sorting,
the header and dates.

Behaviour changes
- Ctrl+C copies **values only**, without column names, for cell and row
  selections alike; *Copy with headers* is Ctrl+Alt+C (Cmd+Option+C on
  macOS) (#28).
- The default copy format is **Markdown** instead of TSV for new
  installs. A copy format saved by an earlier version (0.4.0 saved it in
  every settings file) is kept: pick Markdown in View > Settings to change
  it (#26).

Copy and selection
- Copy keeps only the rows and columns that hold a selected cell: two
  Ctrl+clicked cells in columns A and D copy A and D, not A to D (#27).
- Copy values / Copy with headers in the Edit menu and the cell menu, with
  shortcuts; *Copy as ▸* lists every format, marks the default and has a
  "with headers" toggle. Without headers JSON / JSONL rows are arrays and a
  Markdown table gets an empty header row. The status bar says what was
  copied ("12 cells, no headers as Markdown table") (#28).
- HTML copy: a table styled like the current theme and grid font, with the
  coloring rules, inline styles only, every cell escaped; plain-text
  targets get tab-separated text (#25).
- View > Settings chooses the default copy format among all of them (#26).
- Cell and row selections are exclusive: selecting cells drops the
  selected rows and the reverse; modifiers combine within one kind;
  Shift+arrows extend selected rows; selected rows copy, tag, export and
  sum like cells spanning every visible column (#24).

Grouping
- No column lines across group header rows, frozen edge included (#17).
- A new grouping, or a new nested level, starts collapsed with groups
  largest first; sessions keep their own state (#18).
- *Hide grouped columns* (group bar, default in Settings): the grouped
  columns leave the grid and come back at their place when the grouping
  goes; saved in sessions; copy and export follow the visible columns (#29).

Sorting and columns
- Click the `#` header: file order, reverse file order, off. No sort job
  on large files; saved in sessions (#23).
- Auto-size leaves room for the sort arrow even on unsorted columns, and a
  narrow header keeps its arrow visible and cuts the name instead (#22).

Display
- Header and toolbar: buttons, toggles, fields and chips get an outline of
  at least 3:1 against the toolbar in every theme (before: none outside
  the high-contrast themes), the accent on hover and press; checked by a
  test. Themes may set `control_border` (#21).
- Detail pane: a × button and Ctrl+D (View > Detail pane) (#30).

Coloring rules
- The dialog says rules are saved as you edit, and where. Named rule sets:
  save as, save, load, rename, delete, *Import as new set…*, and View >
  Rule set to switch (#20).

Dates
- Date format dialog: what each mode does, a strftime cheat sheet with
  examples you can click, explanations of the zone and year fields, a hint
  when values do not parse, a *Learn more* link (also in Help).
  `docs/DATE-FORMATS.md` explains the file value / parsed time / display
  zone, how typed dates in filters are read, what is saved, and how to fix
  values that are not dates (#19).

## 0.4.0 (2026-10-27)

Feature release from a colleague's review: sixteen issues, from a query
language and grouping to themes and an idle app that no longer uses CPU.

Filtering
- Query bar (Ctrl+L): write the whole filter as text, e.g.
  `Channel = Security AND (EventID in (4624, 4625) OR Message contains failed) AND NOT User startswith SYSTEM`.
  AND / OR / NOT with parentheses, quoted names and values, `in`,
  `between`, `is blank`, regex and `cs` / `ci`; dates follow the display
  time zone. A query fills the chips and editing chips rewrites the query;
  nothing is applied until the whole query is valid, errors point at the
  character and suggest close column names. Tab completes columns and
  operators; history and sessions keep queries (#1).
- Click a filter chip to invert it (NOT, shown as ≠ / NOT (…)), click its
  × to remove it; saved in sessions (#3).
- Global search: "only matches" is on by default and your last choice is
  remembered; Clear filters empties the search box (#7).
- Group rows by one or several columns (header menu > Group by this
  column, repeatable; first chosen = outermost): collapsible group headers
  with value and row count at every level, a `(blank)` group, a group bar to
  sort groups by value or count per level, move or remove levels, expand /
  collapse all. Group header menu: Filter to this group / Exclude this
  group. Export writes rows in group order; selection, copy and tagging skip
  header rows; grouping is saved in sessions. Counts come from the column
  index when there is one, otherwise a parallel scan (Esc cancels); more
  than 1,000,000 groups is refused with a warning (#5).

Display
- Coloring rules (View > Coloring rules…, cell menu "Color rows like
  this…"): color rows or matching cells from query-language conditions
  (`Info contains Critical`) with theme palette colors (fill, text, bold);
  first matching rule wins; saved in settings and sessions; export / import
  as JSON; row count per rule (#4).
- Highlight duplicate / unique values per column (header menu), over the
  filtered view or the whole file, several columns at once, occurrence count
  in the tooltip; counted from column indexes or a cancellable background
  scan with bounded memory (#13).
- Themes: Dark, Dark soft, Dark high contrast, Light, Light high contrast,
  Dracula, Solarized dark / light, Nord, Gruvbox dark, and System (follows
  the OS). View > Theme previews on hover. Themes are JSON files; your own
  go in `<config>/themes/`. Every theme passes WCAG AA text contrast, the
  high-contrast ones AAA (#12).
- View > Settings: theme, grid font (bundled, installed or a font file,
  monospaced only by default), grid font size 8-32 pt (Ctrl+= / Ctrl+- /
  Ctrl+0, Ctrl+wheel), UI scale, Reset to default. A font that cannot be
  read is refused and the previous font kept (#14).
- When the file already has a row-number column (RecordNumber, #, No.,
  Index, Line, Ligne…) the viewer's own # column is hidden and that column
  is used by Go to row (Ctrl+G) and the status bar. View > Row numbers:
  Auto / Always show # / Never show #, remembered per file. Gappy ids such
  as a time-sorted EventRecordId are not taken for a row counter (#9).

Dates
- Many more formats detected: ISO 8601 basic and offset variants, 3-9
  fraction digits, RFC 2822, 12h AM/PM, English and French month names,
  Apache CLF, and syslog without a year (the year comes from the file date
  and is flagged in the UI) (#10).
- Epoch columns detected and shown as times: Unix s / ms / µs / ns, Windows
  FILETIME, .NET ticks, Chrome / WebKit; Excel serial, Cocoa and Unix days
  when the header says so (#10).
- Column menu "Date format…": Auto, Not a date, a preset, or a custom
  strftime format with a live preview and the unparsable count; zone for
  times without an offset; year for year-less dates. Saved in sessions and
  remembered per file. docs/DATE-FORMATS.md lists every format (#10).

Columns
- Drag a column header to reorder: a drop line and a ghost show the target,
  Esc cancels, the grid scrolls near the edges, a plain click still sorts.
  Dropping across the frozen edge moves the edge by one (#2).
- Auto-size: double-click a header's right edge, or "Auto-size column" /
  "Auto-size all columns" in the header menu; fits the widest value among
  the visible rows and a sample of the file, up to 800 px (#8).
- Hidden columns come back at their previous position; the right half of a
  column's resize handle can be grabbed.

Selection and copy
- Spreadsheet-like selection: click, drag (auto-scroll), Shift+click /
  Shift+arrows to extend, Ctrl+click / Ctrl+drag for several ranges,
  Ctrl+A; size and count / sum of numbers in the status bar (#15).
- Row gutter at the start of each row: click / Shift / Ctrl / drag select
  whole rows, the header box selects all; Tag selected rows, Export
  selection (#16).
- Ctrl+C copies the selection as a table: TSV by default, or CSV (`,` /
  `;`), JSON, Markdown, JSONL (Settings); Copy as ▸ and Copy with headers
  in the cell menu; beyond 1 M cells use Export selection (#16).
- The detail pane highlights and scrolls to the active cell's field; click
  a field to select its cell (#6).
- Fixed: Ctrl+C did nothing once the grid had keyboard focus.

Performance
- No CPU use while idle: 0 frames/s and 0 % CPU, was about 120 frames/s
  and 20 % (the window title was re-sent every frame). While work runs the
  progress display repaints 10 times a second, once a second in the
  background (#11).

Other
- csv-gen: new `dates` layout. Index cache version 5 (existing caches are
  rebuilt once).

## 0.3.1 (2026-10-25)

- Arrow keys keep moving the selection after a click in the grid (they
  also moved the keyboard focus to the next widget, so later presses were
  lost).
- Copy row (Ctrl+Shift+C) gives indented JSON: one field per line, empty
  cells left out, JSON cells such as `Payload` nested as JSON. The
  tab-separated copy stays in the cell menu.

## 0.3.0 (2026-10-25)

Correctness release from a differential test against an independent Python
oracle (about 105,000 queries on a real lab timeline and every synthetic
layout). Several filters showed no rows, or the wrong rows; all are fixed
with tests.

Filters
- `<=` and the end of `between` include the whole day, minute or second
  written: `between 2025-10-21 and 2025-10-21` returned 0 of 56,930 rows.
- The value list and "= / ≠ this value" select exact values; ticking "admin"
  also showed "Admin" and "ADMIN".
- Times typed in date filters mean the display time zone (DST-aware); text
  filters on time columns say they match the file's UTC values.
- A range operand that is not a date-time is an error, not an empty result.
- Range filters show how many cells they cannot compare (excluded rows are
  visible); per-column day/month order.
- Integers above 2^53 (hex Keywords) compare exactly.
- Large case-insensitive `in` lists: hash lookups (2,000 values: 95 s ->
  0.02 s; 20,000 values no longer fail).
- "Bad rows only" is never dropped silently and is saved in sessions.

Other
- Small time/number columns sort by value, not as text.
- Export and Copy column refuse a view that is not the current filter.
- File > Reopen with options (header row, delimiter); a guessed header is
  shown in the status bar.
- F3 / Shift+F3 visit every hit in a sorted view; search highlights fold
  accented letters.
- Evidence conversion: unique temporary files (concurrent conversions),
  cache key includes file size and modification time, `forhunt show` uses
  the tab's own evidence; forhunt 0.3.0.
- Index cache format 4 (old caches are rebuilt).

## 0.2.1 (2026-10-24)

- Empty window lists what can be opened and how evidence files merge;
  forhunt 0.2.2.

## 0.2.0 (2026-10-24)

- Open `.evtx`, `$MFT`, `$J` and evidence folders directly (forhunt
  libraries), one timeline sorted by time, cached conversion.

## 0.1.1 (2026-10-24)

- Clearer "only matches" and "Freeze N cols" toolbar controls.

## 0.1.0 (2026-10-24)

- First release: multi-GB CSV timelines, filters, value lists, global
  search, sorting, tags, export, sessions; macOS, Windows and Linux packages.
