//! One open file: table, background jobs, filters, sort, tags, column
//! layout and the grid source adapter.

use std::collections::{BTreeMap, HashMap};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, OnceLock};
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
use crate::settings::RowNumMode;
use crate::tz;

/// Display columns before the file columns.
pub const DC_TAG: usize = 0;
pub const DC_NUM: usize = 1;
pub const DC_FIRST: usize = 2;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ColFilter {
    /// Quick "contains" text from the filter row.
    pub quick: String,
    /// The quick text is inverted (does not contain).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub quick_not: bool,
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

    /// Inverts one chip: the quick text (`None`), the value selection
    /// (`Some(usize::MAX)`) or condition `i`. Natural opposites swap
    /// (`contains` / `not contains`...), other operators get a NOT.
    pub fn invert(&mut self, i: Option<usize>) {
        match i {
            None => self.quick_not = !self.quick_not,
            Some(usize::MAX) => self.values = self.values.take().map(|o| o.negated()),
            Some(i) => {
                if let Some(o) = self.conds.get_mut(i) {
                    *o = o.negated();
                }
            }
        }
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
    CopyCells,
    Search,
    Export(PathBuf),
}

pub enum JobResult {
    /// The view, and the (blank, unparsable) cell counts of the columns
    /// with an active range condition (rows those conditions exclude).
    View(View, Vec<(usize, (u64, u64))>),
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

/// (data row, number column) and the number shown.
type NumCache = ((u32, Option<usize>), String);

pub struct Tab {
    pub path: PathBuf,
    /// For a CSV written by forhunt: (tab name, evidence path) (D-033).
    pub origin: Option<(String, String)>,
    pub table: Arc<Table>,
    pub engine: Arc<Engine>,
    /// Column kinds were set by the analyst since the app last saved them
    /// in the per-file settings cache.
    pub formats_changed: bool,
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
    /// Hidden column -> the visible column it followed (D-070).
    pub hidden_after: HashMap<usize, Option<usize>>,
    pub grid: GridState,
    pub(crate) rows: Rows,
    rows_key: Option<(u64, Range<u64>)>,
    /// Display zone: time-column range operands are read in it (D-038).
    tz: tz::Display,
    last_query: Option<Query>,
    /// Text the app should put on the clipboard (from a background job).
    pub clipboard: Option<String>,
    bad: Option<Arc<RoaringBitmap>>,
    /// Search hits within the current view (recomputed when either changes).
    pub hits_in_view: Option<u64>,
    /// Per column with an active range condition: (blank, unparsable)
    /// cells, which that condition excludes (shown on its chip).
    pub excluded: HashMap<usize, (u64, u64)>,
    /// Last `row_number` answer ((row, column), number): the status bar and
    /// detail pane ask every frame, a file column costs a row read.
    num_cache: std::cell::RefCell<Option<NumCache>>,
    /// Texts of the # column for the prepared rows.
    nums: Vec<String>,
    nums_first: u64,
    /// The file's own row-number column, once detected after indexing
    /// (D-042).
    detected: Arc<OnceLock<Option<usize>>>,
    pub rownum_mode: RowNumMode,
    /// Query bar text, and the part of the last applied query that the
    /// column filters cannot hold (D-045).
    pub query: crate::querybar::QueryState,
    /// Coloring rules and their results for the prepared rows (D-065).
    pub colors: crate::rules::TabRules,
    /// Duplicate / unique highlighting per column (D-066).
    pub dupes: crate::dupes::Dupes,
    /// Status-bar count / sum of the cell selection it was computed for.
    pub sel_stats: Option<(crate::selection::CellSel, Option<(u64, f64)>)>,
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
        Self::open_with(path, cache, None)
    }

    /// Opens with a given dialect (header on/off, delimiter) instead of the
    /// detected one.
    pub fn open_with(
        path: &Path,
        cache: &Path,
        dialect: Option<fhv_engine::Dialect>,
    ) -> anyhow::Result<Tab> {
        let mut opts = OpenOptions::new(cache);
        opts.dialect = dialect;
        let table = Arc::new(Table::open(path, opts)?);
        let (tx, rx) = channel();
        let detected = Arc::new(OnceLock::new());
        {
            let t = table.clone();
            let det = detected.clone();
            std::thread::Builder::new()
                .name("fhv-index".into())
                .spawn(move || {
                    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        t.build()?;
                        let _ = det.set(fhv_engine::rownum::detect_table(&t));
                        Ok::<(), fhv_engine::Error>(())
                    }));
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
            search_filter: true,
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
            hidden_after: HashMap::new(),
            grid: GridState::default(),
            // The grid shows at most a few hundred characters of a cell.
            rows: {
                let mut r = Rows::default();
                r.max_cell = 4096;
                r
            },
            rows_key: None,
            tz: tz::Display::new("UTC"),
            last_query: None,
            formats_changed: false,
            nums: Vec::new(),
            nums_first: 0,
            detected,
            rownum_mode: RowNumMode::Auto,
            query: Default::default(),
            colors: Default::default(),
            dupes: Default::default(),
            clipboard: None,
            sel_stats: None,
            bad: None,
            hits_in_view: None,
            excluded: HashMap::new(),
            num_cache: Default::default(),
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

    /// Kind of file column `c` as filters, sorts and display use it.
    pub fn kind(&self, c: usize) -> Kind {
        self.engine.kind(c)
    }

    /// Kind of column `c` as profiled (before any override).
    pub fn detected_kind(&self, c: usize) -> Kind {
        self.table
            .column_index()
            .and_then(|ci| ci.cols.get(c).map(|x| x.kind))
            .unwrap_or(Kind::Text)
    }

    /// Sets the day/month order of the dates of column `c` (None: as
    /// detected). Filters, sorts and display follow it.
    pub fn set_date_order(&mut self, c: usize, order: Option<fhv_engine::time::Order>) {
        let base = match self.detected_kind(c) {
            Kind::Time(f) if f.shape == fhv_engine::time::Shape::Text => f,
            _ => fhv_engine::time::Fmt::default(),
        };
        self.set_kind(
            c,
            order.map(|order| Kind::Time(fhv_engine::time::Fmt { order, ..base })),
        );
    }

    /// Sets how column `c` is read (a date format, or not a date; None: as
    /// detected). Filters, sorts and display follow it.
    pub fn set_kind(&mut self, c: usize, kind: Option<Kind>) {
        self.engine.set_kind(c, kind);
        self.rows_key = None;
        self.colors.invalidate();
        self.formats_changed = true;
        // Same query, different meaning: run it again.
        self.last_query = None;
        self.requery();
    }

    /// Analyst-set kinds by column name, as spec strings (sessions and the
    /// per-file settings cache).
    pub fn kind_specs(&self) -> Vec<(String, String)> {
        let mut v: Vec<(String, String)> = self
            .engine
            .kind_overrides()
            .into_iter()
            .map(|(c, k)| (self.table.columns[c].clone(), crate::session::kind_spec(k)))
            .collect();
        v.sort();
        v
    }

    /// Applies kinds saved by name; returns the names not in this file or
    /// with a spec that no longer reads.
    pub fn apply_kind_specs(&mut self, specs: &[(String, String)]) -> Vec<String> {
        let mut bad = Vec::new();
        for (n, spec) in specs {
            match (
                self.table.columns.iter().position(|c| c == n),
                crate::session::kind_from_spec(spec),
            ) {
                (Some(c), Some(k)) => self.set_kind(c, Some(k)),
                _ => bad.push(n.clone()),
            }
        }
        bad
    }

    /// Time / number columns with a range condition.
    fn range_columns(&self) -> Vec<usize> {
        (0..self.filters.len())
            .filter(|&c| {
                !matches!(self.kind(c), Kind::Text)
                    && self.filters[c].conds.iter().any(|o| {
                        let o = match o {
                            Op::Not(x) => &**x,
                            o => o,
                        };
                        matches!(
                            o,
                            Op::Gt(_) | Op::Ge(_) | Op::Lt(_) | Op::Le(_) | Op::Between(..)
                        )
                    })
            })
            .collect()
    }

    pub fn tag_set(&self) -> RoaringBitmap {
        self.tags.keys().copied().collect()
    }

    /// The filter expression of the current UI state (None = all rows).
    pub fn expr(&self) -> Option<Expr> {
        let mut all = self.column_terms(true);
        if let Some(q) = &self.query.residual {
            all.push(crate::querybar::to_utc(self, q.clone()));
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

    /// The column filters as ANDed terms, time operands rewritten to UTC
    /// when `utc` (D-038), else as the analyst wrote them.
    pub fn column_terms(&self, utc: bool) -> Vec<Expr> {
        let mut all = Vec::new();
        for (c, f) in self.filters.iter().enumerate() {
            let time_order = match self.kind(c) {
                Kind::Time(o) => Some(o),
                _ => None,
            };
            let pred = |op: Op| {
                Expr::Pred(Pred {
                    target: Target::Column(c),
                    op: match time_order {
                        Some(o) if utc => self.tz.op_to_utc(&op, o),
                        _ => op,
                    },
                    case_sensitive: f.case_sensitive,
                })
            };
            if !f.quick.is_empty() {
                let q = Op::Contains(f.quick.clone());
                all.push(pred(if f.quick_not { q.negated() } else { q }));
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
        all
    }

    #[cfg(test)]
    pub fn view_query_for_test(&self) -> Option<&Query> {
        self.last_query.as_ref()
    }

    /// Display zone name, and whether it differs from UTC.
    pub fn tz_display(&self) -> &tz::Display {
        &self.tz
    }

    pub fn query(&self) -> Query {
        Query {
            filter: self.expr(),
            sort: self.sort.clone(),
        }
    }

    /// Re-runs the query (after any filter/sort/tag change).
    pub fn requery(&mut self) {
        self.query.stale = true;
        self.pending = true;
        self.changed_at = Instant::now();
    }

    pub(crate) fn spawn<F>(&mut self, kind: JobKind, query: Option<Query>, f: F)
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
        let tags = Arc::new(self.tag_set());
        self.export_rows(path, cols, fmt, move |view| {
            if tagged {
                match &view.order {
                    Some(o) => o.iter().copied().filter(|r| tags.contains(*r)).collect(),
                    None => (&*view.rows & &*tags).iter().collect(),
                }
            } else {
                view.iter().collect()
            }
        });
    }

    /// Exports the rows at the given display positions (the selection).
    pub fn start_export_positions(
        &mut self,
        path: PathBuf,
        positions: Vec<Range<u64>>,
        cols: Vec<usize>,
        fmt: fhv_engine::export::Format,
    ) {
        self.export_rows(path, cols, fmt, move |view| {
            positions
                .iter()
                .flat_map(|r| r.clone())
                .map_while(|p| view.row_at(p))
                .collect()
        });
    }

    fn export_rows(
        &mut self,
        path: PathBuf,
        cols: Vec<usize>,
        fmt: fhv_engine::export::Format,
        pick: impl FnOnce(&View) -> Vec<u32> + Send + 'static,
    ) {
        if self.job.is_some() || !self.ready() {
            self.status = Some("busy: wait for the current operation".into());
            return;
        }
        if !self.view_is_current("not exported") {
            return;
        }
        let t = self.table.clone();
        let p = path.clone();
        let view = self.view.clone();
        self.spawn(JobKind::Export(path), None, move || {
            let rows = pick(&view);
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

    /// True when the rows shown are the result of the filter and sort
    /// shown (not a typing pause, not a failed or cancelled query); else
    /// says why `what` did not happen.
    fn view_is_current(&mut self, what: &str) -> bool {
        if self.pending || self.last_query.as_ref() != Some(&self.query()) {
            self.status = Some(format!(
                "{what}: the current filter / sort is not applied yet or failed; \
                 wait for it (or fix it), then try again"
            ));
            return false;
        }
        true
    }

    /// Copies the values of file column `c` in the view (first 1 M rows),
    /// one per line, in the background.
    pub fn start_copy_column(&mut self, c: usize) {
        if self.job.is_some() || !self.ready() {
            self.status = Some("busy: wait for the current operation".into());
            return;
        }
        if !self.view_is_current("not copied") {
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
    /// something is running or about to start (to keep repainting).
    pub fn poll(&mut self, tz_name: &str) -> bool {
        if self.tz.name != tz_name {
            self.tz = tz::Display::new(tz_name);
            self.rows_key = None;
            self.colors.invalidate();
            // Time filters are read in the display zone.
            self.requery();
        }
        if let Some(rx) = &self.loader {
            match rx.try_recv() {
                Ok(r) => {
                    self.loader = None;
                    match r {
                        Ok(()) => {
                            self.pending = true;
                            // Column kinds are known now.
                            self.colors.invalidate();
                            self.rows_key = None;
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
                        Ok(JobResult::View(v, counts)) => {
                            self.excluded = counts.into_iter().collect();
                            let sel_row = self.grid.selected.and_then(|p| self.view.row_at(p));
                            self.view = v;
                            self.update_hits();
                            self.view_gen += 1;
                            self.last_ms = ms;
                            self.last_query = job.and_then(|j| j.query);
                            // Keep the selected row selected if still shown.
                            self.grid.selected = sel_row.and_then(|r| self.view.position_of(r));
                            // Ranges are display positions (D-060).
                            self.grid.clear_ranges();
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
                JobKind::Export(_) | JobKind::CopyColumn | JobKind::CopyCells => false,
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
                    let ranged = self.range_columns();
                    self.spawn(JobKind::Query, Some(q), move || {
                        let v = fhv_engine::view::run(&engine, &q2).map_err(|e| e.to_string())?;
                        let mut counts = Vec::new();
                        for c in ranged {
                            counts.push((c, engine.unparsable(c).map_err(|e| e.to_string())?));
                        }
                        Ok(JobResult::View(v, counts))
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
        // Also while input waits for its pause: the query starts on a later
        // frame, and nothing else would cause one (D-055).
        self.job.is_some() || self.loader.is_some() || !settled
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
                JobKind::CopyCells => "copying the selection".to_string(),
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

    /// The file column used as the row number: the detected one, unless
    /// the mode says to show the viewer's own `#` (D-042).
    pub fn num_col(&self) -> Option<usize> {
        match self.rownum_mode {
            RowNumMode::Always => None,
            _ => self.detected_col(),
        }
    }

    /// The detected row-number column (whatever the mode).
    pub fn detected_col(&self) -> Option<usize> {
        self.detected.get().copied().flatten()
    }

    /// Whether the viewer's own `#` column is shown.
    pub fn show_hash(&self) -> bool {
        match self.rownum_mode {
            RowNumMode::Always => true,
            RowNumMode::Auto => self.num_col().is_none(),
            RowNumMode::Never => false,
        }
    }

    /// Row number of data row `r` as shown: the file's value when a column
    /// is used, else its position in the file.
    pub fn row_number(&self, r: u32) -> String {
        let key = (r, self.num_col());
        if let Some((k, v)) = &*self.num_cache.borrow()
            && *k == key
        {
            return v.clone();
        }
        let mut n = (r as u64 + 1).to_string();
        if let Some(c) = key.1
            && let Ok(f) = self.table.row_fields(r as u64)
            && let Some(v) = f.get(c)
        {
            n = v.clone();
        }
        *self.num_cache.borrow_mut() = Some((key, n.clone()));
        n
    }

    /// Data row for the number `n` typed in "go to row".
    pub fn row_of_number(&self, n: u64) -> Option<u32> {
        let r = match self.num_col() {
            Some(c) => fhv_engine::rownum::find_in_table(&self.table, c, n)?,
            None => n.checked_sub(1)?,
        };
        u32::try_from(r)
            .ok()
            .filter(|&r| (r as u64) < self.table.row_count())
    }

    /// Syncs grid widths from the per-column widths.
    pub fn sync_widths(&mut self) {
        if self.grid.sel_cols != self.order {
            self.grid.sel_cols = self.order.clone();
            self.grid.clear_ranges();
        }
        let mut w = vec![26.0, if self.show_hash() { 70.0 } else { 0.0 }];
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

    /// Uses `rules` (the app's ruleset) for coloring.
    pub fn set_rules(&mut self, rules: std::sync::Arc<Vec<crate::rules::Rule>>) {
        if self.colors.set(rules) {
            self.rows_key = None;
        }
    }

    /// Prepared rows are read and colored again on the next frame.
    pub fn repaint_cells(&mut self) {
        self.rows_key = None;
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

    /// Tags the rows at the given display positions (at most 1 M).
    pub fn tag_positions(&mut self, positions: &[Range<u64>]) {
        const MAX: usize = 1_000_000;
        let rows: Vec<u32> = positions
            .iter()
            .flat_map(|r| r.clone())
            .map_while(|p| self.view.row_at(p))
            .take(MAX + 1)
            .collect();
        if rows.len() > MAX {
            self.status = Some("not tagged: more than 1 000 000 rows selected".into());
            return;
        }
        for &r in &rows {
            self.tags.entry(r).or_default();
        }
        self.status = Some(format!(
            "tagged {} rows",
            crate::app::fmt_n(rows.len() as u64)
        ));
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
    pub fn source<'a>(&'a mut self, tz: &'a tz::Display, tagged: Color32) -> TabSource<'a> {
        TabSource {
            tab: self,
            tz,
            tagged,
        }
    }
}

pub struct TabSource<'a> {
    pub tab: &'a mut Tab,
    tz: &'a tz::Display,
    tagged: Color32,
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
        if let Kind::Time(f) = self.tab.kind(c)
            && f.year_assumed()
        {
            s.push_str(" (year?)");
        }
        s.push_str(&self.tab.dupes.header_mark(c));
        s
    }

    fn movable(&self, col: usize) -> bool {
        col >= DC_FIRST
    }

    fn header_tip(&self, col: usize) -> Option<String> {
        let c = self.tab.file_col(col)?;
        let tips: Vec<String> = [
            match self.tab.kind(c) {
                Kind::Time(f) => Some(format!("dates: {}", f.describe())),
                _ => None,
            },
            self.tab.dupes.header_tip(c),
        ]
        .into_iter()
        .flatten()
        .collect();
        (!tips.is_empty()).then(|| tips.join("\n"))
    }

    fn prepare(&mut self, rows: Range<u64>) {
        self.tab.nums_first = rows.start;
        self.tab.nums = if self.tab.show_hash() {
            rows.clone()
                .map(|p| {
                    self.tab
                        .view
                        .row_at(p)
                        .map_or(String::new(), |r| (r as u64 + 1).to_string())
                })
                .collect()
        } else {
            Vec::new()
        };
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
        // Rules and duplicate counts look at the file's text.
        crate::rules::paint(tab, rows.clone());
        // Display time zone for time columns, and parsed times of columns
        // whose cells are not the instant as written (epoch numbers...).
        if tab.table.column_index().is_some() {
            for c in 0..tab.table.columns.len() {
                if let Kind::Time(f) = tab.engine.kind(c)
                    && (!self.tz.is_utc() || f.rewrites())
                {
                    for p in rows.clone() {
                        if let Some(cell) = tab.rows.cell_mut(p, c)
                            && let Some(v) = self.tz.convert(cell, f)
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
        // A row rule wins over the tagged color (the star stays).
        if let Some(c) = crate::rules::row_color(self.tab, row) {
            return Some(c);
        }
        let r = self.tab.view.row_at(row)?;
        if self.tab.tags.contains_key(&r) {
            return Some(self.tagged);
        }
        None
    }

    fn cell_style(&self, row: u64, col: usize) -> crate::grid::CellStyle {
        match self.tab.file_col(col) {
            Some(c) => crate::rules::cell_style(self.tab, row, c),
            None => Default::default(),
        }
    }

    fn cell_tip(&self, row: u64, col: usize) -> Option<String> {
        crate::rules::cell_tip(self.tab, row, self.tab.file_col(col)?)
    }
}
