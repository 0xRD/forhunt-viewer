//! Virtual grid: draws only the visible rows of a table with any number of
//! rows (tested to 1e9).
//!
//! egui coordinates are `f32`: 100 M rows x 20 px = 2e9 px, far past f32's
//! exact range, so a regular `ScrollArea` would jitter and lose rows. The
//! grid keeps its vertical position as a fractional row index (`f64`), paints
//! rows relative to the viewport, and has its own vertical scrollbar.
//! Horizontal scrolling is in pixels. The first `frozen` columns do not
//! scroll horizontally.

use std::ops::Range;

use eframe::egui::{
    Align2, Color32, CursorIcon, FontId, Id, Key, Pos2, Rect, Response, Sense, Stroke, StrokeKind,
    Ui, pos2, vec2,
};

/// Rows the grid displays. Row indices are display positions (after filter
/// and sort), columns are display columns; mapping them to file rows and
/// file columns is the source's business.
pub trait GridSource {
    fn row_count(&self) -> u64;
    fn column_count(&self) -> usize;
    fn column_name(&self, col: usize) -> &str;
    /// Text appended to the header (sort arrow, filter mark).
    fn header_suffix(&self, _col: usize) -> String {
        String::new()
    }
    /// Called once per frame with the rows about to be drawn, before any
    /// `cell` call, so the source can fetch them in one go.
    fn prepare(&mut self, rows: Range<u64>);
    /// Text of a cell of a prepared row. Must be cheap.
    fn cell(&self, row: u64, col: usize) -> &str;
    /// Optional per-row background (tagged rows, bad rows).
    fn row_color(&self, _row: u64) -> Option<Color32> {
        None
    }
}

pub struct GridState {
    /// Fractional index of the top visible row.
    pub top: f64,
    pub scroll_x: f32,
    pub widths: Vec<f32>,
    pub selected: Option<u64>,
    pub selected_col: usize,
    /// Columns that stay at the left when scrolling horizontally.
    pub frozen: usize,
    /// Rows that fit in the viewport at the last frame.
    pub page_rows: u64,
    drag_thumb: Option<(f32, f64)>,
}

impl Default for GridState {
    fn default() -> Self {
        GridState {
            top: 0.0,
            scroll_x: 0.0,
            widths: Vec::new(),
            selected: None,
            selected_col: 0,
            frozen: 0,
            page_rows: 1,
            drag_thumb: None,
        }
    }
}

pub struct Style {
    pub row_h: f32,
    pub header_h: f32,
    /// Height of the filter row under the header (0 = none).
    pub filter_h: f32,
    pub font: FontId,
    pub pad: f32,
    pub scrollbar_w: f32,
}

impl Default for Style {
    fn default() -> Self {
        Style {
            row_h: 20.0,
            header_h: 24.0,
            filter_h: 0.0,
            font: FontId::monospace(12.5),
            pad: 5.0,
            scrollbar_w: 12.0,
        }
    }
}

/// Characters drawn per cell at most; the detail pane shows full values.
const MAX_CELL_CHARS: usize = 400;

/// Single-line, bounded version of a cell for the grid.
pub fn display_text(s: &str) -> std::borrow::Cow<'_, str> {
    let long = s.len() > MAX_CELL_CHARS;
    if !long && !s.bytes().any(|b| b < 0x20) {
        return std::borrow::Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len().min(MAX_CELL_CHARS + 4));
    for (n, c) in s.chars().enumerate() {
        if n == MAX_CELL_CHARS {
            out.push('…');
            break;
        }
        out.push(match c {
            '\n' => '⏎',
            '\r' | '\t' => ' ',
            c if (c as u32) < 0x20 => '·',
            c => c,
        });
    }
    std::borrow::Cow::Owned(out)
}

/// Byte ranges of case-insensitive (ASCII folding) matches of `needle`.
pub fn find_all(hay: &str, needle: &str) -> Vec<Range<usize>> {
    // Simple per-character case folding (the search matches with Unicode
    // case folding, so non-ASCII hits must highlight too).
    fn fold(c: char) -> char {
        let mut l = c.to_lowercase();
        match (l.next(), l.next()) {
            (Some(x), None) => x,
            _ => c,
        }
    }
    if needle.is_empty() {
        return Vec::new();
    }
    if hay.is_ascii() && needle.is_ascii() {
        let (lower, n) = (hay.to_ascii_lowercase(), needle.to_ascii_lowercase());
        let mut out = Vec::new();
        let mut from = 0;
        while let Some(p) = lower[from..].find(&n) {
            out.push(from + p..from + p + n.len());
            from += p + n.len();
        }
        return out;
    }
    let n: Vec<char> = needle.chars().map(fold).collect();
    let h: Vec<(usize, char)> = hay.char_indices().map(|(i, c)| (i, fold(c))).collect();
    let end = |k: usize| h.get(k).map_or(hay.len(), |x| x.0);
    let mut out = Vec::new();
    let mut k = 0;
    while k + n.len() <= h.len() {
        if h[k..k + n.len()].iter().map(|x| x.1).eq(n.iter().copied()) {
            out.push(h[k].0..end(k + n.len()));
            k += n.len();
        } else {
            k += 1;
        }
    }
    out
}

#[derive(Default)]
pub struct GridResponse {
    pub selection_changed: bool,
    pub clicked_cell: Option<(u64, usize)>,
    pub double_clicked: Option<u64>,
    /// Header click: (display column, shift held).
    pub header_clicked: Option<(usize, bool)>,
}

pub struct Callbacks<'a> {
    /// Draws the filter widget of a display column in its filter-row rect.
    pub filter_cell: &'a mut dyn FnMut(&mut Ui, usize, Rect),
    /// Context menu of a header.
    pub header_menu: &'a mut dyn FnMut(&mut Ui, usize),
    /// Context menu of a cell (display row, display column).
    pub cell_menu: &'a mut dyn FnMut(&mut Ui, u64, usize),
}

/// x extents of the display columns: (frozen part end, per-column [x0, x1)).
fn layout(state: &GridState, body: Rect) -> (f32, Vec<(f32, f32)>) {
    let frozen = state.frozen.min(state.widths.len());
    let mut xs = Vec::with_capacity(state.widths.len());
    let mut x = body.min.x;
    for w in &state.widths[..frozen] {
        xs.push((x, x + w));
        x += w;
    }
    let frozen_end = x;
    let mut x = frozen_end - state.scroll_x;
    for w in &state.widths[frozen..] {
        xs.push((x, x + w));
        x += w;
    }
    (frozen_end, xs)
}

#[allow(clippy::too_many_arguments)]
pub fn show(
    ui: &mut Ui,
    id: Id,
    state: &mut GridState,
    src: &mut dyn GridSource,
    st: &Style,
    highlight: &str,
    cb: Callbacks<'_>,
) -> GridResponse {
    let mut resp = GridResponse::default();
    let ncols = src.column_count();
    if state.widths.len() != ncols {
        state.widths.resize(ncols, 140.0);
    }
    state.selected_col = state.selected_col.min(ncols.saturating_sub(1));
    let n = src.row_count();
    let full = ui.available_rect_before_wrap();
    let outer = ui.allocate_rect(full, Sense::click_and_drag());
    let visuals = ui.visuals().clone();
    let painter = ui.painter_at(full);

    let top_h = st.header_h + st.filter_h;
    let body = Rect::from_min_max(
        pos2(full.min.x, full.min.y + top_h),
        pos2(full.max.x - st.scrollbar_w, full.max.y),
    );
    let page = ((body.height() / st.row_h).floor() as u64).max(1);
    state.page_rows = page;
    let max_top = n.saturating_sub(page) as f64;

    // Keyboard and wheel, unless a text field has focus.
    let typing = ui
        .ctx()
        .memory(|m| m.focused().is_some_and(|f| f != outer.id));
    if !typing {
        let (scroll, keys) = ui.input(|i| {
            (
                i.smooth_scroll_delta,
                [
                    i.key_pressed(Key::ArrowDown),
                    i.key_pressed(Key::ArrowUp),
                    i.key_pressed(Key::PageDown),
                    i.key_pressed(Key::PageUp),
                    i.key_pressed(Key::Home) && i.modifiers.command,
                    i.key_pressed(Key::End) && i.modifiers.command,
                    i.key_pressed(Key::ArrowLeft),
                    i.key_pressed(Key::ArrowRight),
                ],
            )
        });
        if outer.hovered() {
            state.top -= scroll.y as f64 / st.row_h as f64;
            state.scroll_x -= scroll.x;
        }
        let [down, up, pgdn, pgup, home, end, left, right] = keys;
        let last = n as i64 - 1;
        let mut sel = state.selected.map(|s| s as i64);
        let mut step = |d: i64| {
            sel = Some(match sel {
                Some(s) => (s + d).clamp(0, last.max(0)),
                None => 0,
            })
        };
        if n > 0 {
            if down {
                step(1);
            }
            if up {
                step(-1);
            }
            if pgdn {
                step(page as i64);
            }
            if pgup {
                step(-(page as i64));
            }
            if home {
                sel = Some(0);
            }
            if end {
                sel = Some(last);
            }
        }
        if left {
            state.selected_col = state.selected_col.saturating_sub(1);
        }
        if right && state.selected_col + 1 < ncols {
            state.selected_col += 1;
        }
        let new = sel.map(|s| s as u64);
        if new != state.selected {
            state.selected = new;
            resp.selection_changed = true;
            if let Some(s) = new {
                scroll_to(state, s);
            }
        }
        if left || right {
            reveal_col(state, body);
        }
    }

    // Vertical scrollbar.
    let bar = Rect::from_min_max(pos2(body.max.x, body.min.y), pos2(full.max.x, body.max.y));
    let bar_resp = ui.interact(bar, id.with("vbar"), Sense::click_and_drag());
    let thumb_h = if n == 0 {
        bar.height()
    } else {
        (bar.height() * page as f32 / n as f32).clamp(24.0_f32.min(bar.height()), bar.height())
    };
    let track = (bar.height() - thumb_h).max(1.0);
    if bar_resp.drag_started()
        && let Some(p) = bar_resp.interact_pointer_pos()
    {
        let thumb_y = bar.min.y + track * frac(state.top, max_top);
        if p.y < thumb_y || p.y > thumb_y + thumb_h {
            // Jump: centre the thumb on the click.
            let f = ((p.y - bar.min.y - thumb_h / 2.0) / track).clamp(0.0, 1.0) as f64;
            state.top = f * max_top;
        }
        state.drag_thumb = Some((p.y, state.top));
    }
    if bar_resp.dragged()
        && let (Some((y0, top0)), Some(p)) = (state.drag_thumb, bar_resp.interact_pointer_pos())
    {
        state.top = top0 + (p.y - y0) as f64 / track as f64 * max_top;
    }
    if bar_resp.drag_stopped() {
        state.drag_thumb = None;
    }
    state.top = state.top.clamp(0.0, max_top);

    // Horizontal extent (scrollable part).
    let frozen_w: f32 = state.widths[..state.frozen.min(ncols)].iter().sum();
    let scroll_w: f32 = state.widths[state.frozen.min(ncols)..].iter().sum();
    let view_w = (body.width() - frozen_w).max(0.0);
    state.scroll_x = state.scroll_x.clamp(0.0, (scroll_w - view_w).max(0.0));
    let (frozen_end, xs) = layout(state, body);
    // Clip of a column: frozen columns over the left part, others right of it.
    let clip_for = |c: usize, r: Rect| -> Rect {
        let area = if c < state.frozen {
            Rect::from_min_max(pos2(body.min.x, r.min.y), pos2(body.max.x, r.max.y))
        } else {
            Rect::from_min_max(pos2(frozen_end, r.min.y), pos2(body.max.x, r.max.y))
        };
        r.intersect(area)
    };

    // Header with resize handles, sort clicks and menus.
    let header = Rect::from_min_max(full.min, pos2(body.max.x, full.min.y + st.header_h));
    painter.rect_filled(header, 0.0, visuals.widgets.noninteractive.bg_fill);
    let sep = Stroke::new(1.0, visuals.widgets.noninteractive.bg_stroke.color);
    for (c, &(x0, x1)) in xs.iter().enumerate() {
        let r = Rect::from_min_max(pos2(x0, header.min.y), pos2(x1, header.max.y));
        let clip = clip_for(c, r);
        if clip.width() <= 0.0 {
            continue;
        }
        let hr = ui.interact(clip, id.with(("hdr", c)), Sense::click());
        if hr.hovered() {
            painter.rect_filled(clip, 0.0, visuals.widgets.hovered.weak_bg_fill);
        }
        let p = painter.with_clip_rect(clip.shrink2(vec2(2.0, 0.0)));
        p.text(
            pos2(r.min.x + st.pad, r.center().y),
            Align2::LEFT_CENTER,
            format!("{}{}", src.column_name(c), src.header_suffix(c)),
            FontId::proportional(13.0),
            visuals.strong_text_color(),
        );
        painter.line_segment(
            [pos2(x1 - 0.5, r.min.y + 3.0), pos2(x1 - 0.5, r.max.y - 3.0)],
            sep,
        );
        if hr.clicked() {
            let shift = ui.input(|i| i.modifiers.shift);
            resp.header_clicked = Some((c, shift));
        }
        hr.context_menu(|ui| (cb.header_menu)(ui, c));
        let handle = Rect::from_center_size(pos2(x1, r.center().y), vec2(8.0, st.header_h))
            .intersect(header);
        if handle.width() > 0.0 {
            let h = ui.interact(handle, id.with(("resize", c)), Sense::drag());
            if h.hovered() || h.dragged() {
                ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
            }
            if h.dragged() {
                state.widths[c] = (state.widths[c] + h.drag_delta().x).max(24.0);
            }
        }
    }
    // Filter row.
    if st.filter_h > 0.0 {
        let fr = Rect::from_min_max(pos2(full.min.x, header.max.y), pos2(body.max.x, body.min.y));
        painter.rect_filled(fr, 0.0, visuals.extreme_bg_color);
        for (c, &(x0, x1)) in xs.iter().enumerate() {
            let r = Rect::from_min_max(pos2(x0, fr.min.y), pos2(x1, fr.max.y));
            let clip = clip_for(c, r);
            // Only fully visible cells get a widget (egui widgets do not clip).
            if clip.width() + 0.5 >= r.width() && r.width() > 30.0 {
                (cb.filter_cell)(ui, c, r.shrink(1.5));
            }
        }
    }

    // Rows.
    let first = state.top.floor() as u64;
    let offset = (state.top - first as f64) as f32 * st.row_h;
    let count = (page + 2).min(n.saturating_sub(first));
    src.prepare(first..first + count);
    let body_p = painter.with_clip_rect(body);
    let text_color = visuals.text_color();
    let stripe = visuals.faint_bg_color;
    let sel_fill = visuals.selection.bg_fill;
    let hl_fill = Color32::from_rgba_unmultiplied(255, 200, 0, 90);
    let grid_line = Stroke::new(1.0, sep.color.gamma_multiply(0.5));
    let needle = highlight;
    for k in 0..count {
        let row = first + k;
        let y = body.min.y - offset + k as f32 * st.row_h;
        let rr = Rect::from_min_size(pos2(body.min.x, y), vec2(body.width(), st.row_h));
        if state.selected == Some(row) {
            body_p.rect_filled(rr, 0.0, sel_fill);
        } else if let Some(c) = src.row_color(row) {
            body_p.rect_filled(rr, 0.0, c);
        } else if row % 2 == 1 {
            body_p.rect_filled(rr, 0.0, stripe);
        }
        for (c, &(x0, x1)) in xs.iter().enumerate() {
            let cell = Rect::from_min_max(pos2(x0, y), pos2(x1, y + st.row_h));
            let clip = clip_for(c, cell.shrink2(vec2(st.pad, 0.0)));
            if clip.width() <= 0.0 || clip.height() <= 0.0 {
                continue;
            }
            if state.selected == Some(row) && state.selected_col == c {
                body_p.rect_stroke(
                    clip_for(c, cell),
                    0.0,
                    Stroke::new(1.5, visuals.strong_text_color()),
                    StrokeKind::Inside,
                );
            }
            let text = display_text(src.cell(row, c));
            if text.is_empty() {
                continue;
            }
            let p = body_p.with_clip_rect(clip);
            let galley = p.layout_no_wrap(text.to_string(), st.font.clone(), text_color);
            let origin = Pos2::new(cell.min.x + st.pad, cell.center().y - galley.size().y / 2.0);
            if !needle.is_empty() {
                for m in find_all(&text, needle) {
                    let a = galley.pos_from_cursor(eframe::egui::text::CCursor::new(
                        text[..m.start].chars().count(),
                    ));
                    let b = galley.pos_from_cursor(eframe::egui::text::CCursor::new(
                        text[..m.end].chars().count(),
                    ));
                    p.rect_filled(
                        Rect::from_min_max(
                            origin + vec2(a.min.x, 0.0),
                            origin + vec2(b.min.x, galley.size().y),
                        ),
                        2.0,
                        hl_fill,
                    );
                }
            }
            p.galley(origin, galley, text_color);
        }
    }
    // Column separators over the body, and the frozen edge.
    for (c, &(_, x1)) in xs.iter().enumerate() {
        let clip = clip_for(
            c,
            Rect::from_min_max(pos2(x1 - 1.0, body.min.y), pos2(x1, body.max.y)),
        );
        if clip.width() > 0.0 {
            body_p.line_segment(
                [pos2(x1 - 0.5, body.min.y), pos2(x1 - 0.5, body.max.y)],
                grid_line,
            );
        }
    }
    if state.frozen > 0 {
        body_p.line_segment(
            [pos2(frozen_end, full.min.y), pos2(frozen_end, body.max.y)],
            Stroke::new(2.0, visuals.widgets.active.bg_fill),
        );
    }

    // Scrollbar paint.
    painter.rect_filled(bar, 0.0, visuals.extreme_bg_color);
    let thumb = Rect::from_min_size(
        pos2(
            bar.min.x + 2.0,
            bar.min.y + track * frac(state.top, max_top),
        ),
        vec2(bar.width() - 4.0, thumb_h),
    );
    let thumb_col = if bar_resp.dragged() || bar_resp.hovered() {
        visuals.widgets.hovered.bg_fill
    } else {
        visuals.widgets.inactive.bg_fill
    };
    painter.rect_filled(thumb, 3.0, thumb_col);
    painter.rect_stroke(
        full,
        0.0,
        visuals.widgets.noninteractive.bg_stroke,
        StrokeKind::Inside,
    );

    // Clicks select a cell; right click selects and opens the cell menu.
    let hit = |p: Pos2| -> Option<(u64, usize)> {
        if !body.contains(p) {
            return None;
        }
        let row = (state.top + ((p.y - body.min.y) / st.row_h) as f64).floor() as u64;
        let col = xs.iter().enumerate().position(|(c, &(x0, x1))| {
            let r = clip_for(c, Rect::from_min_max(pos2(x0, p.y), pos2(x1, p.y + 1.0)));
            r.width() > 0.0 && p.x >= r.min.x && p.x < r.max.x
        })?;
        (row < n).then_some((row, col))
    };
    if (outer.clicked() || outer.double_clicked() || outer.secondary_clicked())
        && let Some(p) = outer.interact_pointer_pos()
        && let Some((row, col)) = hit(p)
    {
        if state.selected != Some(row) {
            resp.selection_changed = true;
        }
        state.selected = Some(row);
        state.selected_col = col;
        if outer.clicked() {
            resp.clicked_cell = Some((row, col));
        }
        if outer.double_clicked() {
            resp.double_clicked = Some(row);
        }
        outer.request_focus();
    }
    let sel = state.selected;
    let sel_col = state.selected_col;
    context_menu(&outer, |ui| {
        if let Some(r) = sel {
            (cb.cell_menu)(ui, r, sel_col);
        }
    });
    resp
}

fn context_menu(r: &Response, add: impl FnOnce(&mut Ui)) {
    r.context_menu(add);
}

fn frac(top: f64, max_top: f64) -> f32 {
    if max_top <= 0.0 {
        0.0
    } else {
        (top / max_top) as f32
    }
}

/// Makes `row` visible with minimal movement.
pub fn scroll_to(state: &mut GridState, row: u64) {
    let r = row as f64;
    if r < state.top {
        state.top = r;
    } else if r + 1.0 > state.top + state.page_rows as f64 {
        state.top = r + 1.0 - state.page_rows as f64;
    }
}

/// Scrolls horizontally so the selected column is visible.
fn reveal_col(state: &mut GridState, body: Rect) {
    let c = state.selected_col;
    if c < state.frozen || c >= state.widths.len() {
        return;
    }
    let frozen_w: f32 = state.widths[..state.frozen].iter().sum();
    let x0: f32 = state.widths[state.frozen..c].iter().sum();
    let x1 = x0 + state.widths[c];
    let view = (body.width() - frozen_w).max(1.0);
    if x0 < state.scroll_x {
        state.scroll_x = x0;
    } else if x1 > state.scroll_x + view {
        state.scroll_x = x1 - view;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_text_bounds_and_flattens() {
        assert_eq!(display_text("abc"), "abc");
        assert_eq!(display_text("a\nb\tc\0"), "a⏎b c·");
        let long = "x".repeat(10_000);
        assert_eq!(display_text(&long).chars().count(), MAX_CELL_CHARS + 1);
    }

    #[test]
    fn scroll_to_keeps_row_visible() {
        let mut s = GridState {
            page_rows: 10,
            ..Default::default()
        };
        scroll_to(&mut s, 50);
        assert_eq!(s.top, 41.0);
        scroll_to(&mut s, 3);
        assert_eq!(s.top, 3.0);
    }

    #[test]
    fn find_all_case_insensitive() {
        assert_eq!(find_all("Abc abc", "abc"), vec![0..3, 4..7]);
        assert!(find_all("x", "").is_empty());
        // Non-ASCII letters fold too, like the search itself (the row was a
        // hit but nothing was highlighted).
        assert_eq!(find_all("AUTORITÉ NT\\Système", "autorité"), vec![0..9]);
        assert_eq!(find_all("SYSTÈME", "Système"), vec![0..8]);
        assert_eq!(find_all("ÉCHEC échec", "ÉCHEC"), vec![0..6, 7..13]);
    }
}
