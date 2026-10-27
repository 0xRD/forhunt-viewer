//! Sessions: filters, columns, sort and tags saved as JSON and restored.
//! Columns are referenced by name so a session survives column reordering
//! in a re-exported file; tags by data row number (same file only: the
//! session records the file size to warn on mismatch).

use std::path::PathBuf;

use fhv_engine::filter::Op;
use serde::{Deserialize, Serialize};

use crate::tab::{ColFilter, Tab};

pub const VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
pub struct Session {
    pub version: u32,
    pub file: PathBuf,
    pub file_len: u64,
    /// Visible columns in display order, with widths.
    pub columns: Vec<(String, f32)>,
    pub frozen: usize,
    pub sort: Vec<(String, bool)>,
    pub filters: Vec<(String, ColFilterSer)>,
    pub search: String,
    pub search_filter: bool,
    pub tagged_only: bool,
    /// Absent in sessions of 0.2.1 and older.
    #[serde(default)]
    pub bad_only: bool,
    pub tags: Vec<(u32, String)>,
    /// Day/month order set on date columns by 0.3.x (read only: newer
    /// sessions write `date_formats`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub date_orders: Vec<(String, fhv_engine::time::Order)>,
    /// Date formats set on columns ([`kind_spec`]; absent before 0.4).
    #[serde(default)]
    pub date_formats: Vec<(String, String)>,
    /// Applied query terms the column filters cannot hold, as query text
    /// with explicit cs (absent before 0.4).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    /// Coloring rules when the session was saved (absent before 0.4);
    /// loading the session makes them the app's rules (D-065).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rules: Option<Vec<crate::rules::Rule>>,
    /// Duplicate / unique highlighting by column name (absent before 0.4).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub highlights: Vec<(String, crate::dupes::Mode, crate::dupes::Scope)>,
    /// Group levels by column name, outermost first, with the order of
    /// their groups (absent before 0.4).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub group_by: Vec<(String, fhv_engine::group::GroupSort)>,
    /// Groups start collapsed ("Collapse all").
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub groups_collapsed: bool,
}

/// A column kind set by the analyst as text: a date format spec
/// (`time::Fmt::spec`), or `not-a-date` / `not-a-date:number`.
pub fn kind_spec(k: fhv_engine::columns::Kind) -> String {
    use fhv_engine::columns::Kind;
    match k {
        Kind::Time(f) => f.spec(),
        Kind::Number => "not-a-date:number".into(),
        Kind::Text => "not-a-date".into(),
    }
}

pub fn kind_from_spec(s: &str) -> Option<fhv_engine::columns::Kind> {
    use fhv_engine::columns::Kind;
    match s {
        "not-a-date" => Some(Kind::Text),
        "not-a-date:number" => Some(Kind::Number),
        s => fhv_engine::time::Fmt::from_spec(s).ok().map(Kind::Time),
    }
}

#[derive(Serialize, Deserialize)]
pub struct ColFilterSer {
    pub quick: String,
    /// Absent in sessions of 0.3.1 and older.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub quick_not: bool,
    pub conds: Vec<Op>,
    pub any: bool,
    pub case_sensitive: bool,
    /// Exact value-list selection (absent in sessions of 0.2.1 and older).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub values: Option<Op>,
}

pub fn capture(tab: &Tab) -> Session {
    let name = |c: usize| tab.table.columns[c].clone();
    Session {
        version: VERSION,
        file: tab.path.clone(),
        file_len: tab.table.file_len,
        columns: tab
            .order
            .iter()
            .map(|&c| (name(c), *tab.widths.get(&c).unwrap_or(&150.0)))
            .collect(),
        frozen: tab.grid.frozen,
        sort: tab.sort.iter().map(|k| (name(k.col), k.desc)).collect(),
        filters: tab
            .filters
            .iter()
            .enumerate()
            .filter(|(_, f)| f.is_active())
            .map(|(c, f)| {
                (
                    name(c),
                    ColFilterSer {
                        quick: f.quick.clone(),
                        quick_not: f.quick_not,
                        conds: f.conds.clone(),
                        any: f.any,
                        case_sensitive: f.case_sensitive,
                        values: f.values.clone(),
                    },
                )
            })
            .collect(),
        search: tab.search.clone(),
        search_filter: tab.search_filter,
        tagged_only: tab.tagged_only,
        bad_only: tab.bad_only,
        tags: tab.tags.iter().map(|(r, n)| (*r, n.clone())).collect(),
        date_orders: Vec::new(),
        date_formats: tab.kind_specs(),
        query: (!tab.query.residual_text.is_empty()).then(|| tab.query.residual_text.clone()),
        rules: Some((*tab.colors.rules).clone()),
        highlights: tab
            .dupes
            .cols
            .iter()
            .map(|(&c, x)| (name(c), x.mode, x.scope))
            .collect(),
        group_by: tab
            .groups
            .levels
            .iter()
            .map(|l| (name(l.col), l.sort))
            .collect(),
        groups_collapsed: tab.groups.collapsed,
    }
}

/// Applies a session to an open tab. Returns warnings (unknown columns,
/// different file size).
pub fn apply(tab: &mut Tab, s: &Session) -> Vec<String> {
    let mut warn = Vec::new();
    if s.file_len != tab.table.file_len {
        warn.push(format!(
            "the session was saved for a file of {} bytes, this one has {}: tags may point at other rows",
            s.file_len, tab.table.file_len
        ));
    }
    let find = |n: &str| tab.table.columns.iter().position(|c| c == n);
    let mut order = Vec::new();
    for (n, w) in &s.columns {
        match find(n) {
            Some(c) => {
                order.push(c);
                tab.widths.insert(c, *w);
            }
            None => warn.push(format!("column {n} not in this file")),
        }
    }
    if !order.is_empty() {
        tab.order = order;
    }
    tab.grid.frozen = s.frozen.min(tab.display_count());
    tab.sort = s
        .sort
        .iter()
        .filter_map(|(n, d)| find(n).map(|col| fhv_engine::SortKey { col, desc: *d }))
        .collect();
    for f in tab.filters.iter_mut() {
        *f = ColFilter::default();
    }
    for (n, f) in &s.filters {
        if let Some(c) = find(n) {
            tab.filters[c] = ColFilter {
                quick: f.quick.clone(),
                quick_not: f.quick_not,
                conds: f.conds.clone(),
                any: f.any,
                case_sensitive: f.case_sensitive,
                values: f.values.clone(),
            };
        }
    }
    let n = tab.table.row_count() as u32;
    tab.tags = s
        .tags
        .iter()
        .filter(|(r, _)| *r < n || !tab.ready())
        .cloned()
        .collect();
    tab.search = s.search.clone();
    tab.search_filter = s.search_filter;
    tab.tagged_only = s.tagged_only;
    tab.bad_only = s.bad_only;
    for c in 0..tab.table.columns.len() {
        if tab.engine.kind_override(c).is_some() {
            tab.set_kind(c, None);
        }
    }
    for n in tab.apply_kind_specs(&s.date_formats) {
        warn.push(format!("column {n} not in this file (or its date format)"));
    }
    for (n, o) in &s.date_orders {
        match tab.table.columns.iter().position(|c| c == n) {
            Some(c) => tab.set_date_order(c, Some(*o)),
            None => warn.push(format!("column {n} not in this file")),
        }
    }
    let residual = match &s.query {
        None => None,
        Some(q) => match fhv_engine::query::parse(q, &tab.table.columns, false) {
            Ok(e) => e,
            Err(e) => {
                warn.push(format!("query {q:?} not restored: {e}"));
                None
            }
        },
    };
    crate::querybar::set_residual(tab, residual);
    tab.dupes = Default::default();
    for (n, m, sc) in &s.highlights {
        match tab.table.columns.iter().position(|c| c == n) {
            Some(c) => tab.dupes.set(c, *m, *sc),
            None => warn.push(format!("column {n} not in this file")),
        }
    }
    tab.groups.levels.clear();
    for (n, sort) in &s.group_by {
        match tab.table.columns.iter().position(|c| c == n) {
            Some(col) if tab.groups.level_of(col).is_none() => tab
                .groups
                .levels
                .push(fhv_engine::group::Level { col, sort: *sort }),
            Some(_) => {}
            None => warn.push(format!("column {n} not in this file (grouping)")),
        }
    }
    tab.groups.collapsed = s.groups_collapsed;
    tab.groups.flipped.clear();
    tab.repaint_cells();
    tab.requery();
    warn
}
