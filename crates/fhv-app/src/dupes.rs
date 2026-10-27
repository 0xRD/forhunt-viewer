//! Duplicate / unique value highlighting per column (D-066): the header
//! menu sets a mode and a scope, value counts come from
//! `fhv_engine::dupes` (dictionary bitmaps, else a background scan with
//! progress and cancel), and the grid looks each visible cell up.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};

use eframe::egui::Ui;
use fhv_engine::dupes::Counts;
use roaring::RoaringBitmap;
use serde::{Deserialize, Serialize};

use crate::tab::{JobKind, Tab};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    #[default]
    Off,
    Duplicates,
    Unique,
    Both,
}

impl Mode {
    pub fn duplicates(self) -> bool {
        matches!(self, Mode::Duplicates | Mode::Both)
    }

    pub fn unique(self) -> bool {
        matches!(self, Mode::Unique | Mode::Both)
    }
}

/// Rows the counts are taken over.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// The rows the current filter keeps.
    #[default]
    View,
    File,
}

/// What a count was made for: the view's rows (None: whole file) and case.
#[derive(Clone)]
struct Key {
    rows: Option<Arc<RoaringBitmap>>,
    case_sensitive: bool,
}

impl Key {
    fn same(&self, o: &Key) -> bool {
        self.case_sensitive == o.case_sensitive
            && match (&self.rows, &o.rows) {
                (None, None) => true,
                (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                _ => false,
            }
    }
}

struct Job {
    rx: Receiver<Result<Counts, String>>,
    cancel: Arc<AtomicBool>,
    done: Arc<AtomicU64>,
    total: u64,
    key: Key,
}

impl Drop for Job {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

pub struct Col {
    pub mode: Mode,
    pub scope: Scope,
    counts: Option<(Arc<Counts>, Key)>,
    job: Option<Job>,
    pub error: Option<String>,
}

#[derive(Default)]
pub struct Dupes {
    /// By file column.
    pub cols: BTreeMap<usize, Col>,
}

impl Dupes {
    pub fn get(&self, c: usize) -> Option<(Mode, Scope)> {
        self.cols.get(&c).map(|x| (x.mode, x.scope))
    }

    pub fn mode(&self, c: usize) -> Mode {
        self.cols.get(&c).map_or(Mode::Off, |x| x.mode)
    }

    /// Sets the mode of file column `c` (Off removes it and stops its count).
    pub fn set(&mut self, c: usize, mode: Mode, scope: Scope) {
        if mode == Mode::Off {
            self.cols.remove(&c);
            return;
        }
        let col = self.cols.entry(c).or_insert(Col {
            mode,
            scope,
            counts: None,
            job: None,
            error: None,
        });
        col.mode = mode;
        if col.scope != scope {
            col.scope = scope;
            col.counts = None;
            col.job = None;
        }
        col.error = None;
    }

    /// Columns with counts ready: (file column, counts, mode).
    pub fn ready(&self) -> Vec<(usize, Arc<Counts>, Mode)> {
        self.cols
            .iter()
            .filter_map(|(c, x)| x.counts.as_ref().map(|(k, _)| (*c, k.clone(), x.mode)))
            .collect()
    }

    pub fn scope_text(&self, c: usize) -> &'static str {
        match self.cols.get(&c).map(|x| x.scope) {
            Some(Scope::File) => "in the file",
            _ => "in the filtered view",
        }
    }

    /// Header text: counting progress, or what is highlighted.
    pub fn header_mark(&self, c: usize) -> String {
        let Some(x) = self.cols.get(&c) else {
            return String::new();
        };
        if x.error.is_some() {
            return " [dupes: error]".into();
        }
        if let Some(j) = &x.job {
            let pct = j.done.load(Ordering::Relaxed) * 100 / j.total.max(1);
            return format!(" [counting {}%]", pct.min(100));
        }
        let capped = x.counts.as_ref().is_some_and(|(k, _)| k.capped);
        let m = match x.mode {
            Mode::Duplicates => "dup",
            Mode::Unique => "uniq",
            Mode::Both => "dup+uniq",
            Mode::Off => return String::new(),
        };
        format!(" [{m}{}]", if capped { ", capped" } else { "" })
    }

    /// Tooltip of the header mark.
    pub fn header_tip(&self, c: usize) -> Option<String> {
        let x = self.cols.get(&c)?;
        if let Some(e) = &x.error {
            return Some(format!("duplicate count failed: {e}"));
        }
        let (k, _) = x.counts.as_ref()?;
        let (u, d) = k.unique_and_duplicate();
        let mut s = format!(
            "{} unique and {} duplicated values {}",
            crate::app::fmt_n(u),
            crate::app::fmt_n(d),
            self.scope_text(c)
        );
        if k.capped {
            s.push_str(&format!(
                "; more than {} distinct values: values first seen after that are not counted",
                crate::app::fmt_n(fhv_engine::dupes::MAX_DISTINCT as u64)
            ));
        }
        Some(s)
    }

    pub fn running(&self) -> bool {
        self.cols.values().any(|x| x.job.is_some())
    }
}

/// Starts, finishes and restarts counts (view or case changed). Returns
/// true while a count runs. `case_sensitive`: the global case setting.
pub fn poll(t: &mut Tab, case_sensitive: bool) -> bool {
    if t.dupes.cols.is_empty() || !t.ready() {
        return false;
    }
    // Wait for the view a running query is about to show.
    let querying = t
        .job
        .as_ref()
        .is_some_and(|j| matches!(j.kind, JobKind::Query));
    let mut changed = false;
    let view = t.view.rows.clone();
    let table = t.table.clone();
    for (&c, col) in t.dupes.cols.iter_mut() {
        if let Some(j) = &col.job {
            match j.rx.try_recv() {
                Ok(Ok(k)) => {
                    let key = j.key.clone();
                    col.counts = Some((Arc::new(k), key));
                    col.job = None;
                    changed = true;
                }
                // Cancelled by other work on the table: counted again below.
                Ok(Err(e)) if e == "cancelled" => col.job = None,
                Ok(Err(e)) => {
                    col.error = Some(e);
                    col.job = None;
                }
                Err(TryRecvError::Disconnected) => col.job = None,
                Err(TryRecvError::Empty) => {}
            }
        }
        let want = Key {
            rows: (col.scope == Scope::View).then(|| view.clone()),
            case_sensitive,
        };
        if col.counts.as_ref().is_some_and(|(_, k)| !k.same(&want)) {
            col.counts = None;
            changed = true;
        }
        if col.job.as_ref().is_some_and(|j| !j.key.same(&want)) {
            col.job = None;
        }
        if col.counts.is_none()
            && col.job.is_none()
            && col.error.is_none()
            && !querying
            && !table.progress.cancelled()
        {
            let (tx, rx) = std::sync::mpsc::channel();
            let cancel = Arc::new(AtomicBool::new(false));
            let done = Arc::new(AtomicU64::new(0));
            let total = want.rows.as_ref().map_or(table.row_count(), |r| r.len());
            let (tb, cl, dn, rows) = (
                table.clone(),
                cancel.clone(),
                done.clone(),
                want.rows.clone(),
            );
            let _ = std::thread::Builder::new()
                .name("fhv-dupes".into())
                .spawn(move || {
                    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        fhv_engine::dupes::count(&tb, c, rows.as_deref(), case_sensitive, &cl, &dn)
                            .map_err(|e| e.to_string())
                    }))
                    .unwrap_or_else(|_| Err("internal error (panic)".into()));
                    let _ = tx.send(r);
                });
            col.job = Some(Job {
                rx,
                cancel,
                done,
                total,
                key: want,
            });
        }
    }
    if changed {
        t.repaint_cells();
    }
    t.dupes.running()
}

/// Header menu entry; returns the new (mode, scope) when picked.
pub fn menu(ui: &mut Ui, cur: Option<(Mode, Scope)>) -> Option<(Mode, Scope)> {
    let (mode, scope) = cur.unwrap_or_default();
    let mut out = None;
    ui.menu_button("Highlight duplicates / unique", |ui| {
        for (m, label) in [
            (Mode::Off, "Off"),
            (Mode::Duplicates, "Highlight duplicates"),
            (Mode::Unique, "Highlight unique values (appear once)"),
            (Mode::Both, "Both"),
        ] {
            if ui.selectable_label(mode == m, label).clicked() {
                out = Some((m, scope));
                ui.close();
            }
        }
        ui.separator();
        ui.label("Count over:");
        ui.add_enabled_ui(mode != Mode::Off, |ui| {
            for (s, label) in [
                (Scope::View, "the filtered view"),
                (Scope::File, "the whole file"),
            ] {
                if ui.selectable_label(scope == s, label).clicked() {
                    out = Some((mode, s));
                    ui.close();
                }
            }
        });
        ui.label("Blank cells are ignored; case follows the Aa setting.");
    });
    out
}
