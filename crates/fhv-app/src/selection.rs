//! Cell and row selection of the grid (D-060). Both are compact: cells are
//! a few rectangles (display rows x display columns), rows a list of
//! ranges, never one entry per cell or row, so "select all" over 100 M
//! rows is one rectangle. Positions are display positions: the app clears
//! both when the filter, sort or column order changes.

// Lists of row ranges, not ranges meant as lists of numbers.
#![allow(clippy::single_range_in_vec_init)]

use std::ops::Range;

/// (display row, display column).
pub type Cell = (u64, usize);

/// Rectangle of cells, bounds inclusive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Block {
    pub r0: u64,
    pub r1: u64,
    pub c0: usize,
    pub c1: usize,
}

impl Block {
    pub fn span(a: Cell, b: Cell) -> Block {
        Block {
            r0: a.0.min(b.0),
            r1: a.0.max(b.0),
            c0: a.1.min(b.1),
            c1: a.1.max(b.1),
        }
    }

    pub fn contains(&self, (r, c): Cell) -> bool {
        (self.r0..=self.r1).contains(&r) && (self.c0..=self.c1).contains(&c)
    }

    pub fn rows(&self) -> u64 {
        self.r1 - self.r0 + 1
    }

    pub fn cols(&self) -> usize {
        self.c1 - self.c0 + 1
    }

    pub fn cells(&self) -> u64 {
        self.rows().saturating_mul(self.cols() as u64)
    }

    fn intersect(&self, o: &Block) -> Option<Block> {
        let b = Block {
            r0: self.r0.max(o.r0),
            r1: self.r1.min(o.r1),
            c0: self.c0.max(o.c0),
            c1: self.c1.min(o.c1),
        };
        (b.r0 <= b.r1 && b.c0 <= b.c1).then_some(b)
    }

    /// `self` without `o`: at most four rectangles (above, below, left,
    /// right of the overlap).
    fn minus(&self, o: &Block) -> Vec<Block> {
        let Some(x) = self.intersect(o) else {
            return vec![*self];
        };
        let mut out = Vec::new();
        if self.r0 < x.r0 {
            out.push(Block {
                r1: x.r0 - 1,
                ..*self
            });
        }
        if x.r1 < self.r1 {
            out.push(Block {
                r0: x.r1 + 1,
                ..*self
            });
        }
        let mid = Block {
            r0: x.r0,
            r1: x.r1,
            ..*self
        };
        if self.c0 < x.c0 {
            out.push(Block {
                c1: x.c0 - 1,
                ..mid
            });
        }
        if x.c1 < self.c1 {
            out.push(Block {
                c0: x.c1 + 1,
                ..mid
            });
        }
        out
    }

    /// The union of two disjoint blocks when it is a rectangle.
    fn merge(&self, o: &Block) -> Option<Block> {
        if self.c0 == o.c0 && self.c1 == o.c1 && (self.r1 + 1 == o.r0 || o.r1 + 1 == self.r0) {
            return Some(Block {
                r0: self.r0.min(o.r0),
                r1: self.r1.max(o.r1),
                ..*self
            });
        }
        if self.r0 == o.r0 && self.r1 == o.r1 && (self.c1 + 1 == o.c0 || o.c1 + 1 == self.c0) {
            return Some(Block {
                c0: self.c0.min(o.c0),
                c1: self.c1.max(o.c1),
                ..*self
            });
        }
        None
    }
}

/// Selected cells: committed rectangles (disjoint) plus the live one,
/// from the active cell (`anchor`) to `focus`, which Shift+click, drags
/// and Shift+arrows move.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CellSel {
    done: Vec<Block>,
    pub anchor: Option<Cell>,
    focus: Option<Cell>,
}

impl CellSel {
    pub fn clear(&mut self) {
        *self = CellSel::default();
    }

    /// One cell (a plain click).
    pub fn set(&mut self, p: Cell) {
        *self = CellSel {
            done: Vec::new(),
            anchor: Some(p),
            focus: Some(p),
        };
    }

    /// Live range from the anchor to `p` (Shift+click, drag).
    pub fn extend(&mut self, p: Cell) {
        if self.anchor.is_none() {
            self.set(p);
        } else {
            self.focus = Some(p);
        }
    }

    /// The far corner of the live range (where Shift+arrows move from).
    pub fn focus(&self) -> Option<Cell> {
        self.focus.or(self.anchor)
    }

    /// Ctrl+click: removes `p` when selected, else starts a new range at
    /// it. `p` becomes the active cell. True when `p` was added.
    pub fn toggle(&mut self, p: Cell) -> bool {
        let selected = self.contains(p);
        self.commit();
        self.anchor = Some(p);
        if selected {
            let one = Block::span(p, p);
            self.done = self.done.iter().flat_map(|b| b.minus(&one)).collect();
            self.normalize();
            false
        } else {
            self.focus = Some(p);
            true
        }
    }

    pub fn select_all(&mut self, rows: u64, cols: usize) {
        if rows == 0 || cols == 0 {
            return;
        }
        self.done = vec![Block {
            r0: 0,
            r1: rows - 1,
            c0: 0,
            c1: cols - 1,
        }];
        self.focus = None;
        self.anchor.get_or_insert((0, 0));
    }

    fn live(&self) -> Option<Block> {
        Some(Block::span(self.anchor?, self.focus?))
    }

    /// Makes the live range a committed one.
    fn commit(&mut self) {
        if let Some(l) = self.live() {
            self.done = self.done.iter().flat_map(|b| b.minus(&l)).collect();
            self.done.push(l);
            self.normalize();
        }
        self.focus = None;
    }

    /// Merges neighbours whose union is a rectangle.
    fn normalize(&mut self) {
        'again: loop {
            for i in 0..self.done.len() {
                for j in i + 1..self.done.len() {
                    if let Some(m) = self.done[i].merge(&self.done[j]) {
                        self.done[i] = m;
                        self.done.swap_remove(j);
                        continue 'again;
                    }
                }
            }
            break;
        }
    }

    /// All rectangles (they may overlap the live one).
    pub fn blocks(&self) -> impl Iterator<Item = Block> + '_ {
        self.done.iter().copied().chain(self.live())
    }

    pub fn contains(&self, p: Cell) -> bool {
        self.blocks().any(|b| b.contains(p))
    }

    pub fn ranges(&self) -> usize {
        self.done.len() + self.live().is_some() as usize
    }

    /// Distinct selected cells.
    pub fn cell_count(&self) -> u64 {
        let Some(l) = self.live() else {
            return self.done.iter().map(Block::cells).sum();
        };
        let rest: u64 = self
            .done
            .iter()
            .map(|b| b.cells() - b.intersect(&l).map_or(0, |x| x.cells()))
            .sum();
        rest + l.cells()
    }

    /// More than one cell selected.
    pub fn is_multi(&self) -> bool {
        self.ranges() > 1 || self.blocks().next().is_some_and(|b| b.cells() > 1)
    }

    /// Bounding rectangle.
    pub fn bounds(&self) -> Option<Block> {
        self.blocks().reduce(|a, b| Block {
            r0: a.r0.min(b.r0),
            r1: a.r1.max(b.r1),
            c0: a.c0.min(b.c0),
            c1: a.c1.max(b.c1),
        })
    }
}

/// Sorted, disjoint, non-adjacent ranges of display rows.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RowSet {
    ranges: Vec<Range<u64>>,
}

impl RowSet {
    pub fn ranges(&self) -> &[Range<u64>] {
        &self.ranges
    }

    pub fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }

    pub fn len(&self) -> u64 {
        self.ranges.iter().map(|r| r.end - r.start).sum()
    }

    pub fn contains(&self, row: u64) -> bool {
        let i = self.ranges.partition_point(|r| r.end <= row);
        self.ranges.get(i).is_some_and(|r| r.start <= row)
    }

    pub fn insert(&mut self, add: Range<u64>) {
        if add.is_empty() {
            return;
        }
        let mut out: Vec<Range<u64>> = Vec::with_capacity(self.ranges.len() + 1);
        let mut add = Some(add);
        for r in std::mem::take(&mut self.ranges) {
            if let Some(a) = &add
                && a.start < r.start
            {
                push_merge(&mut out, add.take().unwrap_or_default());
            }
            push_merge(&mut out, r);
        }
        if let Some(a) = add {
            push_merge(&mut out, a);
        }
        self.ranges = out;
    }

    pub fn remove(&mut self, cut: Range<u64>) {
        let mut out = Vec::with_capacity(self.ranges.len() + 1);
        for r in std::mem::take(&mut self.ranges) {
            if r.end <= cut.start || cut.end <= r.start {
                out.push(r);
                continue;
            }
            if r.start < cut.start {
                out.push(r.start..cut.start);
            }
            if cut.end < r.end {
                out.push(cut.end..r.end);
            }
        }
        self.ranges = out;
    }
}

fn push_merge(out: &mut Vec<Range<u64>>, r: Range<u64>) {
    match out.last_mut() {
        Some(l) if r.start <= l.end => l.end = l.end.max(r.end),
        _ => out.push(r),
    }
}

/// Rows selected with the row gutter.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RowSel {
    pub set: RowSet,
    anchor: Option<u64>,
    /// Selection before the press, which a drag adds its span to.
    base: RowSet,
    moved: bool,
}

impl RowSel {
    pub fn clear(&mut self) {
        *self = RowSel::default();
    }

    /// Press on the gutter: a click toggles the row (alone), Ctrl+click
    /// toggles it among the others, Shift+click selects from the last
    /// clicked row (added to the rest with Ctrl).
    pub fn press(&mut self, row: u64, ctrl: bool, shift: bool) {
        self.moved = false;
        if shift && let Some(a) = self.anchor {
            self.base = if ctrl {
                self.set.clone()
            } else {
                RowSet::default()
            };
            self.set = self.base.clone();
            self.set.insert(a.min(row)..a.max(row) + 1);
            self.moved = true;
            return;
        }
        self.anchor = Some(row);
        if ctrl {
            if self.set.contains(row) {
                self.set.remove(row..row + 1);
            } else {
                self.set.insert(row..row + 1);
            }
            self.base = self.set.clone();
        } else {
            let only = self.set.ranges() == [row..row + 1];
            self.base = RowSet::default();
            self.set = RowSet::default();
            if !only {
                self.set.insert(row..row + 1);
            }
        }
    }

    /// Drag over the gutter to `row`.
    pub fn drag_to(&mut self, row: u64) {
        let Some(a) = self.anchor else { return };
        if row == a && !self.moved {
            return;
        }
        self.moved = true;
        self.set = self.base.clone();
        self.set.insert(a.min(row)..a.max(row) + 1);
    }

    /// The header control: all rows, or none when all are selected.
    pub fn toggle_all(&mut self, n: u64) {
        let all = self.set.ranges() == [0..n];
        self.clear();
        if !all {
            self.set.insert(0..n);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn click_drag_and_extend() {
        let mut s = CellSel::default();
        s.set((5, 2));
        assert!(!s.is_multi());
        s.extend((3, 4));
        assert_eq!(
            s.bounds(),
            Some(Block {
                r0: 3,
                r1: 5,
                c0: 2,
                c1: 4
            })
        );
        assert_eq!(s.cell_count(), 9);
        assert_eq!(s.anchor, Some((5, 2)));
        assert_eq!(s.focus(), Some((3, 4)));
        // Extending again moves the far corner, not the anchor.
        s.extend((6, 2));
        assert_eq!(s.cell_count(), 2);
        assert!(s.contains((6, 2)) && !s.contains((3, 4)));
    }

    #[test]
    fn ctrl_toggles_cells_and_ranges() {
        let mut s = CellSel::default();
        s.set((0, 0));
        s.extend((1, 1));
        assert!(s.toggle((10, 3)));
        assert_eq!(s.ranges(), 2);
        assert_eq!(s.cell_count(), 5);
        // Ctrl+drag after the toggle extends the new range only.
        s.extend((11, 3));
        assert_eq!(s.cell_count(), 6);
        // Removing a cell from the middle of a range splits it.
        assert!(!s.toggle((0, 1)));
        assert_eq!(s.cell_count(), 5);
        assert!(!s.contains((0, 1)) && s.contains((0, 0)) && s.contains((1, 1)));
        assert_eq!(s.anchor, Some((0, 1)));
        // Toggling it back merges into one rectangle again.
        assert!(s.toggle((0, 1)));
        s.toggle((20, 0));
        s.toggle((20, 0));
        assert_eq!(s.cell_count(), 6);
        assert_eq!(s.done.len(), 2, "{:?}", s.done);
    }

    #[test]
    fn overlapping_ranges_count_once_and_merge() {
        let mut s = CellSel::default();
        s.set((0, 0));
        s.extend((9, 1));
        s.toggle((5, 1));
        assert!(!s.contains((5, 1)));
        assert_eq!(s.cell_count(), 19);
        s.toggle((5, 1));
        s.extend((12, 2));
        // 19 committed, of which rows 6-9 of column 1 are in the live
        // 8 x 2 range again.
        assert_eq!(s.cell_count(), 19 - 4 + 16);
        s.commit();
        // Disjoint after commit.
        let total: u64 = s.done.iter().map(Block::cells).sum();
        assert_eq!(total, s.cell_count());
        // Adjacent rows with the same columns merge.
        let mut m = CellSel::default();
        m.set((0, 0));
        m.extend((4, 2));
        m.toggle((5, 0));
        m.extend((9, 2));
        m.commit();
        assert_eq!(
            m.done,
            vec![Block {
                r0: 0,
                r1: 9,
                c0: 0,
                c1: 2
            }]
        );
    }

    #[test]
    fn select_all_is_one_block() {
        let mut s = CellSel::default();
        s.select_all(100_000_000, 40);
        assert_eq!(s.ranges(), 1);
        assert_eq!(s.cell_count(), 4_000_000_000);
        assert!(s.contains((99_999_999, 39)));
        s.select_all(0, 3);
        assert_eq!(s.ranges(), 1);
    }

    #[test]
    fn row_set_insert_remove() {
        let mut r = RowSet::default();
        r.insert(10..20);
        r.insert(0..5);
        r.insert(5..10);
        assert_eq!(r.ranges(), [0..20]);
        r.insert(30..40);
        r.insert(15..35);
        assert_eq!(r.ranges(), [0..40]);
        r.remove(5..7);
        assert_eq!(r.ranges(), [0..5, 7..40]);
        assert_eq!(r.len(), 38);
        assert!(r.contains(4) && !r.contains(5) && r.contains(39) && !r.contains(40));
        r.remove(0..100);
        assert!(r.is_empty());
    }

    #[test]
    fn row_gutter_clicks() {
        let mut s = RowSel::default();
        s.press(3, false, false);
        assert_eq!(s.set.ranges(), [3..4]);
        // A second click on the only selected row deselects it.
        s.press(3, false, false);
        assert!(s.set.is_empty());
        s.press(3, false, false);
        s.press(7, true, false);
        assert_eq!(s.set.ranges(), [3..4, 7..8]);
        s.press(10, true, true);
        assert_eq!(s.set.ranges(), [3..4, 7..11]);
        s.press(5, false, true);
        assert_eq!(s.set.ranges(), [5..8]);
        // Drag from 20 to 25, then back to 22.
        s.press(20, false, false);
        s.drag_to(25);
        s.drag_to(22);
        assert_eq!(s.set.ranges(), [20..23]);
        s.toggle_all(1000);
        assert_eq!(s.set.len(), 1000);
        s.toggle_all(1000);
        assert!(s.set.is_empty());
    }
}
