//! Auto-size a column to its widest value (D-071).
//!
//! Width = max(header, widest cell) + padding, clamped to
//! [`MIN_WIDTH`, `MAX_WIDTH`] (wide enough for the 400 characters the grid
//! draws at most). Measured: the rows on screen, [`SAMPLE`] rows spread
//! over the view, and the longest cells of the whole column (#34): from
//! its value dictionary when it has one, else from a parallel pass over
//! files up to [`FULL_SCAN_ROWS`] rows. Cells are measured as the grid
//! draws them (`grid::display_text`: one line, line breaks shown as ⏎, at
//! most 400 characters) in the grid font.

use std::ops::Range;

use eframe::egui::{Color32, FontId, Ui};
use fhv_engine::Rows;

use crate::grid;
use crate::tab::Tab;

pub const MIN_WIDTH: f32 = 24.0;
pub const MAX_WIDTH: f32 = 4000.0;
/// Files up to this many rows are read whole to find the longest cells.
pub const FULL_SCAN_ROWS: u64 = 2_000_000;
/// Longest cells (by length) measured precisely.
const LONGEST: usize = 24;
/// Rows sampled across the view, besides the visible ones.
pub const SAMPLE: u64 = 256;

/// Width for a column from the widest of `header` and `cells`; `measure`
/// gives the width of a text in points.
pub fn width(
    measure: &mut dyn FnMut(&str) -> f32,
    header: &str,
    cells: impl IntoIterator<Item = impl AsRef<str>>,
    pad: f32,
) -> f32 {
    let mut w = measure(header);
    for c in cells {
        let t = grid::display_text(c.as_ref());
        if !t.is_empty() {
            w = w.max(measure(&t));
        }
    }
    (w + 2.0 * pad + 1.0).clamp(MIN_WIDTH, MAX_WIDTH)
}

/// Header width: the label (capped like cells) plus its marks, room for
/// the sort arrow included whether or not the column is sorted, so the
/// arrow always shows and sorting later does not need a wider column
/// (#22). The marks come on top of the cap.
pub fn header_width(
    measure: &mut dyn FnMut(&str) -> f32,
    label: &str,
    marks: &str,
    pad: f32,
) -> f32 {
    let w = measure(label).min(MAX_WIDTH - 2.0 * pad - 1.0) + measure(marks);
    (w + 2.0 * pad + 1.0).max(MIN_WIDTH)
}

/// View positions to measure: `visible` plus `SAMPLE` evenly spaced ones.
pub fn sample_positions(n: u64, visible: Range<u64>) -> Vec<u64> {
    let k = SAMPLE.min(n);
    let mut v: Vec<u64> = (0..k).map(|i| i * n / k).collect();
    v.extend(visible.start.min(n)..visible.end.min(n));
    v.sort_unstable();
    v.dedup();
    v
}

/// Rows holding the longest cells of file column `c` (by byte length),
/// read in one parallel pass; empty when the file is too large or not
/// indexed yet.
fn longest_rows(t: &Tab, c: usize) -> Vec<u32> {
    let table = &t.table;
    let n = table.row_count();
    if n == 0 || n > FULL_SCAN_ROWS || !table.is_complete() {
        return Vec::new();
    }
    let tables = table.tables();
    let mut best: Vec<(u32, u32)> = Vec::new();
    let r = table.par_blocks(
        0..n,
        65_536,
        |b| {
            let mut top: Vec<(u32, u32)> = Vec::new();
            for k in 0..b.len() {
                if let Some(sp) = fhv_engine::scan::nth_field(tables, b.row(k), c) {
                    let len = sp.end - sp.start;
                    if top.len() < LONGEST || len > top[top.len() - 1].0 {
                        top.push((len, (b.first + k as u64) as u32));
                        top.sort_unstable_by_key(|x| std::cmp::Reverse(x.0));
                        top.truncate(LONGEST);
                    }
                }
            }
            top
        },
        |_, top| {
            best.extend(top);
            best.sort_unstable_by_key(|x| std::cmp::Reverse(x.0));
            best.truncate(LONGEST);
            Ok(())
        },
    );
    if r.is_err() {
        return Vec::new();
    }
    best.into_iter().map(|(_, row)| row).collect()
}

/// The longest values of a dictionary column (by characters).
fn longest_values(t: &Tab, c: usize) -> Vec<String> {
    let Some(ci) = t.table.column_index() else {
        return Vec::new();
    };
    let Some(d) = ci.cols.get(c).and_then(|col| col.dict.as_ref()) else {
        return Vec::new();
    };
    let mut v: Vec<&String> = d.values.iter().collect();
    v.sort_unstable_by_key(|s| std::cmp::Reverse(s.len()));
    v.into_iter().take(LONGEST).cloned().collect()
}

/// Sizes the file columns `cols` and stores the widths like a manual resize.
pub fn run(ui: &Ui, t: &mut Tab, st: &grid::Style, cols: &[usize]) {
    let top = t.grid.top.floor() as u64;
    let pos = sample_positions(t.line_count(), top..top + t.grid.page_rows + 2);
    let ids: Vec<u32> = pos.iter().filter_map(|&p| t.row_at(p)).collect();
    let mut rows = Rows::default();
    rows.max_cell = 4096;
    if !ids.is_empty() && t.table.read_row_list(0, &ids, &mut rows).is_err() {
        return;
    }
    let painter = ui.painter();
    let measure = |font: FontId| {
        move |s: &str| {
            painter
                .layout_no_wrap(s.to_string(), font.clone(), Color32::WHITE)
                .size()
                .x
        }
    };
    let mut body = measure(st.font.clone());
    let mut head = measure(FontId::proportional(13.0));
    for &c in cols {
        // Widest arrow, with a sort-key number.
        let mut marks = String::from(" ↓2");
        if t.filters.get(c).is_some_and(|f| f.is_active()) {
            marks.push_str(" ≡");
        }
        marks.push_str(&t.dupes.header_mark(c));
        let hw = header_width(&mut head, &t.table.columns[c], &marks, st.pad);
        let mut w = width(
            &mut body,
            "",
            (0..ids.len() as u64).map(|k| rows.cell(k, c)),
            st.pad,
        );
        // The longest cells of the whole column.
        let mut extra = longest_values(t, c);
        if extra.is_empty() {
            let long = longest_rows(t, c);
            let mut lr = Rows::default();
            lr.max_cell = 4096;
            if !long.is_empty() && t.table.read_row_list(0, &long, &mut lr).is_ok() {
                extra = (0..long.len() as u64)
                    .map(|k| lr.cell(k, c).to_string())
                    .collect();
            }
        }
        w = w.max(width(&mut body, "", &extra, st.pad));
        t.widths.insert(c, w.max(hw));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn width_is_widest_text_plus_padding_clamped() {
        let mut m = |s: &str| s.chars().count() as f32 * 7.0;
        assert_eq!(width(&mut m, "ab", ["x", "abcd"], 5.0), 4.0 * 7.0 + 11.0);
        assert_eq!(width(&mut m, "abcdef", ["x"], 5.0), 6.0 * 7.0 + 11.0);
        assert_eq!(width(&mut m, "", Vec::<&str>::new(), 0.0), MIN_WIDTH);
        // The grid draws 400 characters and an …: all of them fit (#34).
        let huge = "x".repeat(5000);
        assert_eq!(width(&mut m, "a", [huge.as_str()], 5.0), 401.0 * 7.0 + 11.0);
        let mut wide = |s: &str| s.chars().count() as f32 * 20.0;
        assert_eq!(width(&mut wide, "a", [huge.as_str()], 5.0), MAX_WIDTH);
        // Line breaks are measured as the grid draws them (one line).
        assert_eq!(width(&mut m, "", ["ab\ncd"], 0.0), 5.0 * 7.0 + 1.0);
    }

    #[test]
    fn header_keeps_room_for_the_sort_arrow() {
        let mut m = |s: &str| s.chars().count() as f32 * 7.0;
        // Short label: the arrow still fits.
        assert_eq!(header_width(&mut m, "a", " ↓2", 5.0), 4.0 * 7.0 + 11.0);
        // A label at the cap: the marks are added on top, not cut.
        let long = "x".repeat(1000);
        let w = header_width(&mut m, &long, " ↓2", 5.0);
        assert_eq!(w, MAX_WIDTH + 3.0 * 7.0);
        // Any size of font.
        let mut big = |s: &str| s.chars().count() as f32 * 20.0;
        assert_eq!(
            header_width(&mut big, "ab", " ↓2 ≡", 5.0),
            7.0 * 20.0 + 11.0
        );
        assert_eq!(header_width(&mut m, "", "", 0.0), MIN_WIDTH);
    }

    #[test]
    fn sample_is_bounded_and_covers_the_file() {
        let p = sample_positions(1_000_000_000, 500..540);
        assert!(p.len() <= SAMPLE as usize + 40);
        assert_eq!(p[0], 0);
        assert!(p.windows(2).all(|w| w[0] < w[1]));
        assert!(p.contains(&520) && *p.last().unwrap() > 990_000_000);
        assert_eq!(sample_positions(3, 0..10), vec![0, 1, 2]);
        assert!(sample_positions(0, 0..10).is_empty());
    }
}
