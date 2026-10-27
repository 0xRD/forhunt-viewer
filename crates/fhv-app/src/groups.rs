//! Grouping in the grid (D-075..D-078): the levels chosen, the group tree
//! of the view (computed by `fhv_engine::group` with the query), which
//! groups are collapsed, and the grid lines: group header rows
//! interleaved with the data rows of open groups.
//!
//! A grid line is a header (a node of the tree) or a data row (a display
//! position of the grouped view, whose rows are in group order). The
//! mapping is a list of segments, one per visible header and per open
//! leaf group, searched by bisection, so a grouping of 100 M rows costs
//! memory per group, never per row. Selection, copy, tagging and export
//! of a selection take the data lines only.

use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use eframe::egui::{RichText, Ui};
use fhv_engine::filter::{Expr, Op, Pred, Target};
use fhv_engine::group::{GroupSort, Groups, Level};
use fhv_engine::view::{Query, View};

use crate::tab::Tab;

/// The grouping of a tab.
#[derive(Default)]
pub struct GroupState {
    /// Levels chosen (first = outermost).
    pub levels: Vec<Level>,
    /// Levels of the view shown (differs while a query runs, or after a
    /// cancelled / refused grouping).
    pub applied: Vec<Level>,
    pub tree: Option<Arc<Groups>>,
    /// Path hash of every node of `tree` (collapsed state survives
    /// re-computation).
    hashes: Vec<u64>,
    pub lines: Option<Arc<Lines>>,
    /// Groups start collapsed (Collapse all) instead of open.
    pub collapsed: bool,
    /// Groups whose state differs from `collapsed`, by path hash.
    pub flipped: HashSet<u64>,
    /// Why the levels are not applied (too many groups...).
    pub warning: Option<String>,
    /// Rows processed by the running grouping (progress).
    pub done: Arc<AtomicU64>,
    /// The last ungrouped view and its query: changing only the grouping
    /// does not filter and sort again.
    pub base: Option<(Query, View)>,
}

/// What a grouping job returns with the view.
pub struct Outcome {
    pub base: View,
    pub result: Result<(View, Arc<Groups>), String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Line {
    /// Group header: node of the tree.
    Header(usize),
    /// Data row at a display position of the view.
    Data(u64),
}

#[derive(Clone, Copy)]
enum Seg {
    Header(usize),
    Data(u64),
}

/// Grid lines of a grouped view.
#[derive(Default)]
pub struct Lines {
    /// (first line, segment), by line.
    segs: Vec<(u64, Seg)>,
    /// Open leaf groups: (first position, first line, rows), by position.
    data: Vec<(u64, u64, u64)>,
    total: u64,
}

impl Lines {
    fn build(g: &Groups, hashes: &[u64], collapsed: bool, flipped: &HashSet<u64>) -> Lines {
        let mut l = Lines::default();
        let mut n = 0;
        while n < g.nodes.len() {
            let nd = g.nodes[n];
            l.segs.push((l.total, Seg::Header(n)));
            l.total += 1;
            if collapsed != flipped.contains(&hashes[n]) {
                n = nd.end as usize;
                continue;
            }
            if g.is_leaf(n) {
                l.segs.push((l.total, Seg::Data(nd.start)));
                l.data.push((nd.start, l.total, nd.count));
                l.total += nd.count;
            }
            n += 1;
        }
        l
    }

    pub fn len(&self) -> u64 {
        self.total
    }

    pub fn at(&self, line: u64) -> Option<Line> {
        if line >= self.total {
            return None;
        }
        let i = self.segs.partition_point(|s| s.0 <= line).checked_sub(1)?;
        Some(match self.segs[i] {
            (_, Seg::Header(n)) => Line::Header(n),
            (l0, Seg::Data(p)) => Line::Data(p + line - l0),
        })
    }

    /// Line of display position `pos`, unless its group is collapsed.
    pub fn line_of_pos(&self, pos: u64) -> Option<u64> {
        let i = self.data.partition_point(|d| d.0 <= pos).checked_sub(1)?;
        let (p0, l0, n) = self.data[i];
        (pos < p0 + n).then_some(l0 + pos - p0)
    }

    pub fn line_of_node(&self, node: usize) -> Option<u64> {
        self.segs.iter().find_map(|s| match s.1 {
            Seg::Header(n) if n == node => Some(s.0),
            _ => None,
        })
    }
}

fn hashes(g: &Groups) -> Vec<u64> {
    let mut out: Vec<u64> = Vec::with_capacity(g.nodes.len());
    for (n, nd) in g.nodes.iter().enumerate() {
        let mut h = std::hash::DefaultHasher::new();
        let parent = out.get(nd.parent as usize).copied().unwrap_or(0);
        (
            parent,
            nd.depth,
            g.levels[nd.depth as usize].col,
            g.value(n),
        )
            .hash(&mut h);
        out.push(h.finish());
    }
    out
}

impl GroupState {
    /// Sets the tree of the view shown (None: not grouped).
    pub fn set_tree(&mut self, tree: Option<Arc<Groups>>) {
        self.hashes = tree.as_deref().map(hashes).unwrap_or_default();
        self.tree = tree;
        self.rebuild();
    }

    /// Recomputes the lines after a tree or collapse change.
    pub fn rebuild(&mut self) {
        self.lines = self
            .tree
            .as_deref()
            .map(|g| Arc::new(Lines::build(g, &self.hashes, self.collapsed, &self.flipped)));
    }

    pub fn is_open(&self, node: usize) -> bool {
        self.hashes
            .get(node)
            .is_some_and(|h| self.collapsed == self.flipped.contains(h))
    }

    /// Opens / closes a group (None: toggles). Returns whether it changed.
    pub fn set_open(&mut self, node: usize, open: Option<bool>) -> bool {
        let Some(&h) = self.hashes.get(node) else {
            return false;
        };
        let open = open.unwrap_or(!self.is_open(node));
        if open == self.is_open(node) {
            return false;
        }
        if !self.flipped.remove(&h) {
            self.flipped.insert(h);
        }
        true
    }

    pub fn set_all(&mut self, open: bool) {
        self.collapsed = !open;
        self.flipped.clear();
        self.rebuild();
    }

    /// Opens the groups holding display position `pos`. Returns whether
    /// one was closed.
    pub fn reveal(&mut self, pos: u64) -> bool {
        let Some(g) = self.tree.clone() else {
            return false;
        };
        // The leaf holding `pos`, then its ancestors.
        let leaf = (0..g.nodes.len())
            .filter(|&n| g.is_leaf(n))
            .find(|&n| (g.nodes[n].start..g.nodes[n].start + g.nodes[n].count).contains(&pos));
        let mut changed = false;
        if let Some(l) = leaf {
            for n in g.path(l) {
                changed |= self.set_open(n, Some(true));
            }
        }
        if changed {
            self.rebuild();
        }
        changed
    }

    /// Header label: `Column: value (count rows)`.
    pub fn label(&self, t: &Tab, node: usize) -> String {
        let Some(g) = &self.tree else {
            return String::new();
        };
        let nd = g.nodes[node];
        let col = g.levels[nd.depth as usize].col;
        format!(
            "{}: {}   ({} row{})",
            t.table.columns.get(col).map_or("", String::as_str),
            g.value(node).unwrap_or("(blank)"),
            crate::app::fmt_n(nd.count),
            if nd.count == 1 { "" } else { "s" }
        )
    }

    pub fn level_of(&self, c: usize) -> Option<usize> {
        self.levels.iter().position(|l| l.col == c)
    }
}

/// Changes to the grouping from the header menu, the group bar or a
/// group header's menu.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    Add(usize),
    Remove(usize),
    Clear,
    Sort(usize, GroupSort),
    /// Moves level i one step left (outer) / right (inner).
    Move(usize, bool),
    All(bool),
    /// Filter to (false) / exclude (true) the group of a node.
    Filter(usize, bool),
    Retry,
}

pub const SORTS: [(GroupSort, &str); 4] = [
    (GroupSort::ValueAsc, "by value, ascending"),
    (GroupSort::ValueDesc, "by value, descending"),
    (GroupSort::CountDesc, "by count, largest first"),
    (GroupSort::CountAsc, "by count, smallest first"),
];

fn sort_mark(s: GroupSort) -> &'static str {
    match s {
        GroupSort::ValueAsc => "A→Z",
        GroupSort::ValueDesc => "Z→A",
        GroupSort::CountDesc => "count ↓",
        GroupSort::CountAsc => "count ↑",
    }
}

/// Entries of a column header's menu.
pub fn header_menu(ui: &mut Ui, levels: &[Level], c: usize) -> Option<Action> {
    let mut out = None;
    ui.separator();
    match levels.iter().position(|l| l.col == c) {
        None => {
            let text = if levels.is_empty() {
                "Group by this column"
            } else {
                "Group by this column (inner level)"
            };
            if ui.button(text).clicked() {
                out = Some(Action::Add(c));
                ui.close();
            }
        }
        Some(i) => {
            if ui.button("Remove grouping").clicked() {
                out = Some(Action::Remove(i));
                ui.close();
            }
        }
    }
    if !levels.is_empty() && ui.button("Clear all grouping").clicked() {
        out = Some(Action::Clear);
        ui.close();
    }
    out
}

/// Context menu of a group header row.
pub fn row_menu(ui: &mut Ui, node: usize) -> Option<Action> {
    let mut out = None;
    for (label, a, tip) in [
        (
            "Filter to this group",
            Action::Filter(node, false),
            "keep only the rows of this group (filter chips, exact values)",
        ),
        (
            "Exclude this group",
            Action::Filter(node, true),
            "hide the rows of this group",
        ),
        ("Expand all groups", Action::All(true), ""),
        ("Collapse all groups", Action::All(false), ""),
    ] {
        let b = ui.button(label);
        let b = if tip.is_empty() {
            b
        } else {
            b.on_hover_text(tip)
        };
        if b.clicked() {
            out = Some(a);
            ui.close();
        }
    }
    out
}

/// The group bar above the grid (only while levels are chosen).
pub fn bar(ui: &mut Ui, t: &mut Tab) {
    if t.groups.levels.is_empty() {
        return;
    }
    let mut act = None;
    ui.horizontal_wrapped(|ui| {
        ui.label("Grouped by");
        let n = t.groups.levels.len();
        for (i, lv) in t.groups.levels.iter().enumerate() {
            if i > 0 {
                ui.label(RichText::new("›").weak());
            }
            let name = t.table.columns.get(lv.col).map_or("?", String::as_str);
            let r = ui
                .menu_button(format!("{name}  {}", sort_mark(lv.sort)), |ui| {
                    ui.label(RichText::new("Sort groups").strong());
                    for (s, label) in SORTS {
                        if ui.radio(lv.sort == s, label).clicked() {
                            act = Some(Action::Sort(i, s));
                            ui.close();
                        }
                    }
                    ui.separator();
                    if i > 0 && ui.button("Move out (outer level)").clicked() {
                        act = Some(Action::Move(i, true));
                        ui.close();
                    }
                    if i + 1 < n && ui.button("Move in (inner level)").clicked() {
                        act = Some(Action::Move(i, false));
                        ui.close();
                    }
                    if ui.button("Remove grouping").clicked() {
                        act = Some(Action::Remove(i));
                        ui.close();
                    }
                })
                .response;
            r.on_hover_text("sort the groups of this level, move or remove it");
            if ui
                .small_button("x")
                .on_hover_text(format!("stop grouping by {name}"))
                .clicked()
            {
                act = Some(Action::Remove(i));
            }
        }
        ui.separator();
        if ui.button("Expand all").clicked() {
            act = Some(Action::All(true));
        }
        if ui.button("Collapse all").clicked() {
            act = Some(Action::All(false));
        }
        if ui.button("Clear grouping").clicked() {
            act = Some(Action::Clear);
        }
        if let Some(g) = &t.groups.tree {
            let top = g.nodes.iter().filter(|n| n.depth == 0).count();
            ui.label(
                RichText::new(format!(
                    "{} groups, {} in all",
                    crate::app::fmt_n(top as u64),
                    crate::app::fmt_n(g.nodes.len() as u64)
                ))
                .weak(),
            );
        }
        let pending = t.groups.levels != t.groups.applied;
        if let Some(w) = &t.groups.warning {
            ui.label(RichText::new(w).color(ui.visuals().warn_fg_color));
        } else if pending && t.job.is_none() {
            ui.label(
                RichText::new("grouping not applied (cancelled?)")
                    .color(ui.visuals().warn_fg_color),
            );
            if ui.button("Group again").clicked() {
                act = Some(Action::Retry);
            }
        }
    });
    if let Some(a) = act {
        apply(t, a);
    }
}

/// Applies a grouping action.
pub fn apply(t: &mut Tab, a: Action) {
    let g = &mut t.groups;
    let regroup = match a {
        Action::Add(c) => {
            if g.level_of(c).is_none() {
                g.levels.push(Level {
                    col: c,
                    sort: GroupSort::default(),
                });
            }
            true
        }
        Action::Remove(i) => {
            if i < g.levels.len() {
                g.levels.remove(i);
            }
            true
        }
        Action::Clear => {
            g.levels.clear();
            true
        }
        Action::Sort(i, s) => {
            if let Some(l) = g.levels.get_mut(i) {
                l.sort = s;
            }
            true
        }
        Action::Move(i, out) => {
            let j = if out { i.wrapping_sub(1) } else { i + 1 };
            if i < g.levels.len() && j < g.levels.len() {
                g.levels.swap(i, j);
            }
            true
        }
        Action::Retry => true,
        Action::All(open) => {
            g.set_all(open);
            t.lines_changed();
            false
        }
        Action::Filter(node, exclude) => {
            filter_group(t, node, exclude);
            false
        }
    };
    if regroup {
        t.groups.warning = None;
        t.requery();
    }
}

/// "Filter to this group" (chips with the exact value of every level) or
/// "Exclude this group" (a chip for an outermost group, else a NOT query
/// term over the whole path).
fn filter_group(t: &mut Tab, node: usize, exclude: bool) {
    let Some(g) = t.groups.tree.clone() else {
        return;
    };
    if node >= g.nodes.len() {
        return;
    }
    let path: Vec<(usize, Option<String>)> = g
        .path(node)
        .into_iter()
        .map(|n| {
            (
                g.levels[g.nodes[n].depth as usize].col,
                g.value(n).map(str::to_string),
            )
        })
        .collect();
    if !exclude || path.len() == 1 {
        for (c, v) in path {
            let f = &mut t.filters[c];
            match v {
                Some(v) => f.filter_value(v, exclude),
                None => {
                    let op = if exclude { Op::IsNotBlank } else { Op::IsBlank };
                    if f.any && !f.conds.is_empty() {
                        // "Match any" conditions: an extra AND term.
                        and_residual(
                            t,
                            Expr::Pred(Pred {
                                target: Target::Column(c),
                                op,
                                case_sensitive: true,
                            }),
                        );
                    } else if !f.conds.contains(&op) {
                        f.conds.push(op);
                    }
                }
            }
        }
    } else {
        let terms: Vec<Expr> = path
            .into_iter()
            .map(|(c, v)| {
                Expr::Pred(Pred {
                    target: Target::Column(c),
                    op: v.map_or(Op::IsBlank, |v| Op::In(vec![v])),
                    case_sensitive: true,
                })
            })
            .collect();
        and_residual(t, Expr::Not(Box::new(Expr::And(terms))));
    }
    t.requery();
}

/// ANDs `e` to the query terms no column chip holds.
fn and_residual(t: &mut Tab, e: Expr) {
    let r = match t.query.residual.take() {
        None => e,
        Some(Expr::And(mut xs)) => {
            xs.push(e);
            Expr::And(xs)
        }
        Some(x) => Expr::And(vec![x, e]),
    };
    crate::querybar::set_residual(t, Some(r));
}

/// Display position of grid line `line` (ungrouped: the line itself).
pub fn pos_at(lines: Option<&Lines>, line: u64) -> Option<u64> {
    match lines {
        None => Some(line),
        Some(l) => match l.at(line)? {
            Line::Data(p) => Some(p),
            Line::Header(_) => None,
        },
    }
}
