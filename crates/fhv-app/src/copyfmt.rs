//! Copying the selection as a table (D-061): serializers (TSV, CSV, JSON,
//! Markdown, JSONL) and the glue that reads the selected cells or rows of
//! a tab. Values are the file's raw values (time zone and date formats
//! are display only), like Copy cell / Copy row.

// Lists of row ranges, not ranges meant as lists of numbers.
#![allow(clippy::single_range_in_vec_init)]

use std::ops::Range;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::selection::CellSel;
use crate::tab::{DC_FIRST, DC_NUM, DC_TAG, JobKind, JobResult, Tab};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Format {
    #[default]
    Tsv,
    Csv,
    Json,
    Markdown,
    Jsonl,
}

impl Format {
    pub const ALL: [Format; 5] = [
        Format::Tsv,
        Format::Csv,
        Format::Json,
        Format::Markdown,
        Format::Jsonl,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Format::Tsv => "TSV (tab-separated)",
            Format::Csv => "CSV",
            Format::Json => "JSON",
            Format::Markdown => "Markdown table",
            Format::Jsonl => "JSONL",
        }
    }
}

/// More cells than this are not copied (Export selection instead).
pub const MAX_CELLS: u64 = 1_000_000;
/// Up to this many rows are read on the UI thread, more in the background.
const SYNC_ROWS: usize = 20_000;
/// Status-bar count / sum read at most this many rows.
const STATS_ROWS: u64 = 10_000;

/// Delimited text: a cell is quoted (quotes doubled) when it holds the
/// delimiter, a quote, CR or LF, as RFC 4180 and spreadsheets expect.
fn delimited(out: &mut String, delim: char, cells: &[String], eol: &str) {
    for (i, v) in cells.iter().enumerate() {
        if i > 0 {
            out.push(delim);
        }
        if v.contains([delim, '"', '\n', '\r']) {
            out.push('"');
            out.push_str(&v.replace('"', "\"\""));
            out.push('"');
        } else {
            out.push_str(v);
        }
    }
    out.push_str(eol);
}

/// JSON keys: duplicate names get " (2)", " (3)"..., empty ones "column N".
pub fn unique_names(names: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(names.len());
    for (i, n) in names.iter().enumerate() {
        let base = if n.is_empty() {
            format!("column {}", i + 1)
        } else {
            n.clone()
        };
        let mut name = base.clone();
        let mut k = 2;
        while out.contains(&name) {
            name = format!("{base} ({k})");
            k += 1;
        }
        out.push(name);
    }
    out
}

/// A JSON object or array cell, without whitespace outside strings; None
/// for any other value (kept as a string).
fn json_cell(v: &str) -> Option<String> {
    let s = v.trim();
    if !(s.starts_with('{') || s.starts_with('[')) {
        return None;
    }
    serde_json::from_str::<serde::de::IgnoredAny>(s).ok()?;
    let mut out = String::with_capacity(s.len());
    let (mut in_str, mut esc) = (false, false);
    for c in s.chars() {
        if in_str {
            out.push(c);
            if esc {
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else if c == '"' {
                in_str = false;
            }
        } else if c == '"' {
            in_str = true;
            out.push(c);
        } else if !c.is_whitespace() {
            out.push(c);
        }
    }
    Some(out)
}

fn json_object(out: &mut String, keys: &[String], cells: &[String]) {
    out.push('{');
    for (i, (k, v)) in keys.iter().zip(cells).enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&serde_json::to_string(k).unwrap_or_default());
        out.push(':');
        match json_cell(v) {
            Some(j) => out.push_str(&j),
            None => out.push_str(&serde_json::to_string(v).unwrap_or_default()),
        }
    }
    out.push('}');
}

fn md_cell(v: &str) -> String {
    v.replace('\\', "\\\\")
        .replace('|', "\\|")
        .replace("\r\n", "<br>")
        .replace(['\n', '\r'], "<br>")
}

/// The table as text. `headers` applies to TSV and CSV; JSON and JSONL
/// are keyed by column name, a Markdown table always has its header row.
/// Every value is a string except JSON object / array cells, embedded as
/// JSON.
pub fn render(
    fmt: Format,
    csv_delim: char,
    names: &[String],
    rows: &[Vec<String>],
    headers: bool,
) -> String {
    let mut out = String::new();
    match fmt {
        Format::Tsv | Format::Csv => {
            let (d, eol) = if fmt == Format::Tsv {
                ('\t', "\n")
            } else {
                (csv_delim, "\r\n")
            };
            if headers {
                delimited(&mut out, d, names, eol);
            }
            for r in rows {
                delimited(&mut out, d, r, eol);
            }
            out.truncate(out.len() - if out.is_empty() { 0 } else { eol.len() });
        }
        Format::Json | Format::Jsonl => {
            let keys = unique_names(names);
            let json = fmt == Format::Json;
            if json {
                out.push('[');
            }
            for (i, r) in rows.iter().enumerate() {
                if i > 0 {
                    out.push(if json { ',' } else { '\n' });
                }
                if json {
                    out.push_str("\n  ");
                }
                json_object(&mut out, &keys, r);
            }
            if json {
                out.push_str(if rows.is_empty() { "]" } else { "\n]" });
            }
        }
        Format::Markdown => {
            let line = |out: &mut String, cells: &mut dyn Iterator<Item = String>| {
                out.push('|');
                for c in cells {
                    out.push(' ');
                    out.push_str(&c);
                    out.push_str(" |");
                }
                out.push('\n');
            };
            line(&mut out, &mut names.iter().map(|n| md_cell(n)));
            line(&mut out, &mut names.iter().map(|_| "---".to_string()));
            for r in rows {
                line(&mut out, &mut r.iter().map(|v| md_cell(v)));
            }
            out.pop();
        }
    }
    out
}

/// What to copy: display rows (positions) x display columns, with a mask
/// of selected cells for a multi-range selection (other cells of the
/// bounding rectangle are empty).
struct Plan {
    rows: Vec<Range<u64>>,
    dcols: Vec<usize>,
    mask: Option<CellSel>,
    headers: bool,
}

impl Plan {
    fn row_count(&self) -> u64 {
        self.rows.iter().map(|r| r.end - r.start).sum()
    }

    fn cells(&self) -> u64 {
        self.row_count().saturating_mul(self.dcols.len() as u64)
    }
}

/// Reads what a plan needs from the tab, off the UI thread if need be.
#[derive(Clone)]
struct Reader {
    table: Arc<fhv_engine::Table>,
    view: fhv_engine::View,
    order: Vec<usize>,
    tags: Arc<std::collections::BTreeMap<u32, String>>,
}

impl Reader {
    fn new(t: &Tab) -> Reader {
        Reader {
            table: t.table.clone(),
            view: t.view.clone(),
            order: t.order.clone(),
            tags: Arc::new(t.tags.clone()),
        }
    }

    fn name(&self, dc: usize) -> String {
        match dc {
            DC_TAG => "tag".into(),
            DC_NUM => "#".into(),
            _ => dc
                .checked_sub(DC_FIRST)
                .and_then(|i| self.order.get(i))
                .and_then(|&c| self.table.columns.get(c).cloned())
                .unwrap_or_default(),
        }
    }

    /// The rows of the plan, in order. Cancellable.
    fn read(&self, plan: &Plan) -> Result<Vec<Vec<String>>, String> {
        let positions = plan.rows.iter().flat_map(|r| r.clone());
        let mut out = Vec::with_capacity(plan.row_count().min(1 << 20) as usize);
        let mut buf = fhv_engine::Rows::default();
        let mut chunk: Vec<(u64, u32)> = Vec::with_capacity(4096);
        let mut flush = |chunk: &mut Vec<(u64, u32)>| -> Result<(), String> {
            if self.table.progress.cancelled() {
                return Err("cancelled".into());
            }
            let ids: Vec<u32> = chunk.iter().map(|x| x.1).collect();
            self.table
                .read_row_list(0, &ids, &mut buf)
                .map_err(|e| e.to_string())?;
            for (k, &(p, r)) in chunk.iter().enumerate() {
                let row = plan
                    .dcols
                    .iter()
                    .map(|&dc| {
                        if plan.mask.as_ref().is_some_and(|m| !m.contains((p, dc))) {
                            return String::new();
                        }
                        match dc {
                            DC_TAG => match self.tags.get(&r) {
                                Some(n) if !n.is_empty() => n.clone(),
                                Some(_) => "★".into(),
                                None => String::new(),
                            },
                            DC_NUM => (r as u64 + 1).to_string(),
                            _ => dc
                                .checked_sub(DC_FIRST)
                                .and_then(|i| self.order.get(i))
                                .map_or(String::new(), |&c| buf.cell(k as u64, c).to_string()),
                        }
                    })
                    .collect();
                out.push(row);
            }
            chunk.clear();
            Ok(())
        };
        for p in positions {
            let Some(r) = self.view.row_at(p) else { break };
            chunk.push((p, r));
            if chunk.len() == 4096 {
                flush(&mut chunk)?;
            }
        }
        if !chunk.is_empty() {
            flush(&mut chunk)?;
        }
        Ok(out)
    }
}

/// Display columns that are shown (hidden ones have no width).
fn shown(t: &Tab, cols: impl Iterator<Item = usize>) -> Vec<usize> {
    cols.filter(|&c| t.grid.widths.get(c).is_none_or(|&w| w > 0.0))
        .collect()
}

/// The active selection as a plan: the selected rows (all visible file
/// columns, headers on) when the row gutter was used last, else the
/// bounding rectangle of the selected cells (headers off).
fn plan(t: &Tab, headers: Option<bool>) -> Option<Plan> {
    let g = &t.grid;
    if g.rows_active && !g.rows.set.is_empty() {
        return Some(Plan {
            rows: g.rows.set.ranges().to_vec(),
            dcols: (DC_FIRST..t.display_count()).collect(),
            mask: None,
            headers: headers.unwrap_or(true),
        });
    }
    let b = g.cells.bounds()?;
    Some(Plan {
        rows: vec![b.r0..b.r1 + 1],
        dcols: shown(t, b.c0..b.c1 + 1),
        mask: (g.cells.ranges() > 1).then(|| g.cells.clone()),
        headers: headers.unwrap_or(false),
    })
}

/// Copies the selection: returns the text for the clipboard, or starts a
/// background copy (the text arrives through `Tab::clipboard`), or says
/// in the status bar why not. `fmt` None: the default format, and a single
/// cell is copied as its bare value. `headers` None: the default (D-061).
pub fn copy(
    t: &mut Tab,
    fmt: Option<Format>,
    default: Format,
    csv_delim: char,
    headers: Option<bool>,
) -> Option<String> {
    let Some(p) = plan(t, headers) else {
        t.status = Some("nothing selected".into());
        return None;
    };
    if fmt.is_none() && !p.headers && p.mask.is_none() && p.cells() == 1 {
        return single_cell(t);
    }
    let fmt = fmt.unwrap_or(default);
    let n = p.cells();
    if n == 0 {
        return None;
    }
    if n > MAX_CELLS {
        t.status = Some(format!(
            "not copied: {} cells selected, the limit is {}; use Export selection… \
             (right-click) to write them to a file",
            crate::app::fmt_n(n),
            crate::app::fmt_n(MAX_CELLS)
        ));
        return None;
    }
    let rd = Reader::new(t);
    let names: Vec<String> = p.dcols.iter().map(|&c| rd.name(c)).collect();
    if p.row_count() as usize <= SYNC_ROWS {
        return match rd.read(&p) {
            Ok(rows) => {
                t.status = Some(format!("copied {} cells", crate::app::fmt_n(n)));
                Some(render(fmt, csv_delim, &names, &rows, p.headers))
            }
            Err(e) => {
                t.status = Some(format!("error: {e}"));
                None
            }
        };
    }
    if t.job.is_some() || !t.ready() {
        t.status = Some("busy: wait for the current operation".into());
        return None;
    }
    t.table.progress.reset_cancel();
    t.spawn(JobKind::CopyCells, None, move || {
        let rows = rd.read(&p)?;
        Ok(JobResult::Text(
            render(fmt, csv_delim, &names, &rows, p.headers),
            n,
        ))
    });
    None
}

/// The active cell's bare value (a tag's note, the row number).
fn single_cell(t: &mut Tab) -> Option<String> {
    let r = t.grid.selected.and_then(|p| t.view.row_at(p))?;
    let dc = t.grid.selected_col;
    Some(match dc {
        DC_TAG => t.tags.get(&r).cloned().unwrap_or_default(),
        DC_NUM => (r as u64 + 1).to_string(),
        _ => match (t.file_col(dc), t.table.row_fields(r as u64)) {
            (Some(c), Ok(f)) => f.get(c).cloned().unwrap_or_default(),
            _ => return None,
        },
    })
}

/// Rows (display positions) and file columns of the active selection, for
/// Export selection and Tag selected rows.
pub fn selection_rows(t: &Tab) -> Option<(Vec<Range<u64>>, Vec<usize>)> {
    let p = plan(t, None)?;
    let cols = p.dcols.iter().filter_map(|&dc| t.file_col(dc)).collect();
    Some((p.rows, cols))
}

/// "3 rows x 2 cols" / "12 rows" for the status bar, with count and sum
/// of the numeric cells of small selections (cached per selection).
pub fn summary(t: &mut Tab) -> Option<String> {
    let g = &t.grid;
    if g.rows_active && !g.rows.set.is_empty() {
        let n = g.rows.set.len();
        return Some(format!(
            "row selection: {} row{}",
            crate::app::fmt_n(n),
            if n == 1 { "" } else { "s" }
        ));
    }
    if !g.cells.is_multi() {
        return None;
    }
    let b = g.cells.bounds()?;
    let mut s = if g.cells.ranges() == 1 {
        format!(
            "cells: {} rows x {} cols",
            crate::app::fmt_n(b.rows()),
            b.cols()
        )
    } else {
        format!(
            "cells: {} in {} ranges",
            crate::app::fmt_n(g.cells.cell_count()),
            g.cells.ranges()
        )
    };
    let key = g.cells.clone();
    if t.sel_stats.as_ref().is_none_or(|(k, _)| *k != key) {
        let stats = (b.rows() <= STATS_ROWS)
            .then(|| plan(t, None))
            .flatten()
            .and_then(|p| Reader::new(t).read(&p).ok())
            .map(|rows| {
                let nums = rows
                    .iter()
                    .flatten()
                    .filter_map(|v| v.trim().parse::<f64>().ok().filter(|x| x.is_finite()));
                nums.fold((0u64, 0.0), |(n, s), x| (n + 1, s + x))
            });
        t.sel_stats = Some((key, stats));
    }
    if let Some((_, Some((n, sum)))) = &t.sel_stats
        && *n > 0
    {
        s.push_str(&format!("; {} numbers, sum {}", crate::app::fmt_n(*n), sum));
    }
    Some(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    fn tricky() -> (Vec<String>, Vec<Vec<String>>) {
        (
            s(&["Name", "Value", "Name", ""]),
            vec![
                s(&["a\tb", "say \"hi\"", "", "é ü 日本"]),
                s(&["line1\nline2", "x|y", "a,b;c", "{\"k\": [1, 2]}"]),
                s(&["", "", "", ""]),
            ],
        )
    }

    /// Minimal RFC 4180 reader (quotes doubled, quoted delimiters and line
    /// breaks) for the round trips.
    fn parse(text: &str, d: char) -> Vec<Vec<String>> {
        let (mut rows, mut row, mut cell) = (Vec::new(), Vec::new(), String::new());
        let mut chars = text.chars().peekable();
        let mut quoted = false;
        while let Some(c) = chars.next() {
            match (quoted, c) {
                (true, '"') if chars.peek() == Some(&'"') => {
                    chars.next();
                    cell.push('"');
                }
                (true, '"') => quoted = false,
                (true, c) => cell.push(c),
                (false, '"') if cell.is_empty() => quoted = true,
                (false, '\r') if chars.peek() == Some(&'\n') => {}
                (false, '\n') => {
                    row.push(std::mem::take(&mut cell));
                    rows.push(std::mem::take(&mut row));
                }
                (false, c) if c == d => row.push(std::mem::take(&mut cell)),
                (false, c) => cell.push(c),
            }
        }
        row.push(cell);
        rows.push(row);
        rows
    }

    #[test]
    fn tsv_and_csv_round_trip() {
        let (names, rows) = tricky();
        for (fmt, d) in [(Format::Tsv, '\t'), (Format::Csv, ','), (Format::Csv, ';')] {
            let text = render(fmt, d, &names, &rows, true);
            let back = parse(&text, d);
            assert_eq!(back[0], names, "{fmt:?}");
            assert_eq!(back[1..], rows[..], "{fmt:?} {d}: {text}");
        }
        assert_eq!(
            render(Format::Tsv, ',', &names, &rows, false),
            "\"a\tb\"\t\"say \"\"hi\"\"\"\t\té ü 日本\n\
             \"line1\nline2\"\tx|y\ta,b;c\t\"{\"\"k\"\": [1, 2]}\"\n\t\t\t"
        );
        let csv = render(Format::Csv, ';', &names[..2], &[s(&["a,b", "c;d"])], false);
        assert_eq!(csv, "a,b;\"c;d\"");
        let csv = render(Format::Csv, ',', &names[..2], &[s(&["1", "2"])], true);
        assert_eq!(csv, "Name,Value\r\n1,2");
    }

    #[test]
    fn json_keys_values_and_embedded_json() {
        let (names, rows) = tricky();
        let text = render(Format::Json, ',', &names, &rows, false);
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        let a = v.as_array().unwrap();
        assert_eq!(a.len(), 3);
        assert_eq!(a[0]["Name"], "a\tb");
        assert_eq!(a[0]["Name (2)"], "");
        assert_eq!(a[0]["column 4"], "é ü 日本");
        assert_eq!(a[0]["Value"], "say \"hi\"");
        assert_eq!(a[1]["column 4"]["k"][1], 2);
        assert_eq!(a[1]["Name"], "line1\nline2");
        assert_eq!(a[2]["Value"], "");
        // Keys in column order, one object per line.
        assert!(text.starts_with("[\n  {\"Name\":\"a\\tb\",\"Value\""));
        assert_eq!(text.lines().count(), 5);
        let jl = render(Format::Jsonl, ',', &names, &rows, false);
        let lines: Vec<&str> = jl.lines().collect();
        assert_eq!(lines.len(), 3);
        for l in lines {
            serde_json::from_str::<serde_json::Value>(l).unwrap();
        }
        assert_eq!(render(Format::Json, ',', &names, &[], false), "[]");
        // Numbers stay strings.
        let n = render(Format::Jsonl, ',', &s(&["n"]), &[s(&["0042"])], false);
        assert_eq!(n, "{\"n\":\"0042\"}");
        assert_eq!(
            unique_names(&s(&["a", "a", "a (2)", "a"])),
            s(&["a", "a (2)", "a (2) (2)", "a (3)"])
        );
    }

    #[test]
    fn markdown_escapes_pipes_and_newlines() {
        let (names, rows) = tricky();
        let text = render(Format::Markdown, ',', &names, &rows, false);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 5);
        assert_eq!(lines[0], "| Name | Value | Name |  |");
        assert_eq!(lines[1], "| --- | --- | --- | --- |");
        assert_eq!(
            lines[3],
            "| line1<br>line2 | x\\|y | a,b;c | {\"k\": [1, 2]} |"
        );
        assert_eq!(lines[4], "|  |  |  |  |");
    }

    #[test]
    fn empty_tables() {
        let names = s(&["a"]);
        assert_eq!(render(Format::Tsv, ',', &names, &[], false), "");
        assert_eq!(render(Format::Tsv, ',', &names, &[], true), "a");
        assert_eq!(render(Format::Jsonl, ',', &names, &[], true), "");
        assert_eq!(
            render(Format::Markdown, ',', &names, &[], true),
            "| a |\n| --- |"
        );
    }
}
