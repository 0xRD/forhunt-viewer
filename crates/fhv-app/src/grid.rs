//! Virtual grid: draws only the visible rows of a table with any number of
//! rows (tested to 1e9).
//!
//! egui coordinates are `f32`: 100 M rows x 20 px = 2e9 px, far past f32's
//! exact range, so a regular `ScrollArea` would jitter and lose rows. The
//! grid keeps its vertical position as a fractional row index (`f64`), paints
//! rows relative to the viewport, and has its own vertical scrollbar.
//! Horizontal scrolling is in pixels (column widths are small numbers).

use std::ops::Range;

use eframe::egui::{
    Align2, CursorIcon, FontId, Id, Key, Rect, Sense, Stroke, StrokeKind, Ui, pos2, vec2,
};

/// Rows the grid displays. Indices are display positions (after filter and
/// sort); mapping them to file rows is the source's business.
pub trait GridSource {
    fn row_count(&self) -> u64;
    fn column_count(&self) -> usize;
    fn column_name(&self, col: usize) -> &str;
    /// Called once per frame with the rows about to be drawn, before any
    /// `cell` call, so the source can fetch them in one go.
    fn prepare(&mut self, rows: Range<u64>);
    /// Text of a cell of a prepared row. Must be cheap.
    fn cell(&self, row: u64, col: usize) -> &str;
}

pub struct GridState {
    /// Fractional index of the top visible row.
    pub top: f64,
    pub scroll_x: f32,
    pub widths: Vec<f32>,
    pub selected: Option<u64>,
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
            page_rows: 1,
            drag_thumb: None,
        }
    }
}

pub struct Style {
    pub row_h: f32,
    pub header_h: f32,
    pub font: FontId,
    pub pad: f32,
    pub scrollbar_w: f32,
}

impl Default for Style {
    fn default() -> Self {
        Style {
            row_h: 20.0,
            header_h: 24.0,
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
            '\r' => ' ',
            '\t' => ' ',
            c if (c as u32) < 0x20 => '�',
            c => c,
        });
    }
    std::borrow::Cow::Owned(out)
}

pub struct Response {
    /// The selection changed this frame.
    pub selection_changed: bool,
    pub double_clicked: Option<u64>,
}

pub fn show(
    ui: &mut Ui,
    id: Id,
    state: &mut GridState,
    src: &mut dyn GridSource,
    st: &Style,
) -> Response {
    let mut resp = Response {
        selection_changed: false,
        double_clicked: None,
    };
    let ncols = src.column_count();
    if state.widths.len() != ncols {
        state.widths.resize(ncols, 140.0);
    }
    let n = src.row_count();
    let full = ui.available_rect_before_wrap();
    let outer = ui.allocate_rect(full, Sense::click_and_drag());
    let visuals = ui.visuals().clone();
    let painter = ui.painter_at(full);

    let body = Rect::from_min_max(
        pos2(full.min.x, full.min.y + st.header_h),
        pos2(full.max.x - st.scrollbar_w, full.max.y),
    );
    let page = ((body.height() / st.row_h).floor() as u64).max(1);
    state.page_rows = page;
    let max_top = n.saturating_sub(page) as f64;

    // Input: wheel, keys.
    if outer.hovered() || outer.has_focus() || ui.ctx().memory(|m| m.focused().is_none()) {
        let (scroll, keys) = ui.input(|i| {
            (
                i.smooth_scroll_delta,
                (
                    i.key_pressed(Key::ArrowDown),
                    i.key_pressed(Key::ArrowUp),
                    i.key_pressed(Key::PageDown),
                    i.key_pressed(Key::PageUp),
                    i.key_pressed(Key::Home) && i.modifiers.command,
                    i.key_pressed(Key::End) && i.modifiers.command,
                ),
            )
        });
        if outer.hovered() {
            state.top -= scroll.y as f64 / st.row_h as f64;
            state.scroll_x -= scroll.x;
        }
        let (down, up, pgdn, pgup, home, end) = keys;
        let mut sel = state.selected.map(|s| s as i64);
        let last = n as i64 - 1;
        let step = |d: i64, sel: &mut Option<i64>| {
            *sel = Some(match *sel {
                Some(s) => (s + d).clamp(0, last.max(0)),
                None => 0,
            })
        };
        if n > 0 {
            if down {
                step(1, &mut sel);
            }
            if up {
                step(-1, &mut sel);
            }
            if pgdn {
                step(page as i64, &mut sel);
            }
            if pgup {
                step(-(page as i64), &mut sel);
            }
            if home {
                sel = Some(0);
            }
            if end {
                sel = Some(last);
            }
        }
        let new = sel.map(|s| s as u64);
        if new != state.selected {
            state.selected = new;
            resp.selection_changed = true;
            if let Some(s) = new {
                scroll_to(state, s);
            }
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
        if p.y >= thumb_y && p.y <= thumb_y + thumb_h {
            state.drag_thumb = Some((p.y, state.top));
        } else {
            // Jump: centre the thumb on the click.
            let f = ((p.y - bar.min.y - thumb_h / 2.0) / track).clamp(0.0, 1.0) as f64;
            state.top = f * max_top;
            state.drag_thumb = Some((p.y, state.top));
        }
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

    // Horizontal extent.
    let total_w: f32 = state.widths.iter().sum();
    state.scroll_x = state.scroll_x.clamp(0.0, (total_w - body.width()).max(0.0));

    // Header with resize handles.
    let header = Rect::from_min_max(full.min, pos2(body.max.x, body.min.y));
    painter.rect_filled(header, 0.0, visuals.widgets.noninteractive.bg_fill);
    let mut x = body.min.x - state.scroll_x;
    for c in 0..ncols {
        let w = state.widths[c];
        let r = Rect::from_min_size(pos2(x, header.min.y), vec2(w, st.header_h));
        if r.max.x >= header.min.x && r.min.x <= header.max.x {
            let p = painter.with_clip_rect(r.intersect(header));
            p.text(
                pos2(r.min.x + st.pad, r.center().y),
                Align2::LEFT_CENTER,
                src.column_name(c),
                FontId::proportional(13.0),
                visuals.strong_text_color(),
            );
            p.line_segment(
                [
                    pos2(r.max.x - 0.5, r.min.y + 3.0),
                    pos2(r.max.x - 0.5, r.max.y - 3.0),
                ],
                Stroke::new(1.0, visuals.widgets.noninteractive.bg_stroke.color),
            );
            let handle =
                Rect::from_center_size(pos2(r.max.x, r.center().y), vec2(8.0, st.header_h));
            let h = ui.interact(
                handle.intersect(header),
                id.with(("resize", c)),
                Sense::drag(),
            );
            if h.hovered() || h.dragged() {
                ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
            }
            if h.dragged() {
                state.widths[c] = (state.widths[c] + h.drag_delta().x).max(24.0);
            }
        }
        x += w;
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
    let grid_line = Stroke::new(
        1.0,
        visuals
            .widgets
            .noninteractive
            .bg_stroke
            .color
            .gamma_multiply(0.5),
    );
    for k in 0..count {
        let row = first + k;
        let y = body.min.y - offset + k as f32 * st.row_h;
        let rr = Rect::from_min_size(pos2(body.min.x, y), vec2(body.width(), st.row_h));
        if state.selected == Some(row) {
            body_p.rect_filled(rr, 0.0, sel_fill);
        } else if row % 2 == 1 {
            body_p.rect_filled(rr, 0.0, stripe);
        }
        let mut x = body.min.x - state.scroll_x;
        for c in 0..ncols {
            let w = state.widths[c];
            if x + w >= body.min.x && x <= body.max.x {
                let cell = Rect::from_min_size(pos2(x, y), vec2(w, st.row_h));
                let text = display_text(src.cell(row, c));
                if !text.is_empty() {
                    body_p
                        .with_clip_rect(cell.shrink2(vec2(st.pad, 0.0)).intersect(body))
                        .text(
                            pos2(cell.min.x + st.pad, cell.center().y),
                            Align2::LEFT_CENTER,
                            text,
                            st.font.clone(),
                            text_color,
                        );
                }
            }
            x += w;
            if x > body.max.x {
                break;
            }
        }
    }
    // Column separators over the body.
    let mut x = body.min.x - state.scroll_x;
    for w in &state.widths {
        x += w;
        if x > body.min.x && x < body.max.x {
            body_p.line_segment(
                [pos2(x - 0.5, body.min.y), pos2(x - 0.5, body.max.y)],
                grid_line,
            );
        }
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

    // Clicks select.
    if (outer.clicked() || outer.double_clicked())
        && let Some(p) = outer.interact_pointer_pos()
        && body.contains(p)
    {
        let row = (state.top + ((p.y - body.min.y) / st.row_h) as f64).floor() as u64;
        if row < n {
            if state.selected != Some(row) {
                state.selected = Some(row);
                resp.selection_changed = true;
            }
            if outer.double_clicked() {
                resp.double_clicked = Some(row);
            }
        }
        outer.request_focus();
    }
    resp
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_text_bounds_and_flattens() {
        assert_eq!(display_text("abc"), "abc");
        assert_eq!(display_text("a\nb\tc\0"), "a⏎b c�");
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
}
