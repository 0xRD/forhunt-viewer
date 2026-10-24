//! One open file: table, background jobs, filters, sort, tags, column
//! layout and the grid source adapter.

use std::collections::{BTreeMap, HashMap};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};
use std::time::Instant;

use eframe::egui::Color32;
use fhv_engine::columns::Kind;
use fhv_engine::filter::{Engine, Expr, Op, Pred, Target};
use fhv_engine::sort::SortKey;
use fhv_engine::view::{Query, View};
use fhv_engine::{OpenOptions, Phase, Rows, Table};
use roaring::RoaringBitmap;
use serde::{Deserialize, Serialize};

use crate::grid::{GridSource, GridState};
use crate::tz;

/// Display columns before the file columns.
pub const DC_TAG: usize = 0;
pub const DC_NUM: usize = 1;
pub const DC_FIRST: usize = 2;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ColFilter {
    /// Quick "contains" text from the filter row.
    pub quick: String,
    /// Further conditions on the column.
    pub conds: Vec<Op>,
    /// Conditions combine with OR instead of AND.
    pub any: bool,
    pub case_sensitive: bool,
    /// Value-list selection (`in` / `not in` of dictionary values, or
    /// "= / ≠ this value" from a cell). Matched exactly (case and spaces
    /// count) whatever `case_sensitive` says: the list names values that
    /// differ only by case separately. ANDed with the other conditions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub values: Option<Op>,
}

impl ColFilter {
    pub fn is_active(&self) -> bool {
        !self.quick.is_empty() || !self.conds.is_empty() || self.values.is_some()
    }

    /// Whether the value-list selection keeps value `v`.
    pub fn keeps_value(&self, v: &str) -> bool {
        match &self.values {
            Some(Op::In(vs)) => vs.iter().any(|x| x == v),
            Some(Op::NotIn(vs)) => !vs.iter().any(|x| x == v),
            _ => true,
        }
    }

    /// Value-list selection from the ticked values of the full list (the
    /// shorter of `in` / `not in`; none when everything is ticked).
    pub fn set_values<'a>(&mut self, all: impl IntoIterator<Item = (&'a str, bool)>) {
        let (mut on, mut off) = (Vec::new(), Vec::new());
        for (v, keep) in all {
            if keep { &mut on } else { &mut off }.push(v.to_string());
        }
        self.values = if off.is_empty() {
            None
        } else if on.len() <= off.len() {
            Some(Op::In(on))
        } else {
            Some(Op::NotIn(off))
        };
    }

    /// "= this value" (keep only `v`) or "≠ this value" (drop `v`), on top
    /// of the current value selection.
    pub fn filter_value(&mut self, v: String, exclude: bool) {
        let kept = self.keeps_value(&v);
        self.values = Some(match (exclude, self.values.take()) {
            (false, _) => Op::In(if kept { vec![v] } else { Vec::new() }),
            (true, Some(Op::In(mut vs))) => {
                vs.retain(|x| *x != v);
                Op::In(vs)
            }
            (true, Some(Op::NotIn(mut vs))) => {
                if kept {
                    vs.push(v);
                }
                Op::NotIn(vs)
            }
            (true, _) => Op::NotIn(vec![v]),
        });
    }
}

pub enum JobKind {
    Query,
    CopyColumn,
    Search,
    Export(PathBuf),
}

pub enum JobResult {
    View(View),
    Hits(RoaringBitmap),
    Exported(u64),
    Text(String, u64),
}

pub struct Job {
    pub kind: JobKind,
    pub started: Instant,
    rx: Receiver<Result<JobResult, String>>,
    /// The query that produced this job (to tell stale results).
    pub query: Option<Query>,
}

pub struct Tab {
    pub path: PathBuf,
    /// For a CSV written by forhunt: (tab name, evidence path) (D-033).
    pub origin: Option<(String, String)>,
    pub table: Arc<Table>,
    pub engine: Arc<Engine>,
    loader: Option<Receiver<Result<(), String>>>,
    pub load_error: Option<String>,
    pub filters: Vec<ColFilter>,
    pub search: String,
    /// The global search also filters rows (else it only highlights).
    pub search_filter: bool,
    pub search_hits: Option<Arc<RoaringBitmap>>,
    hits_for: String,
    pub tagged_only: bool,
    pub bad_only: bool,
    pub tags: BTreeMap<u32, String>,
    pub sort: Vec<SortKey>,
    pub view: View,
    view_gen: u64,
    pub job: Option<Job>,
    pending: bool,
    /// Last edit of a filter / the search text (queries wait for a pause).
    changed_at: Instant,
    search_seen: String,
    pub last_ms: Option<f64>,
    pub status: Option<String>,
    /// File columns in display order (hidden ones are absent).
    pub order: Vec<usize>,
    pub widths: HashMap<usize, f32>,
    pub grid: GridState,
    rows: Rows,
    rows_key: Option<(u64, Range<u64>)>,
    tz_name: String,
    last_query: Option<Query>,
    /// Text the app should put on the clipboard (from a background job).
    pub clipboard: Option<String>,
    bad: Option<Arc<RoaringBitmap>>,
    /// Search hits within the current view (recomputed when either changes).
    pub hits_in_view: Option<u64>,
    /// Texts of the # column for the prepared rows.
    nums: Vec<String>,
    nums_first: u64,
}

fn initial_width(name: &str) -> f32 {
    match name.to_ascii_lowercase().as_str() {
        n if n.contains("payload") && !n.contains("data") => 420.0,
        n if n.contains("time")
            || n.contains("date")
            || n.contains("created")
            || n.contains("modified") =>
        {
            200.0
        }
        n if n.contains("id") && n.len() <= 9 => 80.0,
        _ => 150.0,
    }
}

impl Tab {
    pub fn open(path: &Path, cache: &Path) -> anyhow::Result<Tab> {
        let table = Arc::new(Table::open(path, OpenOptions::new(cache))?);
        let (tx, rx) = channel();
        {
            let t = table.clone();
            std::thread::Builder::new()
                .name("fhv-index".into())
                .spawn(move || {
                    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| t.build()));
                    let _ = tx.send(match r {
                        Ok(r) => r.map_err(|e| e.to_string()),
                        Err(_) => Err("internal error while indexing (panic)".into()),
                    });
                })?;
        }
        let ncols = table.columns.len();
        let widths = table
            .columns
            .iter()
            .enumerate()
            .map(|(i, n)| (i, initial_width(n)))
            .collect();
        Ok(Tab {
            path: path.to_path_buf(),
            origin: None,
            engine: Arc::new(Engine::new(table.clone())),
            table,
            loader: Some(rx),
            load_error: None,
            filters: vec![ColFilter::default(); ncols],
            search: String::new(),
            search_filter: false,
            search_hits: None,
            hits_for: String::new(),
            tagged_only: false,
            bad_only: false,
            tags: BTreeMap::new(),
            sort: Vec::new(),
            view: View::default(),
            view_gen: 0,
            job: None,
            pending: false,
            changed_at: Instant::now(),
            search_seen: String::new(),
            last_ms: None,
            status: None,
            order: (0..ncols).collect(),
            widths,
            grid: GridState::default(),
            // The grid shows at most a few hundred characters of a cell.
            rows: {
                let mut r = Rows::default();
                r.max_cell = 4096;
                r
            },
            rows_key: None,
            tz_name: "UTC".into(),
            last_query: None,
            nums: Vec::new(),
            nums_first: 0,
            clipboard: None,
            bad: None,
            hits_in_view: None,
        })
    }

    pub fn title(&self) -> String {
        if let Some((name, _)) = &self.origin {
            return name.clone();
        }
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    /// What the tab shows: the evidence for forhunt tabs, else the file.
    pub fn shown_path(&self) -> String {
        match &self.origin {
            Some((_, ev)) => ev.clone(),
            None => self.path.display().to_string(),
        }
    }

    pub fn loading(&self) -> bool {
        self.loader.is_some()
    }

    /// Ready for filters and sorts (index and column indexes done).
    pub fn ready(&self) -> bool {
        self.loader.is_none() && self.load_error.is_none()
    }

    /// Structural and ragged bad rows (computed once the index is done).
    pub fn bad_rows(&mut self) -> Arc<RoaringBitmap> {
        if let Some(b) = &self.bad {
            return b.clone();
        }
        let b = Arc::new(self.compute_bad_rows());
        if self.ready() {
            self.bad = Some(b.clone());
        }
        b
    }

    fn compute_bad_rows(&self) -> RoaringBitmap {
        let mut b = self.table.structural_bad_rows();
        if let Some(ci) = self.table.column_index() {
            b |= &ci.ragged;
        }
        b
    }

    pub fn tag_set(&self) -> RoaringBitmap {
        self.tags.keys().copied().collect()
    }

    /// The filter expression of the current UI state (None = all rows).
    pub fn expr(&self) -> Option<Expr> {
        let mut all = Vec::new();
        for (c, f) in self.filters.iter().enumerate() {
            let pred = |op: Op| {
                Expr::Pred(Pred {
                    target: Target::Column(c),
                    op,
                    case_sensitive: f.case_sensitive,
                })
            };
            if !f.quick.is_empty() {
                all.push(pred(Op::Contains(f.quick.clone())));
            }
            if !f.conds.is_empty() {
                let xs: Vec<Expr> = f.conds.iter().cloned().map(pred).collect();
                all.push(if f.any { Expr::Or(xs) } else { Expr::And(xs) });
            }
            if let Some(op) = &f.values {
                all.push(Expr::Pred(Pred {
                    target: Target::Column(c),
                    op: op.clone(),
                    case_sensitive: true,
                }));
            }
        }
        if self.search_filter && !self.search.is_empty() {
            all.push(Expr::Pred(Pred {
                target: Target::Any,
                op: Op::Contains(self.search.clone()),
                case_sensitive: false,
            }));
        }
        if self.tagged_only {
            all.push(Expr::Rows(Arc::new(self.tag_set())));
        }
        if self.bad_only {
            // Not cached yet (e.g. "bad rows only" restored by a session
            // before the toolbar asked): compute it, never drop the term.
            all.push(Expr::Rows(match &self.bad {
                Some(b) => b.clone(),
                None => Arc::new(self.compute_bad_rows()),
            }));
        }
        (!all.is_empty()).then_some(Expr::And(all))
    }

    pub fn query(&self) -> Query {
        Query {
            filter: self.expr(),
            sort: self.sort.clone(),
        }
    }

    /// Re-runs the query (after any filter/sort/tag change).
    pub fn requery(&mut self) {
        self.pending = true;
        self.changed_at = Instant::now();
    }

    fn spawn<F>(&mut self, kind: JobKind, query: Option<Query>, f: F)
    where
        F: FnOnce() -> Result<JobResult, String> + Send + 'static,
    {
        self.table.progress.reset_cancel();
        let (tx, rx) = channel();
        let _ = std::thread::Builder::new()
            .name("fhv-job".into())
            .spawn(move || {
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f))
                    .unwrap_or_else(|_| Err("internal error (panic)".into()));
                let _ = tx.send(r);
            });
        self.job = Some(Job {
            kind,
            started: Instant::now(),
            rx,
            query,
        });
    }

    /// Cancels the running job, or the indexing while the file loads.
    pub fn cancel_job(&mut self) {
        if self.job.is_some() || self.loader.is_some() {
            self.table.progress.cancel();
        }
    }

    /// Exports the view (or its tagged rows) in display order, in the
    /// background (row selection included).
    pub fn start_export(
        &mut self,
        path: PathBuf,
        tagged: bool,
        cols: Vec<usize>,
        fmt: fhv_engine::export::Format,
    ) {
        if self.job.is_some() || !self.ready() {
            self.status = Some("busy: wait for the current operation".into());
            return;
        }
        // The rows shown must be those of the filter and sort shown.
        if self.pending || self.last_query.as_ref() != Some(&self.query()) {
            self.status = Some(
                "not exported: the current filter / sort is not applied yet or failed; \
                 wait for it (or fix it), then export"
                    .into(),
            );
            return;
        }
        let t = self.table.clone();
        let p = path.clone();
        let view = self.view.clone();
        let tags = Arc::new(self.tag_set());
        self.spawn(JobKind::Export(path), None, move || {
            let rows: Vec<u32> = if tagged {
                match &view.order {
                    Some(o) => o.iter().copied().filter(|r| tags.contains(*r)).collect(),
                    None => (&*view.rows & &*tags).iter().collect(),
                }
            } else {
                view.iter().collect()
            };
            let f = std::fs::File::create(&p).map_err(|e| format!("{}: {e}", p.display()))?;
            let mut w = std::io::BufWriter::with_capacity(4 << 20, f);
            let n = match fhv_engine::export::export(&t, &rows, &cols, fmt, &mut w) {
                Ok(n) => n,
                Err(e) => {
                    // No half-written export left behind (cancel or error).
                    drop(w);
                    let _ = std::fs::remove_file(&p);
                    return Err(e.to_string());
                }
            };
            Ok(JobResult::Exported(n))
        });
    }

    /// Copies the values of file column `c` in the view (first 1 M rows),
    /// one per line, in the background.
    pub fn start_copy_column(&mut self, c: usize) {
        if self.job.is_some() || !self.ready() {
            self.status = Some("busy: wait for the current operation".into());
            return;
        }
        const MAX: usize = 1_000_000;
        let rows: Vec<u32> = self.view.iter().take(MAX).collect();
        let t = self.table.clone();
        self.spawn(JobKind::CopyColumn, None, move || {
            let mut out = String::new();
            let mut buf = fhv_engine::Rows::default();
            for (k, ch) in rows.chunks(4096).enumerate() {
                if t.progress.cancelled() {
                    return Err("cancelled".into());
                }
                t.read_row_list((k * 4096) as u64, ch, &mut buf)
                    .map_err(|e| e.to_string())?;
                for i in 0..ch.len() {
                    out.push_str(buf.cell((k * 4096 + i) as u64, c));
                    out.push('\n');
                }
            }
            Ok(JobResult::Text(out, rows.len() as u64))
        });
    }

    fn start_search(&mut self) {
        let engine = self.engine.clone();
        let needle = self.search.clone();
        self.hits_for = needle.clone();
        self.spawn(JobKind::Search, None, move || {
            let b = engine
                .eval(&Expr::Pred(Pred {
                    target: Target::Any,
                    op: Op::Contains(needle),
                    case_sensitive: false,
                }))
                .map_err(|e| e.to_string())?;
            Ok(JobResult::Hits(b))
        });
    }

    /// Advances background work; call every frame. Returns true while
    /// something is running (to keep repainting).
    pub fn poll(&mut self, tz_name: &str) -> bool {
        if self.tz_name != tz_name {
            self.tz_name = tz_name.to_string();
            self.rows_key = None;
        }
        if let Some(rx) = &self.loader {
            match rx.try_recv() {
                Ok(r) => {
                    self.loader = None;
                    match r {
                        Ok(()) => {
                            self.pending = true;
                            let n = self.table.row_count();
                            self.tags.retain(|r, _| (*r as u64) < n);
                        }
                        Err(e) => self.load_error = Some(e),
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.loader = None;
                    self.load_error = Some("indexing stopped".into());
                }
                Err(_) => {
                    // While indexing: all rows found so far, in file order.
                    let n = self.table.row_count();
                    if self.view.len() != n {
                        self.view = View::all(n);
                        self.view_gen += 1;
                    }
                }
            }
        }
        if let Some(job) = &self.job {
            match job.rx.try_recv() {
                Ok(r) => {
                    let job = self.job.take();
                    let ms = job
                        .as_ref()
                        .map(|j| j.started.elapsed().as_secs_f64() * 1e3);
                    match r {
                        Ok(JobResult::View(v)) => {
                            let sel_row = self.grid.selected.and_then(|p| self.view.row_at(p));
                            self.view = v;
                            self.update_hits();
                            self.view_gen += 1;
                            self.last_ms = ms;
                            self.last_query = job.and_then(|j| j.query);
                            // Keep the selected row selected if still shown.
                            self.grid.selected = sel_row.and_then(|r| self.view.position_of(r));
                            if let Some(p) = self.grid.selected {
                                crate::grid::scroll_to(&mut self.grid, p);
                            } else {
                                self.grid.top = self.grid.top.min(self.view.len() as f64);
                            }
                        }
                        Ok(JobResult::Hits(b)) => {
                            self.search_hits = Some(Arc::new(b));
                            self.update_hits();
                            self.status = None;
                        }
                        Ok(JobResult::Text(text, n)) => {
                            self.status = Some(format!("copied {n} values"));
                            self.clipboard = Some(text);
                        }
                        Ok(JobResult::Exported(n)) => {
                            self.status = Some(format!(
                                "exported {n} rows in {:.1} s",
                                ms.unwrap_or(0.0) / 1e3
                            ));
                        }
                        // Superseded by newer input: nothing to report.
                        Err(e) if e == "cancelled" => {
                            if !self.pending && self.hits_for == self.search {
                                self.status = Some(
                                    "cancelled: the rows shown are those of the previous filter / sort".into(),
                                );
                            }
                        }
                        Err(e) => self.status = Some(format!("error: {e}")),
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.job = None,
                Err(_) => {}
            }
        }
        // Typing: wait for a pause, and stop work the new input supersedes.
        if self.search != self.search_seen {
            self.search_seen = self.search.clone();
            self.changed_at = Instant::now();
        }
        let settled = self.changed_at.elapsed() >= std::time::Duration::from_millis(250);
        if let Some(job) = &self.job {
            let superseded = match job.kind {
                JobKind::Query => self.pending,
                JobKind::Search => self.hits_for != self.search,
                JobKind::Export(_) | JobKind::CopyColumn => false,
            };
            if superseded && !self.table.progress.cancelled() {
                self.table.progress.cancel();
            }
        }
        if self.ready() && self.job.is_none() && settled {
            if self.pending {
                self.pending = false;
                let q = self.query();
                if self.last_query.as_ref() != Some(&q)
                    || self.view.is_empty() && self.table.row_count() > 0
                {
                    let engine = self.engine.clone();
                    let q2 = q.clone();
                    self.spawn(JobKind::Query, Some(q), move || {
                        fhv_engine::view::run(&engine, &q2)
                            .map(JobResult::View)
                            .map_err(|e| e.to_string())
                    });
                }
            } else if !self.search.is_empty() && self.hits_for != self.search {
                self.start_search();
            } else if self.search.is_empty() && self.search_hits.is_some() {
                self.search_hits = None;
                self.hits_for.clear();
                self.update_hits();
            }
        }
        self.job.is_some() || self.loader.is_some()
    }

    fn update_hits(&mut self) {
        self.hits_in_view = self
            .search_hits
            .as_ref()
            .map(|h| self.view.rows.intersection_len(h));
    }

    /// Progress text of the running work.
    pub fn progress_text(&self) -> Option<(String, f32)> {
        let p = &self.table.progress;
        if self.loading() {
            let what = match p.phase() {
                Phase::Transcoding => "converting to UTF-8",
                Phase::Indexing => "indexing rows",
                Phase::Columns => "indexing columns",
                _ => "opening",
            };
            let frac = if p.phase() == Phase::Columns {
                let n = self.table.row_count().max(1);
                (p.bytes.load(std::sync::atomic::Ordering::Relaxed) as f32 / n as f32).min(1.0)
            } else {
                p.fraction()
            };
            return Some((what.into(), frac));
        }
        self.job.as_ref().map(|j| {
            let what = match &j.kind {
                JobKind::Query => "filtering / sorting".to_string(),
                JobKind::Search => "searching".to_string(),
                JobKind::CopyColumn => "copying column values".to_string(),
                JobKind::Export(p) => format!(
                    "exporting to {} ({} rows)",
                    p.display(),
                    self.table
                        .progress
                        .rows
                        .load(std::sync::atomic::Ordering::Relaxed)
                ),
            };
            (
                format!("{what} ({:.1} s)", j.started.elapsed().as_secs_f64()),
                -1.0,
            )
        })
    }

    /// File column of a display column (None for the tag and # columns).
    pub fn file_col(&self, dc: usize) -> Option<usize> {
        dc.checked_sub(DC_FIRST)
            .and_then(|i| self.order.get(i).copied())
    }

    pub fn display_count(&self) -> usize {
        DC_FIRST + self.order.len()
    }

    /// Syncs grid widths from the per-column widths.
    pub fn sync_widths(&mut self) {
        let mut w = vec![26.0, 70.0];
        w.extend(
            self.order
                .iter()
                .map(|c| *self.widths.get(c).unwrap_or(&150.0)),
        );
        self.grid.widths = w;
    }

    pub fn store_widths(&mut self) {
        for (i, &c) in self.order.iter().enumerate() {
            if let Some(&w) = self.grid.widths.get(DC_FIRST + i) {
                self.widths.insert(c, w);
            }
        }
    }

    pub fn toggle_tag(&mut self, row: u32) {
        if self.tags.remove(&row).is_none() {
            self.tags.insert(row, String::new());
        }
        self.rows_key = None;
        if self.tagged_only {
            self.requery();
        }
    }

    /// Next / previous display position holding a search hit.
    pub fn find_hit(&self, forward: bool) -> Option<u64> {
        let hits = self.search_hits.as_ref()?;
        let n = self.view.len();
        if n == 0 || hits.is_empty() {
            return None;
        }
        let cur = self.grid.selected;
        match &self.view.order {
            None => {
                let shown = &*self.view.rows & &**hits;
                let cur_row = cur.and_then(|p| self.view.row_at(p));
                let r = if forward {
                    let from = cur_row.map_or(0, |r| r + 1);
                    shown.range(from..).next().or_else(|| shown.min())
                } else {
                    let to = cur_row.unwrap_or(u32::MAX);
                    shown.range(..to).next_back().or_else(|| shown.max())
                }?;
                self.view.position_of(r)
            }
            Some(order) => {
                use rayon::prelude::*;
                let len = order.len();
                let hit = |p: &usize| hits.contains(order[*p]);
                // Search after (before) the selection, then wrap around up
                // to and including it; without a selection, the whole view.
                let found = match (cur.map(|p| (p as usize).min(len - 1)), forward) {
                    (None, true) => (0..len).into_par_iter().find_first(hit),
                    (None, false) => (0..len).into_par_iter().find_last(hit),
                    (Some(s), true) => (s + 1..len)
                        .into_par_iter()
                        .find_first(hit)
                        .or_else(|| (0..=s).into_par_iter().find_first(hit)),
                    (Some(s), false) => (0..s)
                        .into_par_iter()
                        .find_last(hit)
                        .or_else(|| (s..len).into_par_iter().find_last(hit)),
                };
                found.map(|p| p as u64)
            }
        }
    }

    /// Adapter for the grid.
    pub fn source<'a>(&'a mut self, tz: &'a tz::Display, dark: bool) -> TabSource<'a> {
        TabSource {
            tab: self,
            tz,
            dark,
        }
    }
}

pub struct TabSource<'a> {
    pub tab: &'a mut Tab,
    tz: &'a tz::Display,
    dark: bool,
}

impl GridSource for TabSource<'_> {
    fn row_count(&self) -> u64 {
        self.tab.view.len()
    }

    fn column_count(&self) -> usize {
        self.tab.display_count()
    }

    fn column_name(&self, col: usize) -> &str {
        match col {
            DC_TAG => "★",
            DC_NUM => "#",
            _ => self
                .tab
                .file_col(col)
                .and_then(|c| self.tab.table.columns.get(c))
                .map_or("", String::as_str),
        }
    }

    fn header_suffix(&self, col: usize) -> String {
        let Some(c) = self.tab.file_col(col) else {
            return String::new();
        };
        let mut s = String::new();
        if let Some(i) = self.tab.sort.iter().position(|k| k.col == c) {
            s.push_str(if self.tab.sort[i].desc {
                " ↓"
            } else {
                " ↑"
            });
            if self.tab.sort.len() > 1 {
                s.push_str(&(i + 1).to_string());
            }
        }
        if self.tab.filters.get(c).is_some_and(ColFilter::is_active) {
            s.push_str(" ≡");
        }
        s
    }

    fn prepare(&mut self, rows: Range<u64>) {
        self.tab.nums_first = rows.start;
        self.tab.nums = rows
            .clone()
            .map(|p| {
                self.tab
                    .view
                    .row_at(p)
                    .map_or(String::new(), |r| (r as u64 + 1).to_string())
            })
            .collect();
        let key = (self.tab.view_gen, rows.clone());
        if self.tab.rows_key.as_ref() == Some(&key) {
            return;
        }
        let tab = &mut *self.tab;
        let ids: Vec<u32> = rows.clone().filter_map(|p| tab.view.row_at(p)).collect();
        let contiguous = ids.windows(2).all(|w| w[1] == w[0] + 1);
        let r = if contiguous
            && !ids.is_empty()
            && ids[0] as u64 == rows.start
            && tab.view.order.is_none()
        {
            tab.table.read_rows(rows.clone(), &mut tab.rows)
        } else {
            tab.table.read_row_list(rows.start, &ids, &mut tab.rows)
        };
        if r.is_err() {
            return;
        }
        // Display time zone for time columns.
        if !self.tz.is_utc()
            && let Some(ci) = tab.table.column_index()
        {
            for (c, col) in ci.cols.iter().enumerate() {
                if let Kind::Time(order) = col.kind {
                    for p in rows.clone() {
                        if let Some(cell) = tab.rows.cell_mut(p, c)
                            && let Some(v) = self.tz.convert(cell, order)
                        {
                            *cell = v;
                        }
                    }
                }
            }
        }
        tab.rows_key = Some(key);
    }

    fn cell(&self, row: u64, col: usize) -> &str {
        match col {
            DC_TAG => {
                let r = self.tab.view.row_at(row);
                if r.is_some_and(|r| self.tab.tags.contains_key(&r)) {
                    "★"
                } else {
                    ""
                }
            }
            DC_NUM => self
                .tab
                .nums
                .get(row.wrapping_sub(self.tab.nums_first) as usize)
                .map_or("", String::as_str),
            _ => match self.tab.file_col(col) {
                Some(c) => self.tab.rows.cell(row, c),
                None => "",
            },
        }
    }

    fn row_color(&self, row: u64) -> Option<Color32> {
        let r = self.tab.view.row_at(row)?;
        if self.tab.tags.contains_key(&r) {
            return Some(if self.dark {
                Color32::from_rgb(70, 60, 20)
            } else {
                Color32::from_rgb(255, 243, 190)
            });
        }
        None
    }
}
