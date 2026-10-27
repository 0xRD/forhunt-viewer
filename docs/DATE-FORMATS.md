# Date formats

Which date and time formats forhunt-viewer reads, how a column is detected,
and how to set a column's format by hand. Code: `crates/fhv-engine/src/time.rs`
(parsers), `columns.rs` (detection); decisions D-050 to D-052.

Every time is stored as 100 ns ticks since 1970-01-01 UTC (FILETIME
resolution). Values written without an offset are UTC unless the column has a
zone (below).

## Where these come from

| Source | Typical value | Read as |
|---|---|---|
| forhunt, EvtxECmd, MFTECmd | `2024-03-01 12:34:56.1234567` | ISO 8601 |
| Hayabusa | `2024-03-01 12:34:56.123 +01:00` | ISO 8601 |
| Sysmon, Windows event XML, Elastic | `2024-03-01T12:34:56.123456789Z` | ISO 8601 |
| Splunk exports | `2024-03-01T12:34:56.000+01:00`, `03/01/2024 12:34:56` | ISO / numeric |
| plaso l2tcsv | `03/01/2024` + `12:34:56` (two columns) | numeric (M/d) |
| plaso dynamic / JSON | `2024-03-01T12:34:56.123456+00:00` | ISO 8601 |
| ICS / iCalendar, some Linux tools | `20240301T123456Z` | ISO basic |
| HTTP headers, mail (RFC 2822) | `Fri, 01 Mar 2024 12:34:56 GMT` | RFC 2822 |
| `date`, asctime, ctime | `Fri Mar  1 12:34:56 UTC 2024` | month names |
| syslog (RFC 3164), auth.log | `Mar  1 12:34:56` | year-less |
| Android logcat | `03-01 12:34:56.789` | year-less |
| Apache / nginx access log (CLF) | `[01/Mar/2024:12:34:56 +0100]` | CLF |
| IIS W3C logs | `2024-03-01 12:34:56` (date and time columns) | ISO 8601 |
| Excel / spreadsheet exports | `01/03/2024 12:34`, `3/1/2024 12:34 PM`, `45352.52` | numeric, Excel |
| French-locale exports | `01/03/2024 12:34:56`, `1 mars 2024`, `vendredi 1 mars 2024` | numeric (d/M), month names |
| Oracle / SQL dumps | `01-MAR-2024 12:34:56` | month names |
| Zeek, Suricata, Unix tools | `1709296496.123456` | Unix seconds |
| JavaScript, Java, Elastic `epoch_millis` | `1709296496123` | Unix ms |
| Windows registry / raw FILETIME | `133537700961234567` | FILETIME |
| .NET `DateTime.Ticks` | `638448932961234567` | .NET ticks |
| Chrome / Edge history (`last_visit_time`) | `13353770096123456` | WebKit |
| macOS / iOS databases (Core Data) | `730989296.25` | Cocoa |
| Sleuth Kit `fls -m` (body file) | `1709296496` per MAC field | Unix seconds |

## Date text (auto-detected)

- **ISO 8601 / RFC 3339**: `yyyy-MM-dd`, with `T` or a space, then
  `HH:mm[:ss[.f]]`; 1 to 9 fraction digits (`.` or `,`; beyond 7 digits the
  rest is truncated to 100 ns); offset `Z`, `+02:00`, `+0200`, `+02`,
  ` +02:00` (with a space) or ` UTC` / ` GMT`. `yyyy/MM/dd` and `yyyy.MM.dd`
  also. A date alone is midnight and, as a filter bound, the whole day.
- **ISO 8601 basic**: `yyyyMMddTHHmm[ss[.f]]` with an optional offset
  (`20240301T123456Z`, `20240301T143456.5+0200`). A bare `yyyyMMdd` is not a
  date (it cannot be told from a number).
- **Numeric, year last**: `d/M/yyyy`, `M/d/yyyy`, `d.M.yyyy`, `d-M-yyyy`,
  optionally followed (after a space, `T` or `, `) by `H:mm[:ss[.f]]`, an
  optional `AM` / `PM` and an offset. The day/month order is decided per
  column from the values (a first number above 12 is a day, a second above 12
  makes the first a month); with no evidence, slashes are read month first and
  dots day first; the column menu sets it.
- **Month names**, English and French, full or abbreviated, any case, with or
  without a trailing dot (`Mar`, `March`, `mars`, `févr.`, `AOÛT`), in the
  usual orders: `01-Mar-2024`, `1 March 2024`, `March 1st, 2024`,
  `Mar 1, 2024 1:00 PM`, `1er mars 2024 12:34`, a weekday first (`Fri`,
  `vendredi`, ignored), the time before or after the year
  (`Fri Mar  1 12:34:56 2024`), a zone word after the time (`GMT`, `UTC`,
  `UT`, `Z`, `EST`/`EDT`, `CST`/`CDT`, `MST`/`MDT`, `PST`/`PDT`, `CET`/`CEST`)
  or a numeric offset. Two-digit years after a month name (`01-MAR-24`): 00-69
  are 2000-2069, 70-99 are 1970-1999.
- **RFC 2822 / HTTP**: `Fri, 01 Mar 2024 12:34:56 GMT` / `-0500`.
- **Apache / CLF**: `01/Mar/2024:12:34:56 +0100`, with or without `[...]`.
- **Year-less**: syslog `Mar  1 12:34:56[.fff]` and `MM-dd HH:mm:ss[.fff]`
  (logcat). They need a year: detection uses the year of the file's last
  modification, and dates in a month after the modification month are put in
  the year before (a log written in February 2025 that starts in December is
  December 2024). The header shows `(year?)`, its tooltip and the column menu
  say which year was assumed (in the warning colour). The year can be set in
  *Date format…*.

A column is a time column when at least 90 % of its sampled non-blank cells
are date text (sample: the first 10 000 rows plus 64 slices across the file,
D-015), whatever mix of the kinds above.

## Numbers (epochs)

| Preset | Unit | Auto-detected when |
|---|---|---|
| Unix seconds | s since 1970, decimals allowed | every sampled value is in 1990-2040, or the header names a time and ≥ 99 % are in 1971-2100 |
| Unix milliseconds | ms since 1970 | same rule |
| Unix microseconds | µs since 1970 | same rule |
| Unix nanoseconds | ns since 1970 | same rule |
| Windows FILETIME | 100 ns since 1601 | same rule (18 digits, a narrow range) |
| .NET ticks | 100 ns since 0001-01-01 | same rule |
| Chrome / WebKit | µs since 1601 | same rule |
| Excel serial date | days since 1899-12-30, fraction = time of day | only when the header contains `excel`, `serial` or `oadate`, ≥ 99 % in 1971-2100 |
| Mac / Cocoa absolute time | s since 2001-01-01 | only when the header contains `cocoa`, `cfabsolute`, `nsdate`, `coredata` or `macabsolute` |
| Unix days | days since 1970 | only when the header contains `days`, `epochday` or `epoch_day` |

The header "names a time" when it contains `time`, `date`, `stamp`,
`created`, `modified`, `accessed`, `changed`, `written`, `updated`, `epoch`,
`when`, `visit`, `expir` or `birth`, or is `ts` / `*_ts` / `ts_*` / `*_at`.
Only plain decimal numbers count (no sign, hex or exponent); zeros are
ignored (tools write 0 for "never"); 99 % of the column's non-blank cells must
be such numbers. Units are tried in the order of the table's narrow ranges,
which do not overlap. Excel, Cocoa and day counts overlap ordinary numbers
(and Cocoa times of today overlap Unix seconds of 1993), so they are never
guessed from values alone: a Cocoa column without such a header reads as Unix
seconds and must be set by hand. Excel's 1900 leap-year bug is not modelled
(serials before 61, i.e. before 1900-03-01, are one day off).

Epoch columns show the parsed time in the grid (`yyyy-MM-dd HH:mm:ss.fffffff`,
in the display zone); text matching (filter row, contains, regex, global
search) matches the number in the file, and the chip and filter box say so.

## Custom format (column menu)

`strftime` / chrono style, matched against the whole cell:

| Spec | Meaning |
|---|---|
| `%Y` `%y` | 4-digit year; 2-digit year (00-69 = 20xx) |
| `%m` `%b` `%B` `%h` | month 1-12; month name (English or French, full or short) |
| `%d` `%e` | day 1-31 (`%e` allows a leading space) |
| `%j` | day of the year 1-366 |
| `%H` `%k` `%I` `%l` | hour (with `%p` for 12-hour clocks) |
| `%M` `%S` | minute, second |
| `%f` `%3f` `%6f` `%9f` | fraction digits (any number, or exactly n) |
| `%.f` | optional `.` or `,` followed by fraction digits |
| `%p` | AM / PM |
| `%z` | `Z`, `±HH:MM`, `±HHMM`, `±HH` |
| `%Z` | zone word (UTC, GMT, EST...) |
| `%a` `%A` | weekday name (ignored) |
| `%s` | Unix seconds (with decimals) |
| `%T` `%R` `%F` `%D` | `%H:%M:%S`, `%H:%M`, `%Y-%m-%d`, `%m/%d/%y` |
| `%%` | a literal `%` |

A space matches any run of spaces (or none); other characters match
themselves (case-insensitively). Flags such as `%-d` are accepted and ignored.
The format must name a day and a month, a day of the year, or `%s`.
Without `%Y` / `%y`, the year field of the dialog is used.

## Setting a column by hand

Right-click a header > *Date format…*:

- *Auto (as detected)*, *Not a date* (the column is then text, or numbers if
  it holds numbers), a *Preset* (date text or one of the epoch units), or a
  *Custom format*;
- the day/month order of year-last numeric dates;
- the zone of times written without an offset: `UTC`, an IANA name
  (`Europe/Paris`, with DST) or a fixed offset (`+05:30`). A wall-clock time
  that occurs twice in a DST change takes the earlier instant, one that does
  not exist the instant after the gap. Times with an offset, and epoch numbers,
  are never shifted;
- the year of year-less dates.

The dialog previews the first 20 non-blank values of the column as parsed
(UTC) and counts those that do not read as dates. The choice applies to
filters, sorts, the excluded-cells count and display at once; it is saved in
sessions (`date_formats`) and remembered per file in the settings
(`date_formats`, keyed by path), so the file reopens with it. The header
tooltip and the column menu show the detected or chosen format.

Range filter operands on a time column are date text (ISO or the column's
day/month order and year) in the display zone, or a value written like the
column's cells (an epoch number on an epoch column).

## The file's value, the parsed time, the display zone

Three things are easy to mix up:

| | What it is | Changed by |
|---|---|---|
| **File value** | the text in the file, e.g. `03/01/2024 14:05` or `1709296496` | nothing: copy, export, text filters and global search use it as is |
| **Parsed time** | the instant the value stands for, in UTC | the column's format (*Date format…*): order, preset or pattern, and the zone of times written without an offset |
| **Display zone** | the zone times are shown in | View > Time zone; display only |

Example: a column holds `2024-03-01 14:05:00` (no offset), written on a
machine in Paris.

- With the default zone (UTC) it is read as 14:05 UTC: wrong by an hour.
- Set *Zone of times without offset* to `Europe/Paris`: it is read as
  13:05 UTC (CET is UTC+1 in March), and shown as 13:05 with the display
  zone UTC, or 14:05 with the display zone Europe/Paris.
- A value with an offset (`2024-03-01T14:05:00+01:00`) or an epoch number
  is always the same instant: the column zone never shifts it.

## Dates in filters and queries

A date typed in a range filter, a chip or the query bar (`TimeCreated >=
2024-03-01 14:00`) is read in the **display zone**, like the times on
screen, so what you type matches what you see. A date alone means the
whole day: `<= 2024-03-01` keeps all of 1 March, `between 2024-03-01 and
2024-03-02` both days. Text operators (`contains`, `=`, regex) match the
file value, not the parsed time.

## What is saved

- The column format (mode, order, zone, year) is remembered per file path
  in the settings, so the file reopens with it, and saved in sessions.
- The display zone is a setting of the app, not of the file.
- Clearing a column back to *Auto* removes the remembered format.

## Fixing values that are not dates

The dialog previews the first 20 non-blank values and counts those that do
not parse. Typical cases:

| Preview shows | Fix |
|---|---|
| `13/02/2024` not a date, day/month unsure | *d/M or M/d*: day first |
| `01/03/24 14:05` (two-digit year) | Custom `%d/%m/%y %H:%M` |
| `2024.03.01-14h05m09` | Custom `%Y.%m.%d-%Hh%Mm%S` |
| `45352.52` read as a number | Preset *Excel serial date* |
| `730989296.25` read as Unix seconds in 1993 | Preset *Mac / Cocoa absolute time* |
| `-`, `N/A`, `0` among dates | nothing: they stay "not a date" and are counted as excluded cells by range filters |
| an id column (`EventRecordID`) taken for an epoch | *Not a date* |

The dialog's *Cheat sheet and examples* lists the patterns, and a click on
an example fills the custom field.

## Not supported

ISO week dates (`2024-W09-5`) and ordinal dates (`2024-061`) without a custom
format; two-digit years in numeric dates (`03/01/24`, use a custom format);
month names in other languages; time zone abbreviations other than those
listed; leap seconds (`:60` is read as `:59`).
