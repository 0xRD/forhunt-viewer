//! Auto-size a column to its widest value (D-071).
//!
//! Width = max(header, widest sampled cell) + padding, clamped to
//! [`MIN_WIDTH`, `MAX_WIDTH`]. Only a bounded sample is measured, so the
//! cost does not depend on the file size: the rows on screen plus
//! [`SAMPLE`] rows spread evenly over the view. Cells are measured as the
//! grid draws them (`grid::display_text`: one line, line breaks shown as
//! ⏎, at most 400 characters) in the grid font.

use std::ops::Range;

use eframe::egui::{Color32, FontId, Ui};
use fhv_engine::Rows;

use crate::grid;
use crate::tab::Tab;

pub const MIN_WIDTH: f32 = 24.0;
pub const MAX_WIDTH: f32 = 800.0;
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

/// View positions to measure: `visible` plus `SAMPLE` evenly spaced ones.
pub fn sample_positions(n: u64, visible: Range<u64>) -> Vec<u64> {
    let k = SAMPLE.min(n);
    let mut v: Vec<u64> = (0..k).map(|i| i * n / k).collect();
    v.extend(visible.start.min(n)..visible.end.min(n));
    v.sort_unstable();
    v.dedup();
    v
}

/// Sizes the file columns `cols` and stores the widths like a manual resize.
pub fn run(ui: &Ui, t: &mut Tab, st: &grid::Style, cols: &[usize]) {
    let top = t.grid.top.floor() as u64;
    let pos = sample_positions(t.view.len(), top..top + t.grid.page_rows + 2);
    let ids: Vec<u32> = pos.iter().filter_map(|&p| t.view.row_at(p)).collect();
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
        let mut h = t.table.columns[c].clone();
        if t.sort.iter().any(|k| k.col == c) {
            h.push_str(" ↓2");
        }
        if t.filters.get(c).is_some_and(|f| f.is_active()) {
            h.push_str(" ≡");
        }
        let hw = width(&mut head, &h, Vec::<&str>::new(), st.pad);
        let w = width(
            &mut body,
            "",
            (0..ids.len() as u64).map(|k| rows.cell(k, c)),
            st.pad,
        );
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
        let huge = "x".repeat(5000);
        assert_eq!(width(&mut m, "a", [huge.as_str()], 5.0), MAX_WIDTH);
        // Line breaks are measured as the grid draws them (one line).
        assert_eq!(width(&mut m, "", ["ab\ncd"], 0.0), 5.0 * 7.0 + 1.0);
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
