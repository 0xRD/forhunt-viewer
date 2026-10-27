//! Coloring rules (D-065): a condition in the query language plus a style
//! from the theme's rule palette, for the whole row or the matching cells.
//!
//! Rules are tried in list order; each cell takes the style of the first
//! rule that covers it (a row rule covers every cell of a matching row, a
//! cell rule the cells whose own condition holds), and the row background
//! is the first matching row rule's. Only the rows the grid prepares are
//! evaluated, on the file's cell text (before display-zone conversion, like
//! filters), and the result is kept until those rows or the rules change.

use std::ops::Range;
use std::sync::Arc;
use std::sync::mpsc::Receiver;

use eframe::egui::{self, Color32, RichText, Ui};
use fhv_engine::columns::Kind;
use fhv_engine::filter::{Compiled, Expr, Op, Pred, Target};
use serde::{Deserialize, Serialize};

use crate::grid::CellStyle;
use crate::tab::Tab;
use crate::theme::{Pair, Theme};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    #[default]
    Row,
    Cell,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Rule {
    pub enabled: bool,
    /// Condition, in the query language (D-046). Conditions without `cs` /
    /// `ci` ignore case.
    pub when: String,
    /// Name of a color of the theme's rule palette ([`PALETTE`]).
    pub color: String,
    pub scope: Scope,
    /// Fill the background (the text then takes the palette's text color,
    /// which every theme checks for contrast on it).
    pub fill: bool,
    /// Color the text.
    pub text: bool,
    pub bold: bool,
}

impl Default for Rule {
    fn default() -> Self {
        Rule {
            enabled: true,
            when: String::new(),
            color: "red".into(),
            scope: Scope::Row,
            fill: true,
            text: false,
            bold: false,
        }
    }
}

pub const PALETTE: [&str; 8] = [
    "red", "orange", "yellow", "green", "teal", "blue", "purple", "gray",
];

/// A shared ruleset file.
#[derive(Serialize, Deserialize)]
struct RuleFile {
    forhunt_viewer_rules: u32,
    rules: Vec<Rule>,
}

pub fn export_json(rules: &[Rule]) -> String {
    serde_json::to_string_pretty(&RuleFile {
        forhunt_viewer_rules: 1,
        rules: rules.to_vec(),
    })
    .unwrap_or_default()
}

/// Rules of an exported file (or of a bare JSON array of rules).
pub fn import_json(b: &[u8]) -> Result<Vec<Rule>, String> {
    serde_json::from_slice::<RuleFile>(b)
        .map(|f| f.rules)
        .or_else(|_| serde_json::from_slice::<Vec<Rule>>(b))
        .map_err(|e| format!("not a ruleset: {e}"))
}

fn palette(th: &Theme, name: &str) -> Pair {
    th.rules.get(name).unwrap_or(th.rules.gray)
}

/// One condition compiled for this file's columns.
struct Leaf {
    /// None: any column (`*`), with one compiled predicate per column.
    col: Option<usize>,
    cps: Vec<Compiled>,
    neg: bool,
}

enum Node {
    And(Vec<Node>),
    Or(Vec<Node>),
    Not(Box<Node>),
    Leaf(Leaf),
}

fn cell<'a>(cells: &[&'a str], c: usize) -> &'a [u8] {
    cells.get(c).copied().unwrap_or("").as_bytes()
}

impl Leaf {
    fn matches(&self, cells: &[&str]) -> bool {
        let hit = match self.col {
            Some(c) => self.cps[0].matches(cell(cells, c)),
            None => self
                .cps
                .iter()
                .enumerate()
                .any(|(c, cp)| cp.matches(cell(cells, c))),
        };
        hit != self.neg
    }

    fn mark(&self, cells: &[&str], out: &mut [bool]) {
        match self.col {
            Some(c) if c < out.len() => out[c] |= self.matches(cells),
            Some(_) => {}
            None if self.neg => {
                if self.matches(cells) {
                    out.iter_mut().for_each(|x| *x = true);
                }
            }
            None => {
                for (c, cp) in self.cps.iter().enumerate() {
                    if c < out.len() && cp.matches(cell(cells, c)) {
                        out[c] = true;
                    }
                }
            }
        }
    }
}

impl Node {
    fn matches(&self, cells: &[&str]) -> bool {
        match self {
            Node::And(xs) => xs.iter().all(|x| x.matches(cells)),
            Node::Or(xs) => xs.iter().any(|x| x.matches(cells)),
            Node::Not(x) => !x.matches(cells),
            Node::Leaf(l) => l.matches(cells),
        }
    }

    /// Columns whose own condition holds (cell rules).
    fn mark(&self, cells: &[&str], out: &mut [bool]) {
        match self {
            Node::And(xs) | Node::Or(xs) => xs.iter().for_each(|x| x.mark(cells, out)),
            Node::Not(_) => {}
            Node::Leaf(l) => l.mark(cells, out),
        }
    }
}

fn compile_node(e: &Expr, kind: &dyn Fn(usize) -> Kind, ncols: usize) -> Result<Node, String> {
    Ok(match e {
        Expr::And(xs) => Node::And(
            xs.iter()
                .map(|x| compile_node(x, kind, ncols))
                .collect::<Result<_, _>>()?,
        ),
        Expr::Or(xs) => Node::Or(
            xs.iter()
                .map(|x| compile_node(x, kind, ncols))
                .collect::<Result<_, _>>()?,
        ),
        Expr::Not(x) => Node::Not(Box::new(compile_node(x, kind, ncols)?)),
        Expr::Rows(_) => return Err("row sets are not supported in rules".into()),
        Expr::Pred(Pred {
            target,
            op,
            case_sensitive: cs,
        }) => {
            let (op, neg) = match op {
                Op::Not(x) => ((**x).clone(), true),
                o => (o.clone(), false),
            };
            let err = |e: fhv_engine::Error| e.to_string();
            match target {
                Target::Column(c) => Node::Leaf(Leaf {
                    col: Some(*c),
                    cps: vec![Compiled::new(&op, *cs, kind(*c)).map_err(err)?],
                    neg,
                }),
                // Like the global search: each column with its own type, as
                // text when the operand is not of that type.
                Target::Any => Node::Leaf(Leaf {
                    col: None,
                    cps: (0..ncols)
                        .map(|c| {
                            Compiled::new(&op, *cs, kind(c))
                                .or_else(|_| Compiled::new(&op, *cs, Kind::Text))
                        })
                        .collect::<Result<_, _>>()
                        .map_err(err)?,
                    neg,
                }),
            }
        }
    })
}

/// Rules compiled for one tab (column kinds, display zone).
#[derive(Default)]
pub struct CompiledRules {
    /// (rule index, condition, scope).
    rules: Vec<(usize, Node, Scope)>,
    /// Per rule: why it does not apply to this file.
    pub errors: Vec<Option<String>>,
}

/// The condition of `r` for this tab, time operands in UTC (D-038).
pub fn rule_expr(t: &Tab, when: &str) -> Result<Option<Expr>, String> {
    fhv_engine::query::parse(when, &t.table.columns, false)
        .map(|e| e.map(|e| crate::querybar::to_utc(t, e)))
        .map_err(|e| e.to_string())
}

pub fn compile(rules: &[Rule], t: &Tab) -> CompiledRules {
    let mut out = CompiledRules::default();
    let ncols = t.table.columns.len();
    let kind = |c: usize| t.kind(c);
    for (i, r) in rules.iter().enumerate() {
        let mut err = None;
        if r.enabled {
            match rule_expr(t, &r.when) {
                Ok(Some(e)) => match compile_node(&e, &kind, ncols) {
                    Ok(n) => out.rules.push((i, n, r.scope)),
                    Err(e) => err = Some(e),
                },
                Ok(None) => {}
                Err(e) => err = Some(e),
            }
        }
        out.errors.push(err);
    }
    out
}

const NONE: u16 = u16::MAX;

/// Rule and duplicate results of the rows the grid prepared.
#[derive(Default)]
pub struct Paint {
    first: u64,
    n: usize,
    ncols: usize,
    /// Per row: the row rule (row background).
    row: Vec<u16>,
    /// Per (row, file column): the rule styling the cell.
    cell: Vec<u16>,
    /// Per (row, file column): occurrences of the value (0: not counted).
    dup: Vec<u32>,
}

/// Coloring state of a tab.
#[derive(Default)]
pub struct TabRules {
    pub rules: Arc<Vec<Rule>>,
    compiled: Option<CompiledRules>,
    dirty: bool,
    pub paint: Paint,
    /// Theme of the frame (styles are resolved at paint time).
    pub theme: Option<Arc<Theme>>,
}

impl TabRules {
    /// Returns true when the rules changed.
    pub fn set(&mut self, rules: Arc<Vec<Rule>>) -> bool {
        if Arc::ptr_eq(&self.rules, &rules) {
            return false;
        }
        self.rules = rules;
        self.dirty = true;
        true
    }

    /// Column kinds or display zone changed: compile again.
    pub fn invalidate(&mut self) {
        self.dirty = true;
    }

    pub fn errors(&self) -> &[Option<String>] {
        self.compiled.as_ref().map_or(&[], |c| &c.errors)
    }
}

/// Evaluates rules and duplicate counts for the prepared rows (display
/// positions `rows`, raw file text in `t.rows`).
pub fn paint(t: &mut Tab, rows: Range<u64>) {
    if t.colors.dirty || t.colors.compiled.is_none() {
        let rules = t.colors.rules.clone();
        t.colors.compiled = Some(compile(&rules, t));
        t.colors.dirty = false;
    }
    let ncols = t.table.columns.len();
    let n = (rows.end - rows.start) as usize;
    let Tab {
        colors,
        dupes,
        rows: cells_src,
        ..
    } = t;
    let compiled = colors.compiled.as_ref();
    let counts = dupes.ready();
    let p = &mut colors.paint;
    p.first = rows.start;
    p.ncols = ncols;
    p.row.clear();
    p.cell.clear();
    p.dup.clear();
    let has_rules = compiled.is_some_and(|c| !c.rules.is_empty());
    if !has_rules && counts.is_empty() {
        p.n = 0;
        return;
    }
    p.n = n;
    p.row.resize(n, NONE);
    p.cell.resize(n * ncols, NONE);
    p.dup
        .resize(if counts.is_empty() { 0 } else { n * ncols }, 0);
    let mut hits = vec![false; ncols];
    for (k, pos) in rows.enumerate() {
        let cells: Vec<&str> = (0..ncols).map(|c| cells_src.cell(pos, c)).collect();
        let cell_rule = &mut p.cell[k * ncols..(k + 1) * ncols];
        if let Some(cr) = compiled.filter(|_| has_rules) {
            for (i, node, scope) in &cr.rules {
                if !node.matches(&cells) {
                    continue;
                }
                let i = *i as u16;
                match scope {
                    Scope::Row => {
                        if p.row[k] == NONE {
                            p.row[k] = i;
                        }
                        cell_rule
                            .iter_mut()
                            .filter(|x| **x == NONE)
                            .for_each(|x| *x = i);
                        break;
                    }
                    Scope::Cell => {
                        hits.iter_mut().for_each(|h| *h = false);
                        node.mark(&cells, &mut hits);
                        for (x, h) in cell_rule.iter_mut().zip(&hits) {
                            if *h && *x == NONE {
                                *x = i;
                            }
                        }
                    }
                }
            }
        }
        for (c, cnt, _) in &counts {
            if *c < ncols {
                p.dup[k * ncols + c] = cnt.count(cells[*c]).unwrap_or(0);
            }
        }
    }
}

fn at(p: &Paint, row: u64, c: usize) -> Option<(usize, usize)> {
    let k = row.checked_sub(p.first)? as usize;
    (k < p.n && c < p.ncols).then_some((k, k * p.ncols + c))
}

/// Background of the row from its row rule.
pub fn row_color(t: &Tab, row: u64) -> Option<Color32> {
    let p = &t.colors.paint;
    let k = row.checked_sub(p.first)? as usize;
    let i = *p.row.get(k)?;
    let r = t.colors.rules.get(i as usize)?;
    let th = t.colors.theme.as_ref()?;
    r.fill.then(|| palette(th, &r.color).bg.0)
}

/// Style of file column `c` at display row `row`.
pub fn cell_style(t: &Tab, row: u64, c: usize) -> CellStyle {
    let mut st = CellStyle::default();
    let (p, Some(th)) = (&t.colors.paint, t.colors.theme.as_ref()) else {
        return st;
    };
    let Some((k, ix)) = at(p, row, c) else {
        return st;
    };
    let ri = p.cell[ix];
    if let Some(r) = t.colors.rules.get(ri as usize).filter(|_| ri != NONE) {
        let pair = palette(th, &r.color);
        if r.fill && p.row[k] != ri {
            st.bg = Some(pair.bg.0);
        }
        if r.fill || r.text {
            st.fg = Some(pair.fg.0);
        }
        st.bold = r.bold;
    }
    let n = p.dup.get(ix).copied().unwrap_or(0);
    let mode = t.dupes.mode(c);
    let pair = match n {
        0 => None,
        1 if mode.unique() => Some(th.unique),
        n if n >= 2 && mode.duplicates() => Some(th.duplicate),
        _ => None,
    };
    if let Some(pair) = pair {
        st.marker = Some(pair.fg.0);
        // Coloring rules win (D-067): the marker stays.
        if ri == NONE && p.row[k] == NONE {
            st.bg = Some(pair.bg.0);
            st.fg = Some(pair.fg.0);
        }
    }
    st
}

pub fn cell_tip(t: &Tab, row: u64, c: usize) -> Option<String> {
    let p = &t.colors.paint;
    let (_, ix) = at(p, row, c)?;
    let mut lines = Vec::new();
    let ri = p.cell[ix];
    if ri != NONE
        && let Some(r) = t.colors.rules.get(ri as usize)
    {
        lines.push(format!("rule {}: {}", ri + 1, r.when));
    }
    let n = p.dup.get(ix).copied().unwrap_or(0);
    if n > 0 && t.dupes.mode(c) != crate::dupes::Mode::Off {
        lines.push(format!(
            "{} occurrence{} {}",
            crate::app::fmt_n(n as u64),
            if n == 1 { "" } else { "s" },
            t.dupes.scope_text(c)
        ));
    }
    (!lines.is_empty()).then(|| lines.join("\n"))
}

/// The rule editor window (View > Coloring rules…).
#[derive(Default)]
pub struct Editor {
    /// Rule being written before "Add rule" (pre-filled from a cell).
    pub draft: Rule,
    counting: Option<Receiver<Vec<Option<u64>>>>,
    counts: Option<Vec<Option<u64>>>,
    msg: Option<String>,
}

impl Editor {
    pub fn with_draft(when: String) -> Editor {
        Editor {
            draft: Rule {
                when,
                ..Rule::default()
            },
            ..Default::default()
        }
    }
}

fn swatch(ui: &mut Ui, pair: Pair) {
    let (r, _) = ui.allocate_exact_size(egui::vec2(28.0, 16.0), egui::Sense::hover());
    ui.painter().rect_filled(r, 3.0, pair.bg.0);
    ui.painter().text(
        r.center(),
        egui::Align2::CENTER_CENTER,
        "Aa",
        egui::FontId::proportional(11.0),
        pair.fg.0,
    );
}

/// Style widgets of a rule; true when changed.
fn style_ui(
    ui: &mut Ui,
    r: &mut Rule,
    th: &Theme,
    id: impl std::hash::Hash + std::fmt::Debug,
) -> bool {
    let before = r.clone();
    swatch(ui, palette(th, &r.color));
    egui::ComboBox::from_id_salt(("rule color", &id))
        .selected_text(r.color.clone())
        .width(80.0)
        .show_ui(ui, |ui| {
            for name in PALETTE {
                ui.horizontal(|ui| {
                    swatch(ui, palette(th, name));
                    ui.selectable_value(&mut r.color, name.to_string(), name);
                });
            }
        });
    egui::ComboBox::from_id_salt(("rule scope", &id))
        .selected_text(match r.scope {
            Scope::Row => "whole row",
            Scope::Cell => "matching cells",
        })
        .width(110.0)
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut r.scope, Scope::Row, "whole row");
            ui.selectable_value(&mut r.scope, Scope::Cell, "matching cells");
        });
    ui.checkbox(&mut r.fill, "fill")
        .on_hover_text("background color (text in the matching color)");
    ui.checkbox(&mut r.text, "text");
    ui.checkbox(&mut r.bold, "bold");
    *r != before
}

/// Shows the editor; true when `rules` changed.
pub fn window(
    ctx: &egui::Context,
    open: &mut Option<Editor>,
    rules: &mut Vec<Rule>,
    tab: Option<&Tab>,
) -> bool {
    let Some(ed) = open.as_mut() else {
        return false;
    };
    if let Some(rx) = &ed.counting
        && let Ok(v) = rx.try_recv()
    {
        ed.counts = Some(v);
        ed.counting = None;
    }
    let th = crate::theme::current(ctx);
    let mut changed = false;
    let mut keep = true;
    egui::Window::new("Coloring rules")
        .open(&mut keep)
        .default_width(760.0)
        .show(ctx, |ui| {
            ui.label(
                RichText::new(
                    "Conditions use the query syntax (Info contains Critical, EventId in (4624, 4625), \
                     * contains mimikatz). The first rule covering a cell gives its style.",
                )
                .weak(),
            );
            ui.separator();
            let errors: Vec<Option<String>> = tab.map(|t| t.colors.errors().to_vec()).unwrap_or_default();
            let mut action: Option<(usize, i32)> = None;
            egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
                if rules.is_empty() {
                    ui.label(RichText::new("No rules yet.").italics());
                }
                for (i, r) in rules.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        changed |= ui.checkbox(&mut r.enabled, "").on_hover_text("enabled").changed();
                        ui.label(format!("{}.", i + 1));
                        if ui.small_button("↑").on_hover_text("move up").clicked() {
                            action = Some((i, -1));
                        }
                        if ui.small_button("↓").on_hover_text("move down").clicked() {
                            action = Some((i, 1));
                        }
                        let te = ui.add(
                            egui::TextEdit::singleline(&mut r.when)
                                .desired_width(300.0)
                                .hint_text("condition"),
                        );
                        let label = format!("rule {} condition", i + 1);
                        ui.ctx().accesskit_node_builder(te.id, |n| n.set_label(label));
                        changed |= te.changed();
                        changed |= style_ui(ui, r, &th, i);
                        if let Some(Some(n)) = ed.counts.as_ref().and_then(|c| c.get(i)) {
                            ui.label(format!("{} rows", crate::app::fmt_n(*n)));
                        }
                        if ui.small_button("Delete").clicked() {
                            action = Some((i, 0));
                        }
                    });
                    if let Some(Some(e)) = errors.get(i) {
                        ui.colored_label(th.error.0, format!("    not applied to this file: {e}"));
                    }
                }
            });
            if let Some((i, d)) = action {
                match d {
                    0 => {
                        rules.remove(i);
                    }
                    -1 if i > 0 => rules.swap(i, i - 1),
                    1 if i + 1 < rules.len() => rules.swap(i, i + 1),
                    _ => {}
                }
                ed.counts = None;
                changed = true;
            }
            ui.separator();
            ui.label(RichText::new("New rule").strong());
            ui.horizontal(|ui| {
                let te = ui.add(
                    egui::TextEdit::singleline(&mut ed.draft.when)
                        .desired_width(300.0)
                        .hint_text("e.g. Info contains Critical"),
                );
                ui.ctx()
                    .accesskit_node_builder(te.id, |n| n.set_label("new rule condition"));
                style_ui(ui, &mut ed.draft, &th, "draft");
                let err = tab.and_then(|t| rule_expr(t, &ed.draft.when).err());
                if ui
                    .add_enabled(
                        !ed.draft.when.trim().is_empty() && err.is_none(),
                        egui::Button::new("Add rule"),
                    )
                    .clicked()
                {
                    rules.push(std::mem::take(&mut ed.draft));
                    ed.counts = None;
                    changed = true;
                }
                if let Some(e) = err.filter(|_| !ed.draft.when.trim().is_empty()) {
                    ui.colored_label(th.error.0, e);
                }
            });
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("Import…").on_hover_text("add the rules of a ruleset file").clicked()
                    && let Some(p) = rfd::FileDialog::new().add_filter("rules", &["json"]).pick_file()
                {
                    match std::fs::read(&p).map_err(|e| e.to_string()).and_then(|b| import_json(&b)) {
                        Ok(v) => {
                            ed.msg = Some(format!("imported {} rules", v.len()));
                            rules.extend(v);
                            changed = true;
                        }
                        Err(e) => ed.msg = Some(format!("{}: {e}", p.display())),
                    }
                }
                if ui.button("Export…").clicked()
                    && let Some(p) = rfd::FileDialog::new()
                        .set_file_name("coloring-rules.json")
                        .add_filter("rules", &["json"])
                        .save_file()
                {
                    ed.msg = Some(
                        match crate::settings::write_atomic(&p, export_json(rules).as_bytes()) {
                            Ok(()) => format!("exported to {}", p.display()),
                            Err(e) => format!("{}: {e}", p.display()),
                        },
                    );
                }
                if let Some(t) = tab
                    && ui
                        .add_enabled(ed.counting.is_none() && t.ready(), egui::Button::new("Count rows"))
                        .on_hover_text("rows of the current view matching each condition (background scan)")
                        .clicked()
                {
                    let exprs: Vec<Option<Expr>> = rules
                        .iter()
                        .map(|r| rule_expr(t, &r.when).ok().flatten().filter(|_| r.enabled))
                        .collect();
                    let engine = t.engine.clone();
                    let view = t.view.rows.clone();
                    let (tx, rx) = std::sync::mpsc::channel();
                    ed.counting = Some(rx);
                    let _ = std::thread::Builder::new()
                        .name("fhv-rule-count".into())
                        .spawn(move || {
                            let v = exprs
                                .iter()
                                .map(|e| {
                                    let e = e.as_ref()?;
                                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                        engine.eval(e).ok().map(|b| b.intersection_len(&view))
                                    }))
                                    .ok()
                                    .flatten()
                                })
                                .collect();
                            let _ = tx.send(v);
                        });
                }
                if ed.counting.is_some() {
                    ui.spinner();
                }
                if let Some(m) = &ed.msg {
                    ui.label(m);
                }
            });
        });
    if !keep {
        *open = None;
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules_round_trip_through_json() {
        let rules = vec![
            Rule {
                when: "Info contains Critical".into(),
                ..Rule::default()
            },
            Rule {
                enabled: false,
                when: "* regex 'mimi.*'".into(),
                color: "blue".into(),
                scope: Scope::Cell,
                fill: false,
                text: true,
                bold: true,
            },
        ];
        let s = export_json(&rules);
        assert!(s.contains("\"forhunt_viewer_rules\": 1"));
        assert_eq!(import_json(s.as_bytes()).unwrap(), rules);
        // A bare array, and fields left out take their defaults.
        let bare = import_json(br#"[{"when": "A = 1"}]"#).unwrap();
        assert_eq!(bare[0].color, "red");
        assert!(bare[0].enabled && bare[0].fill);
        assert!(import_json(b"{}").is_err());
    }
}
