//! Copying the selection as a table (D-061): serializers (TSV, CSV, JSON,
//! Markdown, JSONL, HTML) and the glue that reads the selected cells or
//! rows of a tab. Values are the file's raw values (time zone and date
//! formats are display only), like Copy cell / Copy row.

// Lists of row ranges, not ranges meant as lists of numbers.
#![allow(clippy::single_range_in_vec_init)]

use std::ops::Range;
use std::sync::Arc;

use eframe::egui::Color32;
use serde::{Deserialize, Serialize};

use crate::selection::CellSel;
use crate::tab::{DC_FIRST, DC_NUM, DC_TAG, JobKind, JobResult, Tab};

/// Copy format. Markdown is the default since 0.5 (#26); a saved choice
/// is kept.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Format {
    Tsv,
    Csv,
    Json,
    #[default]
    Markdown,
    Jsonl,
    Html,
}

impl Format {
    pub const ALL: [Format; 6] = [
        Format::Markdown,
        Format::Tsv,
        Format::Csv,
        Format::Json,
        Format::Jsonl,
        Format::Html,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Format::Tsv => "TSV (tab-separated)",
            Format::Csv => "CSV",
            Format::Json => "JSON",
            Format::Markdown => "Markdown table",
            Format::Jsonl => "JSONL",
            Format::Html => "HTML table (theme colors)",
        }
    }
}

/// What goes on the clipboard: text, and for HTML the styled table (the
/// text is then its tab-separated fallback for plain-text targets).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Clip {
    pub text: String,
    pub html: Option<String>,
}

impl Clip {
    pub fn text(text: String) -> Clip {
        Clip { text, html: None }
    }
}

/// Colors and font of an HTML copy: the theme and grid font of the app.
#[derive(Clone, Debug)]
pub struct Look {
    pub bg: Color32,
    pub stripe: Color32,
    pub fg: Color32,
    pub header_bg: Color32,
    pub header_fg: Color32,
    pub border: Color32,
    pub font: String,
    pub size_pt: f32,
}

impl Default for Look {
    fn default() -> Look {
        Look {
            bg: Color32::WHITE,
            stripe: Color32::from_gray(245),
            fg: Color32::BLACK,
            header_bg: Color32::from_gray(230),
            header_fg: Color32::BLACK,
            border: Color32::from_gray(200),
            font: "monospace".into(),
            size_pt: 10.0,
        }
    }
}

/// Coloring-rule style of a copied row: the row fill and, per copied
/// column, (fill, text, bold).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RowPaint {
    pub bg: Option<Color32>,
    pub cells: Vec<(Option<Color32>, Option<Color32>, bool)>,
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

/// The table as text. `headers`: a header line (TSV, CSV), objects keyed
/// by column name instead of arrays (JSON, JSONL), the column names in
/// the header row instead of empty cells (Markdown needs a header row),
/// a `<thead>` (HTML). Every value is a string except JSON object / array
/// cells, embedded as JSON.
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
                if headers {
                    json_object(&mut out, &keys, r);
                } else {
                    json_array(&mut out, r);
                }
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
            if headers {
                line(&mut out, &mut names.iter().map(|n| md_cell(n)));
            } else {
                line(&mut out, &mut names.iter().map(|_| String::new()));
            }
            line(&mut out, &mut names.iter().map(|_| "---".to_string()));
            for r in rows {
                line(&mut out, &mut r.iter().map(|v| md_cell(v)));
            }
            out.pop();
        }
        Format::Html => out = html_table(names, rows, None, headers, &Look::default()),
    }
    out
}

fn json_array(out: &mut String, cells: &[String]) {
    out.push('[');
    for (i, v) in cells.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        match json_cell(v) {
            Some(j) => out.push_str(&j),
            None => out.push_str(&serde_json::to_string(v).unwrap_or_default()),
        }
    }
    out.push(']');
}

fn hex(c: Color32) -> String {
    format!("#{:02x}{:02x}{:02x}", c.r(), c.g(), c.b())
}

/// Cell text for HTML: every markup character escaped (evidence is
/// untrusted), line breaks as `<br>`.
fn html_escape(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    let mut chars = v.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push_str("<br>");
            }
            '\n' => out.push_str("<br>"),
            c => out.push(c),
        }
    }
    out
}

/// A `<table>` with inline styles only (mail and office clients drop
/// style sheets), colored like the grid: theme, stripes, rule colors.
pub fn html_table(
    names: &[String],
    rows: &[Vec<String>],
    paints: Option<&[RowPaint]>,
    headers: bool,
    look: &Look,
) -> String {
    let font: String = look
        .font
        .chars()
        .filter(|c| !matches!(c, '\'' | '"' | ';' | '<' | '>' | '&' | '\\'))
        .collect();
    let font = format!("'{font}',monospace;font-size:{:.1}pt", look.size_pt);
    let cell = format!(
        "border:1px solid {};padding:2px 6px;white-space:pre-wrap;vertical-align:top;\
         text-align:left;font-family:{font}",
        hex(look.border)
    );
    let mut out = format!(
        "<table style=\"border-collapse:collapse;background:{};color:{};font-family:{font}\">",
        hex(look.bg),
        hex(look.fg)
    );
    if headers {
        out.push_str("<thead><tr>");
        for n in names {
            out.push_str(&format!(
                "<th style=\"{cell};background:{};color:{};font-weight:bold\">{}</th>",
                hex(look.header_bg),
                hex(look.header_fg),
                html_escape(n)
            ));
        }
        out.push_str("</tr></thead>");
    }
    out.push_str("<tbody>");
    for (i, r) in rows.iter().enumerate() {
        let p = paints.and_then(|p| p.get(i));
        let row_bg = p
            .and_then(|p| p.bg)
            .unwrap_or(if i % 2 == 1 { look.stripe } else { look.bg });
        out.push_str(&format!("<tr style=\"background:{}\">", hex(row_bg)));
        for (k, v) in r.iter().enumerate() {
            let (bg, fg, bold) = p.and_then(|p| p.cells.get(k).copied()).unwrap_or_default();
            out.push_str(&format!(
                "<td style=\"{cell};background:{};color:{}{}\">{}</td>",
                hex(bg.unwrap_or(row_bg)),
                hex(fg.unwrap_or(look.fg)),
                if bold { ";font-weight:bold" } else { "" },
                html_escape(v)
            ));
        }
        out.push_str("</tr>");
    }
    out.push_str("</tbody></table>");
    out
}

/// What to copy: display rows (positions) x display columns, with a mask
/// of selected cells for a multi-range selection (other cells of the
/// kept rows and columns are empty).
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
    /// Grid lines of a grouped view (plan rows are lines).
    lines: Option<Arc<crate::groups::Lines>>,
    /// Coloring rules, for an HTML copy.
    styler: Option<Arc<crate::rules::Styler>>,
}

impl Reader {
    fn new(t: &Tab) -> Reader {
        Reader {
            table: t.table.clone(),
            view: t.view.clone(),
            order: t.order.clone(),
            tags: Arc::new(t.tags.clone()),
            lines: t.groups.lines.clone(),
            styler: None,
        }
    }

    fn file_col(&self, dc: usize) -> Option<usize> {
        dc.checked_sub(DC_FIRST)
            .and_then(|i| self.order.get(i))
            .copied()
    }

    fn name(&self, dc: usize) -> String {
        match dc {
            DC_TAG => "tag".into(),
            DC_NUM => "#".into(),
            _ => self
                .file_col(dc)
                .and_then(|c| self.table.columns.get(c).cloned())
                .unwrap_or_default(),
        }
    }

    /// The rows of the plan, in order, and their rule colors when there is
    /// a styler. Cancellable.
    fn read(&self, plan: &Plan) -> Result<(Vec<Vec<String>>, Vec<RowPaint>), String> {
        let positions = plan.rows.iter().flat_map(|r| r.clone());
        let mut out = Vec::with_capacity(plan.row_count().min(1 << 20) as usize);
        let mut paints = Vec::new();
        let mut buf = fhv_engine::Rows::default();
        let mut chunk: Vec<(u64, u32)> = Vec::with_capacity(4096);
        let ncols = self.table.columns.len();
        let mut flush = |chunk: &mut Vec<(u64, u32)>| -> Result<(), String> {
            if self.table.progress.cancelled() {
                return Err("cancelled".into());
            }
            let ids: Vec<u32> = chunk.iter().map(|x| x.1).collect();
            self.table
                .read_row_list(0, &ids, &mut buf)
                .map_err(|e| e.to_string())?;
            for (k, &(p, r)) in chunk.iter().enumerate() {
                let picked = |dc: usize| plan.mask.as_ref().is_none_or(|m| m.contains((p, dc)));
                let row = plan
                    .dcols
                    .iter()
                    .map(|&dc| {
                        if !picked(dc) {
                            return String::new();
                        }
                        match dc {
                            DC_TAG => match self.tags.get(&r) {
                                Some(n) if !n.is_empty() => n.clone(),
                                Some(_) => "★".into(),
                                None => String::new(),
                            },
                            DC_NUM => (r as u64 + 1).to_string(),
                            _ => self
                                .file_col(dc)
                                .map_or(String::new(), |c| buf.cell(k as u64, c).to_string()),
                        }
                    })
                    .collect();
                out.push(row);
                if let Some(st) = &self.styler {
                    let cells: Vec<&str> = (0..ncols).map(|c| buf.cell(k as u64, c)).collect();
                    let (bg, styles) = st.row(&cells);
                    paints.push(RowPaint {
                        bg,
                        cells: plan
                            .dcols
                            .iter()
                            .map(|&dc| match self.file_col(dc).filter(|_| picked(dc)) {
                                Some(c) => {
                                    let s = styles[c];
                                    (s.bg, s.fg, s.bold)
                                }
                                None => Default::default(),
                            })
                            .collect(),
                    });
                }
            }
            chunk.clear();
            Ok(())
        };
        for p in positions {
            let q = match crate::groups::pos_at(self.lines.as_deref(), p) {
                Some(q) => q,
                // Group headers are not data: skipped (D-077).
                None if self.lines.as_ref().is_some_and(|l| p < l.len()) => continue,
                None => break,
            };
            let Some(r) = self.view.row_at(q) else { break };
            chunk.push((p, r));
            if chunk.len() == 4096 {
                flush(&mut chunk)?;
            }
        }
        if !chunk.is_empty() {
            flush(&mut chunk)?;
        }
        Ok((out, paints))
    }
}

/// Display columns that are shown (hidden ones have no width).
fn shown(t: &Tab, cols: impl Iterator<Item = usize>) -> Vec<usize> {
    cols.filter(|&c| t.grid.widths.get(c).is_none_or(|&w| w > 0.0))
        .collect()
}

/// The active selection as a plan. Selected rows (row gutter) are the
/// same as cells spanning every shown column of those rows (#24). Cells:
/// only the rows and the columns holding a selected cell, in display
/// order, not the bounding rectangle (#27).
fn plan(t: &Tab, headers: bool) -> Option<Plan> {
    let g = &t.grid;
    if g.rows_active && !g.rows.set.is_empty() {
        return Some(Plan {
            rows: g.rows.set.ranges().to_vec(),
            dcols: shown(t, DC_FIRST..t.display_count()),
            mask: None,
            headers,
        });
    }
    let (rows, cols) = projection(&g.cells)?;
    Some(Plan {
        rows,
        dcols: shown(t, cols.into_iter()),
        mask: (g.cells.ranges() > 1).then(|| g.cells.clone()),
        headers,
    })
}

/// Rows and columns that hold at least one selected cell, sorted.
fn projection(cells: &CellSel) -> Option<(Vec<Range<u64>>, Vec<usize>)> {
    cells.bounds()?;
    let mut rows = crate::selection::RowSet::default();
    let mut cols = Vec::new();
    for b in cells.blocks() {
        rows.insert(b.r0..b.r1 + 1);
        cols.extend(b.c0..=b.c1);
    }
    cols.sort_unstable();
    cols.dedup();
    Some((rows.ranges().to_vec(), cols))
}

/// Copies the selection: returns what goes on the clipboard, or starts a
/// background copy (it arrives through `Tab::clipboard`), or says in the
/// status bar why not. `fmt` None: the default format, and a single cell
/// is copied as its bare value. `headers`: with the column names (Ctrl+C,
/// D-092) or values only (Ctrl+Alt+C, #28).
pub fn copy(
    t: &mut Tab,
    fmt: Option<Format>,
    default: Format,
    csv_delim: char,
    headers: bool,
    look: &Look,
) -> Option<Clip> {
    let Some(p) = plan(t, headers) else {
        t.status = Some("nothing selected".into());
        return None;
    };
    if fmt.is_none() && p.mask.is_none() && p.cells() == 1 {
        return single_cell(t).map(Clip::text);
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
    let mut rd = Reader::new(t);
    if fmt == Format::Html {
        rd.styler = crate::rules::Styler::new(t).map(Arc::new);
    }
    let names: Vec<String> = p.dcols.iter().map(|&c| rd.name(c)).collect();
    let status = format!(
        "copied {} cells{} as {}",
        crate::app::fmt_n(n),
        if p.headers {
            " with headers"
        } else {
            ", no headers"
        },
        fmt.label()
    );
    let look = look.clone();
    let build = move |rows: Vec<Vec<String>>, paints: Vec<RowPaint>| -> Clip {
        if fmt == Format::Html {
            let paints = (!paints.is_empty()).then_some(&paints[..]);
            Clip {
                html: Some(html_table(&names, &rows, paints, p.headers, &look)),
                text: render(Format::Tsv, csv_delim, &names, &rows, p.headers),
            }
        } else {
            Clip::text(render(fmt, csv_delim, &names, &rows, p.headers))
        }
    };
    if p.row_count() as usize <= SYNC_ROWS {
        return match rd.read(&p) {
            Ok((rows, paints)) => {
                t.status = Some(status);
                Some(build(rows, paints))
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
        let (rows, paints) = rd.read(&p)?;
        Ok(JobResult::Clip(build(rows, paints), status))
    });
    None
}

/// The active cell's bare value (a tag's note, the row number).
fn single_cell(t: &mut Tab) -> Option<String> {
    let r = t.grid.selected.and_then(|p| t.row_at(p))?;
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
    let p = plan(t, false)?;
    let cols = p.dcols.iter().filter_map(|&dc| t.file_col(dc)).collect();
    Some((p.rows, cols))
}

/// "3 rows x 2 cols" for the status bar, the same for cells and selected
/// rows, with count and sum of the numeric cells of small selections
/// (cached per selection).
pub fn summary(t: &mut Tab) -> Option<String> {
    let g = &t.grid;
    let rows_mode = g.rows_active && !g.rows.set.is_empty();
    if !rows_mode && !g.cells.is_multi() {
        return None;
    }
    let p = plan(t, false)?;
    let ranges = if rows_mode { 1 } else { g.cells.ranges() };
    let mut s = if ranges == 1 {
        format!(
            "{}: {} rows x {} cols",
            if rows_mode { "rows" } else { "cells" },
            crate::app::fmt_n(p.row_count()),
            p.dcols.len()
        )
    } else {
        format!(
            "cells: {} in {} ranges",
            crate::app::fmt_n(g.cells.cell_count()),
            ranges
        )
    };
    let key = (g.cells.clone(), g.rows.set.clone(), rows_mode);
    if t.sel_stats.as_ref().is_none_or(|(k, _)| *k != key) {
        let stats = (p.row_count() <= STATS_ROWS)
            .then(|| Reader::new(t).read(&p).ok())
            .flatten()
            .map(|(rows, _)| {
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
        let text = render(Format::Json, ',', &names, &rows, true);
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
        let jl = render(Format::Jsonl, ',', &names, &rows, true);
        let lines: Vec<&str> = jl.lines().collect();
        assert_eq!(lines.len(), 3);
        for l in lines {
            serde_json::from_str::<serde_json::Value>(l).unwrap();
        }
        assert_eq!(render(Format::Json, ',', &names, &[], false), "[]");
        // Numbers stay strings.
        let n = render(Format::Jsonl, ',', &s(&["n"]), &[s(&["0042"])], true);
        assert_eq!(n, "{\"n\":\"0042\"}");
        assert_eq!(
            unique_names(&s(&["a", "a", "a (2)", "a"])),
            s(&["a", "a (2)", "a (2) (2)", "a (3)"])
        );
    }

    #[test]
    fn markdown_escapes_pipes_and_newlines() {
        let (names, rows) = tricky();
        let text = render(Format::Markdown, ',', &names, &rows, true);
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

    /// Values only (#28): no header line, arrays instead of objects, an
    /// empty Markdown header row, no `<thead>`.
    #[test]
    fn values_only_per_format() {
        let (names, rows) = tricky();
        for (fmt, d) in [(Format::Tsv, '\t'), (Format::Csv, ',')] {
            let back = parse(&render(fmt, d, &names, &rows, false), d);
            assert_eq!(back, rows, "{fmt:?}: no header line");
        }
        let v: serde_json::Value =
            serde_json::from_str(&render(Format::Json, ',', &names, &rows, false)).unwrap();
        assert_eq!(v[0][0], "a\tb");
        assert_eq!(v[1][3]["k"][0], 1);
        assert_eq!(v[2], serde_json::json!(["", "", "", ""]));
        let jl = render(Format::Jsonl, ',', &names, &rows, false);
        assert!(jl.lines().all(|l| l.starts_with('[')));
        assert_eq!(jl.lines().count(), 3);
        let md = render(Format::Markdown, ',', &names, &rows, false);
        let lines: Vec<&str> = md.lines().collect();
        assert_eq!(lines[0], "|  |  |  |  |");
        assert_eq!(lines[1], "| --- | --- | --- | --- |");
        assert_eq!(lines.len(), 5);
        let html = render(Format::Html, ',', &names, &rows, false);
        assert!(!html.contains("<thead>") && !html.contains("Value"));
        assert!(render(Format::Html, ',', &names, &rows, true).contains("<th "));
    }

    #[test]
    fn html_is_escaped_and_styled_inline() {
        let names = s(&["<b>Name</b>", "Value"]);
        let rows = vec![
            s(&["<script>alert('x')</script>", "a & \"b\""]),
            s(&["line1\r\nline2", "  spaced  "]),
        ];
        let look = Look {
            bg: Color32::from_rgb(0x10, 0x20, 0x30),
            stripe: Color32::from_rgb(0x11, 0x21, 0x31),
            fg: Color32::from_rgb(0xee, 0xee, 0xee),
            font: "Jet\"Brains';<Mono>".into(),
            ..Look::default()
        };
        let red = Color32::from_rgb(0xff, 0, 0);
        let paints = vec![
            RowPaint {
                bg: Some(red),
                cells: vec![(None, None, true), (None, Some(Color32::BLACK), false)],
            },
            RowPaint::default(),
        ];
        let h = html_table(&names, &rows, Some(&paints), true, &look);
        assert!(!h.contains("<script>") && !h.contains("<b>"));
        assert!(h.contains("&lt;script&gt;alert(&#39;x&#39;)&lt;/script&gt;"));
        assert!(h.contains("a &amp; &quot;b&quot;"));
        assert!(h.contains("line1<br>line2"));
        assert!(h.contains("white-space:pre-wrap"));
        // The font name cannot leave its quotes.
        assert!(h.contains("'JetBrainsMono',monospace"));
        // Theme colors, stripes, the row rule fill, bold and text colors.
        assert!(h.contains("background:#102030;color:#eeeeee"));
        assert!(h.contains("<tr style=\"background:#ff0000\">"));
        assert!(h.contains("font-weight:bold\">&lt;script"));
        assert!(h.contains("color:#000000\">a &amp;"));
        assert!(h.contains("<tr style=\"background:#112131\">"));
        assert!(!h.contains("<style") && !h.contains("class="));
        assert_eq!(h.matches("<tr").count(), 3);
    }

    /// Ctrl+click on non-adjacent cells copies only their rows and
    /// columns (#27); a single range is unchanged.
    #[test]
    fn projection_keeps_selected_rows_and_columns_only() {
        let mut c = CellSel::default();
        c.set((2, 3));
        c.toggle((7, 6));
        let (rows, cols) = projection(&c).unwrap();
        assert_eq!(rows, vec![2..3, 7..8]);
        assert_eq!(cols, vec![3, 6]);
        c.toggle((4, 3));
        let (rows, cols) = projection(&c).unwrap();
        assert_eq!(rows, vec![2..3, 4..5, 7..8]);
        assert_eq!(cols, vec![3, 6]);
        c.set((1, 2));
        c.extend((3, 4));
        let (rows, cols) = projection(&c).unwrap();
        assert_eq!(rows, vec![1..4]);
        assert_eq!(cols, vec![2, 3, 4]);
        assert!(projection(&CellSel::default()).is_none());
    }

    #[test]
    fn markdown_is_the_default_and_a_saved_choice_stays() {
        assert_eq!(Format::default(), Format::Markdown);
        let fresh: crate::settings::Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(fresh.copy_format, Format::Markdown);
        let saved: crate::settings::Settings =
            serde_json::from_str(r#"{"copy_format":"Tsv"}"#).unwrap();
        assert_eq!(saved.copy_format, Format::Tsv);
        assert!(Format::ALL.contains(&Format::Html));
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
