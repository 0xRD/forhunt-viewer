# Changelog

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
