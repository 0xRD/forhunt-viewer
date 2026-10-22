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
    pub tags: Vec<(u32, String)>,
}

#[derive(Serialize, Deserialize)]
pub struct ColFilterSer {
    pub quick: String,
    pub conds: Vec<Op>,
    pub any: bool,
    pub case_sensitive: bool,
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
                        conds: f.conds.clone(),
                        any: f.any,
                        case_sensitive: f.case_sensitive,
                    },
                )
            })
            .collect(),
        search: tab.search.clone(),
        search_filter: tab.search_filter,
        tagged_only: tab.tagged_only,
        tags: tab.tags.iter().map(|(r, n)| (*r, n.clone())).collect(),
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
                conds: f.conds.clone(),
                any: f.any,
                case_sensitive: f.case_sensitive,
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
    tab.requery();
    warn
}
