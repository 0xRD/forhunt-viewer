//! The application window: menus, tabs, toolbar, grid, detail pane, status
//! bar and dialogs.

use eframe::egui::{self, Color32, Key, KeyboardShortcut, Modifiers, RichText, Ui};
use fhv_engine::export::Format;
use fhv_engine::filter::{Op, value_counts};
use fhv_engine::sort::SortKey;
use std::path::{Path, PathBuf};

use crate::grid::{self, Callbacks, Style};
use crate::session;
use crate::settings::{Settings, dirs};
use crate::tab::{ColFilter, DC_FIRST, DC_NUM, DC_TAG, Tab};
use crate::tz;

/// Index caches and converted copies above this are evicted at start-up.
const CACHE_MAX: u64 = 20 << 30;

/// Operators offered in the column filter editor.
const OPS: &[&str] = &[
    "equals",
    "not equals",
    "contains",
    "not contains",
    "begins with",
    "ends with",
    "is blank",
    "is not blank",
    "in list",
    "not in list",
    "regex",
    ">",
    ">=",
    "<",
    "<=",
    "between",
];

fn make_op(name: &str, a: &str, b: &str) -> Op {
    let (a, b) = (a.to_string(), b.to_string());
    match name {
        "equals" => Op::Equals(a),
        "not equals" => Op::NotEquals(a),
        "not contains" => Op::NotContains(a),
        "begins with" => Op::BeginsWith(a),
        "ends with" => Op::EndsWith(a),
        "is blank" => Op::IsBlank,
        "is not blank" => Op::IsNotBlank,
        "regex" => Op::Regex(a),
        ">" => Op::Gt(a),
        ">=" => Op::Ge(a),
        "<" => Op::Lt(a),
        "<=" => Op::Le(a),
        "between" => Op::Between(a, b),
        _ => Op::Contains(a),
    }
}

pub fn op_text(op: &Op) -> String {
    match op {
        Op::IsBlank | Op::IsNotBlank => op.label().to_string(),
        Op::In(v) | Op::NotIn(v) => {
            let shown: Vec<&str> = v.iter().take(3).map(String::as_str).collect();
            let more = if v.len() > 3 {
                format!(" +{}", v.len() - 3)
            } else {
                String::new()
            };
            format!("{} [{}{}]", op.label(), shown.join(", "), more)
        }
        Op::Between(a, b) => format!("between {a} and {b}"),
        Op::Equals(v)
        | Op::NotEquals(v)
        | Op::Contains(v)
        | Op::NotContains(v)
        | Op::BeginsWith(v)
        | Op::EndsWith(v)
        | Op::Regex(v)
        | Op::Gt(v)
        | Op::Ge(v)
        | Op::Lt(v)
        | Op::Le(v) => format!("{} {v:?}", op.label()),
    }
}

/// Column filter editor state (bound to one file).
struct FilterEdit {
    path: PathBuf,
    col: usize,
    op: String,
    a: String,
    b: String,
    /// Values of "in list" / "not in list", one per line.
    list: String,
    value_search: String,
    /// Dictionary values with counts in the view (None until available).
    values: Option<Vec<(String, u64)>>,
    /// Checked state per value.
    checked: Vec<bool>,
    /// Indexes of values matching `value_search`, for `shown_for`.
    shown: Vec<usize>,
    shown_for: Option<String>,
    /// Where it was opened from (the window starts there).
    at: Option<egui::Pos2>,
}

struct ExportDlg {
    tagged: bool,
    jsonl: bool,
}

pub struct App {
    pub(crate) tabs: Vec<Tab>,
    active: usize,
    settings: Settings,
    tz: tz::Display,
    cache_dir: PathBuf,
    error: Option<String>,
    filter_edit: Option<FilterEdit>,
    pub(crate) columns_dlg: bool,
    export_dlg: Option<ExportDlg>,
    goto: Option<String>,
    note_edit: Option<(u32, String)>,
    evidence_dlg: Option<String>,
    pub(crate) help: bool,
    focus_search: bool,
    style: Style,
    /// Fields of the row shown in the detail pane (read once per row, not
    /// per frame): (file, data row, raw values, displayed values).
    detail: Option<(PathBuf, u32, Vec<String>, Vec<String>)>,
    /// Hidden scroll benchmark (env FHV_BENCH_SCROLL=frames): frame times.
    bench: Option<(usize, Vec<f64>, Vec<f64>)>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, files: Vec<PathBuf>) -> App {
        App::with(cc, files, Settings::load(), dirs().cache)
    }

    /// With explicit settings and cache directory (tests).
    pub fn with(
        cc: &eframe::CreationContext<'_>,
        files: Vec<PathBuf>,
        settings: Settings,
        cache_dir: PathBuf,
    ) -> App {
        crate::fonts::install(&cc.egui_ctx);
        cc.egui_ctx.set_visuals(if settings.dark {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        });
        let mut app = App {
            tabs: Vec::new(),
            active: 0,
            tz: tz::Display::new(&settings.tz),
            settings,
            cache_dir,
            error: None,
            filter_edit: None,
            columns_dlg: false,
            export_dlg: None,
            goto: None,
            note_edit: None,
            evidence_dlg: None,
            help: false,
            focus_search: false,
            detail: None,
            bench: std::env::var("FHV_BENCH_SCROLL")
                .ok()
                .and_then(|v| v.parse().ok())
                .map(|n| (n, Vec::new(), Vec::new())),
            style: Style {
                filter_h: 24.0,
                ..Style::default()
            },
        };
        // Keep the cache bounded (oldest files first).
        let cache = app.cache_dir.clone();
        let _ = std::thread::Builder::new()
            .name("fhv-cache-evict".into())
            .spawn(move || crate::settings::evict_cache(&cache, CACHE_MAX));
        for f in files {
            app.open(&f);
        }
        app
    }

    /// Opens a file in a new tab (or switches to it if already open).
    pub fn open(&mut self, path: &Path) {
        if let Some(i) = self.tabs.iter().position(|t| t.path == path) {
            self.active = i;
            return;
        }
        match Tab::open(path, &self.cache_dir) {
            Ok(t) => {
                self.tabs.push(t);
                self.active = self.tabs.len() - 1;
                self.settings.add_recent(path.to_path_buf());
                self.settings.save();
            }
            Err(e) => self.error = Some(format!("{}: {e}", path.display())),
        }
    }

    fn open_dialog(&mut self) {
        if let Some(files) = rfd::FileDialog::new()
            .add_filter("CSV / TSV", &["csv", "tsv", "txt", "tab", "psv"])
            .add_filter("All files", &["*"])
            .pick_files()
        {
            for f in files {
                self.open(&f);
            }
        }
    }

    fn tab(&mut self) -> Option<&mut Tab> {
        self.tabs.get_mut(self.active)
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        let cmd = |k| KeyboardShortcut::new(Modifiers::COMMAND, k);
        let cmd_shift = |k| KeyboardShortcut::new(Modifiers::COMMAND | Modifiers::SHIFT, k);
        let (open, close, find, goto, copy_row, copy, f3, sf3, esc) = ctx.input_mut(|i| {
            (
                i.consume_shortcut(&cmd(Key::O)),
                i.consume_shortcut(&cmd(Key::W)),
                i.consume_shortcut(&cmd(Key::F)),
                i.consume_shortcut(&cmd(Key::G)),
                i.consume_shortcut(&cmd_shift(Key::C)),
                i.events.iter().any(|e| matches!(e, egui::Event::Copy)),
                i.consume_shortcut(&KeyboardShortcut::new(Modifiers::NONE, Key::F3)),
                i.consume_shortcut(&KeyboardShortcut::new(Modifiers::SHIFT, Key::F3)),
                i.key_pressed(Key::Escape),
            )
        });
        let typing = ctx.memory(|m| m.focused().is_some()) && ctx.egui_wants_keyboard_input();
        if open {
            self.open_dialog();
        }
        if close && !self.tabs.is_empty() {
            self.close_tab(self.active);
        }
        if find {
            self.focus_search = true;
        }
        if goto {
            self.goto = Some(String::new());
        }
        if copy_row && let Some(t) = self.tab() {
            copy_selected_row(ctx, t);
        }
        if copy
            && !typing
            && let Some(t) = self.tab()
        {
            copy_selected_cell(ctx, t);
        }
        if (f3 || sf3)
            && let Some(t) = self.tab()
            && let Some(p) = t.find_hit(f3)
        {
            t.grid.selected = Some(p);
            grid::scroll_to(&mut t.grid, p);
        }
        if esc && let Some(t) = self.tab() {
            t.cancel_job();
        }
    }

    fn close_tab(&mut self, i: usize) {
        if i < self.tabs.len() {
            self.tabs[i].cancel_job();
            self.tabs.remove(i);
            self.active = self.active.min(self.tabs.len().saturating_sub(1));
        }
    }

    fn menu(&mut self, ui: &mut Ui) {
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                if ui.button("Open…   Ctrl+O").clicked() {
                    ui.close();
                    self.open_dialog();
                }
                ui.menu_button("Open recent", |ui| {
                    let recent = self.settings.recent.clone();
                    if recent.is_empty() {
                        ui.label("(none)");
                    }
                    for r in recent {
                        if ui.button(r.display().to_string()).clicked() {
                            ui.close();
                            self.open(&r);
                        }
                    }
                });
                ui.separator();
                let has = !self.tabs.is_empty();
                if ui
                    .add_enabled(has, egui::Button::new("Export rows…"))
                    .clicked()
                {
                    ui.close();
                    self.export_dlg = Some(ExportDlg {
                        tagged: false,
                        jsonl: false,
                    });
                }
                if ui
                    .add_enabled(has, egui::Button::new("Save session…"))
                    .clicked()
                {
                    ui.close();
                    self.save_session();
                }
                if ui.button("Load session…").clicked() {
                    ui.close();
                    self.load_session();
                }
                ui.separator();
                if ui
                    .add_enabled(has, egui::Button::new("Close tab   Ctrl+W"))
                    .clicked()
                {
                    ui.close();
                    self.close_tab(self.active);
                }
                if ui.button("Quit").clicked() {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
            ui.menu_button("View", |ui| {
                if ui.checkbox(&mut self.settings.dark, "Dark theme").changed() {
                    ui.ctx().set_visuals(if self.settings.dark {
                        egui::Visuals::dark()
                    } else {
                        egui::Visuals::light()
                    });
                    self.settings.save();
                }
                if ui
                    .checkbox(&mut self.settings.detail_pane, "Detail pane")
                    .changed()
                {
                    self.settings.save();
                }
                if ui.button("Columns…").clicked() {
                    ui.close();
                    self.columns_dlg = true;
                }
                if ui
                    .button("Clear index cache")
                    .on_hover_text(format!(
                        "{} in {} (indexes and converted UTF-16 / Windows-1252 copies; rebuilt when needed)",
                        fmt_bytes(crate::settings::cache_size(&self.cache_dir)),
                        self.cache_dir.display()
                    ))
                    .clicked()
                {
                    ui.close();
                    let freed = crate::settings::evict_cache(&self.cache_dir, 0);
                    if let Some(t) = self.tabs.get_mut(self.active) {
                        t.status = Some(format!("cleared {} of cache", fmt_bytes(freed)));
                    }
                }
                ui.menu_button(format!("Time zone: {}", self.tz.name), |ui| {
                    ui.label("Display only; the data is not changed.");
                    for z in tz::COMMON {
                        if ui.selectable_label(self.tz.name == *z, *z).clicked() {
                            ui.close();
                            self.settings.tz = z.to_string();
                            self.tz = tz::Display::new(z);
                            self.settings.save();
                        }
                    }
                });
            });
            ui.menu_button("forhunt", |ui| {
                if ui.button("Evidence path for commands…").clicked() {
                    ui.close();
                    self.evidence_dlg = Some(self.settings.evidence.clone());
                }
                let has_id = self.tab_ref().is_some_and(|t| forhunt_col(t).is_some());
                if ui
                    .add_enabled(has_id, egui::Button::new("Export tagged ForhuntIds…"))
                    .on_disabled_hover_text("needs a ForhuntId column and tagged rows")
                    .clicked()
                {
                    ui.close();
                    self.export_forhunt_ids();
                }
            });
            ui.menu_button("Help", |ui| {
                if ui.button("Keyboard shortcuts").clicked() {
                    ui.close();
                    self.help = true;
                }
            });
        });
    }

    fn tab_ref(&self) -> Option<&Tab> {
        self.tabs.get(self.active)
    }

    fn tabs_bar(&mut self, ui: &mut Ui) {
        ui.horizontal_wrapped(|ui| {
            let mut close = None;
            for (i, t) in self.tabs.iter().enumerate() {
                let label = if t.loading() {
                    format!("● {}", t.title())
                } else {
                    t.title()
                };
                if ui
                    .selectable_label(i == self.active, label)
                    .on_hover_text(t.path.display().to_string())
                    .clicked()
                {
                    self.active = i;
                }
                if ui.small_button("×").on_hover_text("close").clicked() {
                    close = Some(i);
                }
                ui.separator();
            }
            if ui.button("+ Open").clicked() {
                self.open_dialog();
            }
            if let Some(i) = close {
                self.close_tab(i);
            }
        });
    }

    fn toolbar(&mut self, ui: &mut Ui) {
        let focus = std::mem::take(&mut self.focus_search);
        let Some(t) = self.tabs.get_mut(self.active) else {
            return;
        };
        ui.horizontal(|ui| {
            let lbl = ui.label("Search");
            let r = ui
                .add(
                    egui::TextEdit::singleline(&mut t.search)
                        .hint_text("any column (Ctrl+F)")
                        .desired_width(260.0),
                )
                .labelled_by(lbl.id);
            if focus {
                r.request_focus();
            }
            if r.changed() && t.search_filter {
                t.requery();
            }
            let hits = t.hits_in_view;
            if !t.search.is_empty() {
                ui.label(match hits {
                    Some(h) => format!("{h} rows"),
                    None => "…".into(),
                });
            }
            if ui
                .button("◀")
                .on_hover_text("previous match (Shift+F3)")
                .clicked()
                && let Some(p) = t.find_hit(false)
            {
                t.grid.selected = Some(p);
                grid::scroll_to(&mut t.grid, p);
            }
            if ui.button("▶").on_hover_text("next match (F3)").clicked()
                && let Some(p) = t.find_hit(true)
            {
                t.grid.selected = Some(p);
                grid::scroll_to(&mut t.grid, p);
            }
            if ui.checkbox(&mut t.search_filter, "filter").changed() {
                t.requery();
            }
            ui.separator();
            let ntags = t.tags.len();
            if ui
                .toggle_value(&mut t.tagged_only, format!("★ tagged ({ntags})"))
                .on_hover_text("show tagged rows only")
                .changed()
            {
                t.requery();
            }
            let nbad = if t.ready() { t.bad_rows().len() } else { 0 }; // cached
            if nbad > 0
                && ui
                    .toggle_value(
                        &mut t.bad_only,
                        RichText::new(format!("⚠ bad rows ({nbad})"))
                            .color(Color32::from_rgb(230, 120, 60)),
                    )
                    .on_hover_text("rows with a wrong number of fields or a broken quote")
                    .changed()
            {
                t.requery();
            }
            ui.separator();
            let any = t.filters.iter().any(ColFilter::is_active)
                || t.tagged_only
                || t.bad_only
                || t.search_filter;
            if ui
                .add_enabled(any, egui::Button::new("Clear filters"))
                .clicked()
            {
                for f in &mut t.filters {
                    *f = ColFilter::default();
                }
                t.tagged_only = false;
                t.bad_only = false;
                t.search_filter = false;
                t.requery();
            }
            if ui
                .add_enabled(!t.sort.is_empty(), egui::Button::new("Clear sort"))
                .clicked()
            {
                t.sort.clear();
                t.requery();
            }
            ui.separator();
            ui.label("Freeze");
            let max = t.display_count();
            ui.add(egui::DragValue::new(&mut t.grid.frozen).range(0..=max))
                .on_hover_text("columns kept at the left when scrolling");
        });
        // Filter chips.
        let mut remove: Option<(usize, Option<usize>)> = None;
        let has_chips = t.filters.iter().any(ColFilter::is_active) || !t.sort.is_empty();
        // Always one line (even empty) so the grid does not jump when the
        // first filter or sort appears.
        ui.horizontal_wrapped(|ui| {
            if !has_chips {
                ui.label(RichText::new("no filter, file order").weak());
            }
            {
                for (c, f) in t.filters.iter().enumerate() {
                    let name = &t.table.columns[c];
                    if !f.quick.is_empty()
                        && ui
                            .button(format!("{name} contains {:?}  ×", f.quick))
                            .on_hover_text("remove")
                            .clicked()
                    {
                        remove = Some((c, None));
                    }
                    for (i, op) in f.conds.iter().enumerate() {
                        let joiner = if i > 0 {
                            if f.any { "or " } else { "and " }
                        } else {
                            ""
                        };
                        if ui
                            .button(format!("{joiner}{name} {}  ×", op_text(op)))
                            .on_hover_text("remove")
                            .clicked()
                        {
                            remove = Some((c, Some(i)));
                        }
                    }
                }
                if !t.sort.is_empty() {
                    let s: Vec<String> = t
                        .sort
                        .iter()
                        .map(|k| {
                            format!(
                                "{}{}",
                                t.table.columns[k.col],
                                if k.desc { " ↓" } else { " ↑" }
                            )
                        })
                        .collect();
                    ui.label(RichText::new(format!("sorted by {}", s.join(", "))).weak());
                }
            }
        });
        if let Some((c, i)) = remove {
            match i {
                None => t.filters[c].quick.clear(),
                Some(i) => {
                    t.filters[c].conds.remove(i);
                }
            }
            t.requery();
        }
    }

    fn status_bar(&mut self, ui: &mut Ui) {
        let Some(t) = self.tabs.get_mut(self.active) else {
            ui.label("Open a CSV file: File > Open, drag and drop, or forhunt-viewer FILE");
            return;
        };
        ui.horizontal(|ui| {
            let total = t.table.row_count();
            ui.label(format!(
                "showing {} of {} rows",
                fmt_n(t.view.len()),
                fmt_n(total)
            ));
            if let Some(ms) = t.last_ms {
                ui.label(RichText::new(format!("({ms:.0} ms)")).weak());
            }
            ui.separator();
            if let Some((what, frac)) = t.progress_text() {
                if frac >= 0.0 {
                    ui.add(egui::ProgressBar::new(frac).desired_width(140.0).text(what));
                } else {
                    ui.spinner();
                    ui.label(what);
                }
                if (t.job.is_some() || t.loading()) && ui.small_button("cancel (Esc)").clicked() {
                    t.cancel_job();
                }
                ui.separator();
            }
            if let Some(e) = &t.load_error {
                ui.colored_label(Color32::RED, format!("error: {e}"));
            }
            if let Some(s) = &t.status {
                ui.label(s);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let d = &t.table.dialect;
                let delim = match d.delim {
                    b'\t' => "tab".to_string(),
                    c => format!("'{}'", c as char),
                };
                ui.label(
                    RichText::new(format!(
                        "{} | {} | {} | {}{}",
                        t.table.encoding.name(),
                        delim,
                        if d.header { "header" } else { "no header" },
                        fmt_bytes(t.table.file_len),
                        if t.table
                            .from_cache
                            .load(std::sync::atomic::Ordering::Relaxed)
                        {
                            " | cached index"
                        } else {
                            ""
                        }
                    ))
                    .weak(),
                );
            });
        });
    }

    fn detail_pane(&mut self, ui: &mut Ui) {
        let Some(t) = self.tabs.get_mut(self.active) else {
            return;
        };
        let Some(row) = t.grid.selected.and_then(|p| t.view.row_at(p)) else {
            ui.label(RichText::new("Select a row").weak());
            return;
        };
        let cached = self
            .detail
            .as_ref()
            .is_some_and(|d| d.0 == t.path && d.1 == row);
        if !cached {
            match t.table.row_fields(row as u64) {
                Ok(f) => {
                    let shown = f
                        .iter()
                        .map(|v| pretty_json(v).unwrap_or_else(|| v.clone()))
                        .collect();
                    self.detail = Some((t.path.clone(), row, f, shown));
                }
                Err(e) => {
                    ui.colored_label(Color32::RED, e.to_string());
                    return;
                }
            }
        }
        let Some((_, _, fields, shown_all)) = &self.detail else {
            return;
        };
        ui.horizontal(|ui| {
            ui.heading(format!("Row {}", row as u64 + 1));
            let tagged = t.tags.contains_key(&row);
            if ui
                .button(if tagged { "★ untag" } else { "☆ tag" })
                .clicked()
            {
                t.toggle_tag(row);
            }
            if tagged && ui.button("note…").clicked() {
                self.note_edit = Some((row, t.tags.get(&row).cloned().unwrap_or_default()));
            }
            if ui.button("copy row").clicked() {
                ui.ctx().copy_text(row_tsv(t, fields));
            }
        });
        if let Some(note) = t.tags.get(&row).filter(|n| !n.is_empty()) {
            ui.label(RichText::new(format!("note: {note}")).italics());
        }
        let kinds = t.table.column_index();
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| {
                egui::Grid::new("detail")
                    .num_columns(2)
                    .striped(true)
                    .show(ui, |ui| {
                        for (i, v) in fields.iter().enumerate() {
                            let name = t
                                .table
                                .columns
                                .get(i)
                                .cloned()
                                .unwrap_or_else(|| format!("(extra field {})", i + 1));
                            let r = ui.label(RichText::new(name).strong());
                            r.context_menu(|ui| {
                                if ui.button("Copy value").clicked() {
                                    ui.ctx().copy_text(v.clone());
                                    ui.close();
                                }
                            });
                            ui.vertical(|ui| {
                                let mut text = shown_all[i].as_str();
                                ui.add(
                                    egui::TextEdit::multiline(&mut text)
                                        .font(egui::TextStyle::Monospace)
                                        .desired_rows(1)
                                        .desired_width(f32::INFINITY),
                                );
                                if let (Some(ci), false) = (&kinds, self.tz.is_utc())
                                    && let Some(col) = ci.cols.get(i)
                                    && let fhv_engine::columns::Kind::Time(o) = col.kind
                                    && let Some(c) = self.tz.convert(v, o)
                                {
                                    ui.label(
                                        RichText::new(format!("{c} ({})", self.tz.name)).weak(),
                                    );
                                }
                            });
                            ui.end_row();
                        }
                    });
            });
    }

    fn grid(&mut self, ui: &mut Ui) {
        let dark = self.settings.dark;
        let evidence = self.settings.evidence.clone();
        let Some(t) = self.tabs.get_mut(self.active) else {
            ui.centered_and_justified(|ui| {
                ui.label(
                    RichText::new("Drop CSV files here, or File > Open (Ctrl+O)")
                        .size(18.0)
                        .weak(),
                );
            });
            return;
        };
        if let Some(e) = &t.load_error {
            ui.colored_label(
                Color32::RED,
                format!("Cannot open {}: {e}", t.path.display()),
            );
            return;
        }
        t.sync_widths();
        let ready = t.ready();
        let highlight = t.search.clone();
        // Filter row and menus need the tab mutably while the grid holds the
        // source; collect their actions and apply after.
        let mut quick_edits: Vec<(usize, String)> = Vec::new();
        let open_editor: std::cell::Cell<Option<usize>> = std::cell::Cell::new(None);
        let mut header_action: Option<HeaderAction> = None;
        let mut cell_action: Option<CellAction> = None;
        let filters: Vec<ColFilter> = t.filters.clone();
        let col_names = t.table.columns.clone();
        let order = t.order.clone();
        let file_col = |dc: usize| dc.checked_sub(DC_FIRST).and_then(|i| order.get(i).copied());
        let has_forhunt = forhunt_col(t).is_some();
        let resp = {
            let mut filter_cell = |ui: &mut Ui, dc: usize, rect: egui::Rect| {
                let Some(c) = file_col(dc) else { return };
                let mut q = filters[c].quick.clone();
                let bw = 18.0;
                let te = egui::Rect::from_min_max(
                    rect.min,
                    egui::pos2(rect.max.x - bw - 2.0, rect.max.y),
                );
                let r = ui.put(
                    te,
                    egui::TextEdit::singleline(&mut q)
                        .hint_text("contains…")
                        .margin(egui::vec2(4.0, 2.0))
                        .interactive(ready),
                );
                // Screen readers (and UI tests) see "filter <column>".
                let label = format!("filter {}", col_names[c]);
                ui.ctx()
                    .accesskit_node_builder(r.id, |n| n.set_label(label));
                if r.changed() {
                    quick_edits.push((c, q));
                }
                let br =
                    egui::Rect::from_min_max(egui::pos2(rect.max.x - bw, rect.min.y), rect.max);
                let active = !filters[c].conds.is_empty();
                let b = ui.put(
                    br,
                    egui::Button::new(if active { "▼" } else { "▾" })
                        .small()
                        .selected(active),
                );
                if b.on_hover_text("filter conditions and value list")
                    .clicked()
                {
                    open_editor.set(Some(c));
                }
            };
            let mut header_menu = |ui: &mut Ui, dc: usize| {
                let Some(c) = file_col(dc) else {
                    ui.label("(virtual column)");
                    return;
                };
                ui.label(RichText::new(&col_names[c]).strong());
                if ui.button("Sort ascending").clicked() {
                    header_action = Some(HeaderAction::Sort(c, false));
                    ui.close();
                }
                if ui.button("Sort descending").clicked() {
                    header_action = Some(HeaderAction::Sort(c, true));
                    ui.close();
                }
                if ui.button("Filter…").clicked() {
                    open_editor.set(Some(c));
                    ui.close();
                }
                if ui.button("Hide column").clicked() {
                    header_action = Some(HeaderAction::Hide(c));
                    ui.close();
                }
                if ui.button("Freeze up to here").clicked() {
                    header_action = Some(HeaderAction::Freeze(dc + 1));
                    ui.close();
                }
                if ui.button("Copy column values (view)").clicked() {
                    header_action = Some(HeaderAction::CopyColumn(c));
                    ui.close();
                }
            };
            let mut cell_menu = |ui: &mut Ui, row: u64, dc: usize| {
                if ui.button("Copy cell   Ctrl+C").clicked() {
                    cell_action = Some(CellAction::CopyCell(row, dc));
                    ui.close();
                }
                if ui.button("Copy row   Ctrl+Shift+C").clicked() {
                    cell_action = Some(CellAction::CopyRow(row));
                    ui.close();
                }
                if ui.button("Tag / untag").clicked() {
                    cell_action = Some(CellAction::Tag(row));
                    ui.close();
                }
                if ui.button("Tag note…").clicked() {
                    cell_action = Some(CellAction::Note(row));
                    ui.close();
                }
                if let Some(c) = file_col(dc) {
                    if ui
                        .button(format!("Filter {} = this value", col_names[c]))
                        .clicked()
                    {
                        cell_action = Some(CellAction::FilterEq(row, c, false));
                        ui.close();
                    }
                    if ui
                        .button(format!("Filter {} ≠ this value", col_names[c]))
                        .clicked()
                    {
                        cell_action = Some(CellAction::FilterEq(row, c, true));
                        ui.close();
                    }
                }
                if has_forhunt {
                    ui.separator();
                    if ui.button("Copy `forhunt show` command").clicked() {
                        cell_action = Some(CellAction::ForhuntShow(row));
                        ui.close();
                    }
                }
            };
            let tz = &self.tz;
            // The grid state lives in the tab, which the source borrows.
            let mut gs = std::mem::take(&mut t.grid);
            let mut src = t.source(tz, dark);
            let r = grid::show(
                ui,
                egui::Id::unique(("grid", self.active)),
                &mut gs,
                &mut src,
                &self.style,
                &highlight,
                Callbacks {
                    filter_cell: &mut filter_cell,
                    header_menu: &mut header_menu,
                    cell_menu: &mut cell_menu,
                },
            );
            t.grid = gs;
            r
        };
        t.store_widths();
        for (c, q) in quick_edits {
            t.filters[c].quick = q;
            t.requery();
        }
        if let Some((dc, shift)) = resp.header_clicked
            && let Some(c) = t.file_col(dc)
            && ready
        {
            match t.sort.iter().position(|k| k.col == c) {
                Some(i) if shift => t.sort[i].desc = !t.sort[i].desc,
                Some(i) if t.sort.len() == 1 => {
                    if t.sort[i].desc {
                        t.sort.clear();
                    } else {
                        t.sort[i].desc = true;
                    }
                }
                _ if shift => t.sort.push(SortKey {
                    col: c,
                    desc: false,
                }),
                _ => {
                    t.sort = vec![SortKey {
                        col: c,
                        desc: false,
                    }]
                }
            }
            t.requery();
        }
        if let Some((row, dc)) = resp.clicked_cell
            && dc == DC_TAG
            && let Some(r) = t.view.row_at(row)
        {
            t.toggle_tag(r);
        }
        if let Some(a) = header_action {
            match a {
                HeaderAction::Sort(c, desc) => {
                    t.sort = vec![SortKey { col: c, desc }];
                    t.requery();
                }
                HeaderAction::Hide(c) => t.order.retain(|&x| x != c),
                HeaderAction::Freeze(n) => t.grid.frozen = n,
                HeaderAction::CopyColumn(c) => t.start_copy_column(c),
            }
        }
        if let Some(a) = cell_action {
            match a {
                CellAction::CopyCell(row, dc) => {
                    t.grid.selected = Some(row);
                    t.grid.selected_col = dc;
                    copy_selected_cell(ui.ctx(), t);
                }
                CellAction::CopyRow(row) => {
                    t.grid.selected = Some(row);
                    copy_selected_row(ui.ctx(), t);
                }
                CellAction::Tag(row) => {
                    if let Some(r) = t.view.row_at(row) {
                        t.toggle_tag(r);
                    }
                }
                CellAction::Note(row) => {
                    if let Some(r) = t.view.row_at(row) {
                        t.tags.entry(r).or_default();
                        self.note_edit = Some((r, t.tags.get(&r).cloned().unwrap_or_default()));
                    }
                }
                CellAction::FilterEq(row, c, neg) => {
                    if let Some(r) = t.view.row_at(row)
                        && let Ok(f) = t.table.row_fields(r as u64)
                    {
                        let v = f.get(c).cloned().unwrap_or_default();
                        t.filters[c]
                            .conds
                            .push(if neg { Op::NotEquals(v) } else { Op::Equals(v) });
                        t.requery();
                    }
                }
                CellAction::ForhuntShow(row) => {
                    if let (Some(r), Some(c)) = (t.view.row_at(row), forhunt_col(t))
                        && let Ok(f) = t.table.row_fields(r as u64)
                    {
                        let id = f.get(c).cloned().unwrap_or_default();
                        // The id comes from the file: only a well-formed
                        // forhunt id goes into a shell command.
                        if valid_forhunt_id(&id) {
                            let ev = if evidence.is_empty() {
                                "<evidence>".to_string()
                            } else {
                                evidence.clone()
                            };
                            ui.ctx()
                                .copy_text(format!("forhunt show {id} -p {}", shell_quote(&ev)));
                            t.status = Some("copied forhunt command".into());
                        } else {
                            t.status =
                                Some(format!("not a forhunt id: {:?}", grid::display_text(&id)));
                        }
                    }
                }
            }
        }
        if let Some(c) = open_editor.get() {
            self.open_filter_editor(c);
        }
    }

    pub(crate) fn open_filter_editor(&mut self, c: usize) {
        let Some(t) = self.tab() else { return };
        let path = t.path.clone();
        self.filter_edit = Some(FilterEdit {
            path,
            col: c,
            op: "contains".into(),
            a: String::new(),
            b: String::new(),
            list: String::new(),
            value_search: String::new(),
            values: None,
            checked: Vec::new(),
            shown: Vec::new(),
            shown_for: None,
            at: None,
        });
    }

    fn filter_editor(&mut self, ctx: &egui::Context) {
        let Some(fe) = &mut self.filter_edit else {
            return;
        };
        // The editor belongs to one file: close it when that tab is no
        // longer the active one (closed, switched, another file opened).
        let Some(t) = self.tabs.get_mut(self.active).filter(|t| t.path == fe.path) else {
            self.filter_edit = None;
            return;
        };
        let c = fe.col;
        if c >= t.table.columns.len() {
            self.filter_edit = None;
            return;
        }
        // Value list: built once the column index exists (it may not when
        // the editor is opened during indexing), counts within the view.
        if fe.values.is_none()
            && t.ready()
            && let Some(ci) = t.table.column_index()
        {
            if let Some(v) = value_counts(&ci, c, Some(&t.view.rows)) {
                let cond = t.filters[c]
                    .conds
                    .iter()
                    .find(|o| matches!(o, Op::In(_) | Op::NotIn(_)));
                fe.checked = v
                    .iter()
                    .map(|(val, _)| match cond {
                        Some(Op::In(vs)) => vs.iter().any(|x| x == val),
                        Some(Op::NotIn(vs)) => !vs.iter().any(|x| x == val),
                        _ => true,
                    })
                    .collect();
                fe.values = Some(v);
            } else {
                fe.values = Some(Vec::new());
            }
        }
        if fe.at.is_none() {
            fe.at = ctx.pointer_latest_pos().or(Some(egui::pos2(200.0, 150.0)));
        }
        let tz_note = (!self.tz.is_utc()).then(|| self.tz.name.clone());
        let mut open = true;
        let mut changed = false;
        egui::Window::new(format!("Filter: {}", t.table.columns[c]))
            .id(egui::Id::unique(("filter-edit", c)))
            .default_pos(fe.at.unwrap_or_default())
            .open(&mut open)
            .default_width(400.0)
            .show(ctx, |ui| {
                let f = &mut t.filters[c];
                if !f.conds.is_empty() {
                    let mut del = None;
                    for (i, op) in f.conds.iter().enumerate() {
                        ui.horizontal(|ui| {
                            ui.label(op_text(op));
                            if ui.small_button("×").clicked() {
                                del = Some(i);
                            }
                        });
                    }
                    if let Some(i) = del {
                        f.conds.remove(i);
                        changed = true;
                    }
                    ui.horizontal(|ui| {
                        changed |= ui.radio_value(&mut f.any, false, "match all").changed();
                        changed |= ui.radio_value(&mut f.any, true, "match any").changed();
                    });
                    ui.separator();
                }
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt("op")
                        .selected_text(fe.op.clone())
                        .show_ui(ui, |ui| {
                            for o in OPS {
                                ui.selectable_value(&mut fe.op, o.to_string(), *o);
                            }
                        });
                    let list_op = matches!(fe.op.as_str(), "in list" | "not in list");
                    if !list_op && !matches!(fe.op.as_str(), "is blank" | "is not blank") {
                        ui.add(egui::TextEdit::singleline(&mut fe.a).desired_width(150.0));
                    }
                    if fe.op == "between" {
                        ui.label("and");
                        ui.add(egui::TextEdit::singleline(&mut fe.b).desired_width(150.0));
                    }
                    if ui.button("Add").clicked() {
                        let op = if list_op {
                            let vals: Vec<String> = fe
                                .list
                                .lines()
                                .map(|l| l.trim().to_string())
                                .filter(|l| !l.is_empty())
                                .collect();
                            if fe.op == "in list" { Op::In(vals) } else { Op::NotIn(vals) }
                        } else {
                            make_op(&fe.op, &fe.a, &fe.b)
                        };
                        f.conds.push(op);
                        fe.a.clear();
                        fe.b.clear();
                        fe.list.clear();
                        changed = true;
                    }
                });
                if matches!(fe.op.as_str(), "in list" | "not in list") {
                    ui.label(RichText::new("One value per line (paste a list):").small());
                    ui.add(egui::TextEdit::multiline(&mut fe.list).desired_rows(4).desired_width(f32::INFINITY));
                }
                changed |= ui.checkbox(&mut f.case_sensitive, "case sensitive").changed();
                let mut hint = String::from(
                    "Dates: 2024-03-11 10:00:00, ISO 8601 or the column's own format. Numbers compare numerically.",
                );
                if let Some(z) = &tz_note {
                    hint.push_str(&format!(
                        " Dates are compared with the values in the file (UTC), not the {z} times shown."
                    ));
                }
                ui.label(RichText::new(hint).weak().small());
                match &fe.values {
                    Some(values) if !values.is_empty() => {
                        ui.separator();
                        ui.horizontal(|ui| {
                            ui.label(format!("Values ({}, counts in view)", values.len()));
                            ui.add(
                                egui::TextEdit::singleline(&mut fe.value_search)
                                    .hint_text("search values")
                                    .desired_width(140.0),
                            );
                        });
                        if fe.shown_for.as_deref() != Some(fe.value_search.as_str()) {
                            let needle = fe.value_search.to_lowercase();
                            fe.shown = (0..values.len())
                                .filter(|&i| needle.is_empty() || values[i].0.to_lowercase().contains(&needle))
                                .collect();
                            fe.shown_for = Some(fe.value_search.clone());
                        }
                        ui.horizontal(|ui| {
                            if ui.small_button("all").clicked() {
                                for &i in &fe.shown {
                                    fe.checked[i] = true;
                                }
                            }
                            if ui.small_button("none").clicked() {
                                for &i in &fe.shown {
                                    fe.checked[i] = false;
                                }
                            }
                        });
                        // Only the visible part of the list is laid out.
                        let row_h = ui.spacing().interact_size.y;
                        egui::ScrollArea::vertical().max_height(260.0).show_rows(ui, row_h, fe.shown.len(), |ui, range| {
                            for &i in &fe.shown[range] {
                                let (v, n) = &values[i];
                                let label = if v.is_empty() {
                                    "(blank)".to_string()
                                } else {
                                    grid::display_text(v).into_owned()
                                };
                                ui.checkbox(&mut fe.checked[i], format!("{label}   ({})", fmt_n(*n)));
                            }
                        });
                        if ui.button("Apply value selection").clicked() {
                            f.conds.retain(|o| !matches!(o, Op::In(_) | Op::NotIn(_)));
                            let on: Vec<String> =
                                values.iter().zip(&fe.checked).filter(|(_, c)| **c).map(|(v, _)| v.0.clone()).collect();
                            let off: Vec<String> =
                                values.iter().zip(&fe.checked).filter(|(_, c)| !**c).map(|(v, _)| v.0.clone()).collect();
                            if !off.is_empty() {
                                // The shorter list selects the same rows.
                                f.conds.push(if on.len() <= off.len() { Op::In(on) } else { Op::NotIn(off) });
                            }
                            changed = true;
                        }
                    }
                    Some(_) => {
                        ui.label(RichText::new("No value list: too many distinct values.").weak().small());
                    }
                    None => {
                        ui.label(RichText::new("Value list available once indexing finishes.").weak().small());
                    }
                }
            });
        if changed {
            t.requery();
        }
        if !open {
            self.filter_edit = None;
        }
    }

    fn columns_window(&mut self, ctx: &egui::Context) {
        if !self.columns_dlg {
            return;
        }
        let Some(t) = self.tabs.get_mut(self.active) else {
            self.columns_dlg = false;
            return;
        };
        let mut open = true;
        egui::Window::new("Columns")
            .open(&mut open)
            .default_height(500.0)
            .show(ctx, |ui| {
                ui.label(RichText::new("Checked columns are shown; drag ↕ to reorder.").weak());
                ui.horizontal(|ui| {
                    if ui.button("Show all").clicked() {
                        for c in 0..t.table.columns.len() {
                            if !t.order.contains(&c) {
                                t.order.push(c);
                            }
                        }
                    }
                    if ui.button("Reset order").clicked() {
                        t.order = (0..t.table.columns.len()).collect();
                    }
                });
                egui::ScrollArea::vertical().show(ui, |ui| {
                    // Visible columns in order (draggable), then hidden ones.
                    let mut moved: Option<(usize, usize)> = None;
                    let mut hide: Option<usize> = None;
                    for (i, &c) in t.order.iter().enumerate() {
                        ui.horizontal(|ui| {
                            let h = ui.add(egui::Label::new("↕").sense(egui::Sense::drag()));
                            if h.dragged()
                                && let Some(p) = ui.ctx().pointer_interact_pos()
                            {
                                let dy = p.y - h.rect.center().y;
                                let steps = (dy / h.rect.height().max(1.0)).round() as i64;
                                if steps != 0 {
                                    let to = (i as i64 + steps).clamp(0, t.order.len() as i64 - 1)
                                        as usize;
                                    moved = Some((i, to));
                                }
                            }
                            let mut on = true;
                            if ui.checkbox(&mut on, &t.table.columns[c]).changed() && !on {
                                hide = Some(c);
                            }
                            if ui.small_button("↑").clicked() && i > 0 {
                                moved = Some((i, i - 1));
                            }
                            if ui.small_button("↓").clicked() {
                                moved = Some((i, (i + 1).min(t.order.len() - 1)));
                            }
                        });
                    }
                    if let Some((a, b)) = moved {
                        let c = t.order.remove(a);
                        t.order.insert(b, c);
                    }
                    if let Some(c) = hide {
                        t.order.retain(|&x| x != c);
                    }
                    ui.separator();
                    for c in 0..t.table.columns.len() {
                        if !t.order.contains(&c) {
                            let mut on = false;
                            if ui.checkbox(&mut on, &t.table.columns[c]).changed() && on {
                                t.order.push(c);
                            }
                        }
                    }
                });
            });
        self.columns_dlg = open;
    }

    fn export_window(&mut self, ctx: &egui::Context) {
        let Some(dlg) = &mut self.export_dlg else {
            return;
        };
        let mut open = true;
        let mut go = false;
        egui::Window::new("Export rows")
            .open(&mut open)
            .show(ctx, |ui| {
                ui.radio_value(
                    &mut dlg.tagged,
                    false,
                    "Rows in the current view (filter + sort)",
                );
                ui.radio_value(&mut dlg.tagged, true, "Tagged rows only");
                ui.horizontal(|ui| {
                    ui.radio_value(&mut dlg.jsonl, false, "CSV");
                    ui.radio_value(&mut dlg.jsonl, true, "JSONL");
                });
                ui.label(
                    RichText::new(
                    "Visible columns, in display order. UTF-8, LF line ends; bytes that are not valid UTF-8 become U+FFFD.",
                )
                .weak(),
                );
                go = ui.button("Export…").clicked();
            });
        if go {
            let (tagged, jsonl) = (dlg.tagged, dlg.jsonl);
            self.export_dlg = None;
            let Some(t) = self.tabs.get_mut(self.active) else {
                return;
            };
            let ext = if jsonl { "jsonl" } else { "csv" };
            let stem = t
                .path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            if let Some(p) = rfd::FileDialog::new()
                .set_file_name(format!("{stem}-export.{ext}"))
                .add_filter(ext, &[ext])
                .save_file()
            {
                if let Some(e) = refuse_target(&self.tabs, &p) {
                    self.error = Some(e);
                    return;
                }
                let Some(t) = self.tabs.get_mut(self.active) else {
                    return;
                };
                let cols = t.order.clone();
                t.start_export(
                    p,
                    tagged,
                    cols,
                    if jsonl { Format::Jsonl } else { Format::Csv },
                );
            }
        } else if !open {
            self.export_dlg = None;
        }
    }

    fn small_dialogs(&mut self, ctx: &egui::Context) {
        if let Some(text) = &mut self.goto {
            let mut done = false;
            let mut open = true;
            egui::Window::new("Go to row")
                .open(&mut open)
                .collapsible(false)
                .show(ctx, |ui| {
                    let r =
                        ui.add(egui::TextEdit::singleline(text).hint_text("row number (# column)"));
                    r.request_focus();
                    if ui.input(|i| i.key_pressed(Key::Enter)) {
                        done = true;
                    }
                });
            if done {
                if let (Ok(n), Some(t)) = (
                    text.trim().replace(['_', ','], "").parse::<u64>(),
                    self.tabs.get_mut(self.active),
                ) {
                    let r = n.checked_sub(1).and_then(|r| u32::try_from(r).ok());
                    match r.and_then(|r| t.view.position_of(r)) {
                        Some(p) => {
                            t.grid.selected = Some(p);
                            grid::scroll_to(&mut t.grid, p);
                            t.status = None;
                        }
                        None => t.status = Some(format!("row {n} is not in the current view")),
                    }
                }
                self.goto = None;
            } else if !open {
                self.goto = None;
            }
        }
        if let Some((row, text)) = &mut self.note_edit {
            let mut open = true;
            let mut save = false;
            egui::Window::new(format!("Note for row {}", *row as u64 + 1))
                .open(&mut open)
                .show(ctx, |ui| {
                    ui.add(egui::TextEdit::multiline(text).desired_rows(3));
                    save = ui.button("Save").clicked();
                });
            if save {
                if let Some(t) = self.tabs.get_mut(self.active) {
                    t.tags.insert(*row, text.clone());
                }
                self.note_edit = None;
            } else if !open {
                self.note_edit = None;
            }
        }
        if let Some(text) = &mut self.evidence_dlg {
            let mut open = true;
            let mut save = false;
            egui::Window::new("forhunt evidence path")
                .open(&mut open)
                .show(ctx, |ui| {
                    ui.label("Used in copied `forhunt show <id> -p <evidence>` commands.");
                    ui.add(egui::TextEdit::singleline(text).desired_width(360.0));
                    save = ui.button("Save").clicked();
                });
            if save {
                self.settings.evidence = text.clone();
                self.settings.save();
                self.evidence_dlg = None;
            } else if !open {
                self.evidence_dlg = None;
            }
        }
        if self.help {
            egui::Window::new("Keyboard shortcuts")
                .open(&mut self.help)
                .show(ctx, |ui| {
                    egui::Grid::new("keys").striped(true).show(ui, |ui| {
                        for (k, v) in [
                            ("Ctrl+O", "open files"),
                            ("Ctrl+W", "close tab"),
                            ("Ctrl+F", "global search"),
                            ("F3 / Shift+F3", "next / previous match"),
                            ("Ctrl+G", "go to row"),
                            ("Arrows, PgUp/PgDn", "move selection"),
                            ("Ctrl+Home / Ctrl+End", "first / last row"),
                            ("Ctrl+C", "copy cell"),
                            ("Ctrl+Shift+C", "copy row (tab-separated)"),
                            ("Click header", "sort (again: descending, then off)"),
                            ("Shift+click header", "add a sort column"),
                            ("Right click", "header and cell menus"),
                            ("Esc", "cancel the running operation"),
                        ] {
                            ui.label(RichText::new(k).monospace());
                            ui.label(v);
                            ui.end_row();
                        }
                    });
                });
        }
        if let Some(e) = self.error.clone() {
            let mut open = true;
            egui::Window::new("Error").open(&mut open).show(ctx, |ui| {
                ui.label(e);
            });
            if !open {
                self.error = None;
            }
        }
    }

    fn save_session(&mut self) {
        let Some(t) = self.tab_ref() else { return };
        let s = session::capture(t);
        let stem = t
            .path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        if let Some(p) = rfd::FileDialog::new()
            .set_file_name(format!("{stem}.fhv-session.json"))
            .add_filter("session", &["json"])
            .save_file()
        {
            if let Some(e) = refuse_target(&self.tabs, &p) {
                self.error = Some(e);
                return;
            }
            let r = serde_json::to_vec_pretty(&s)
                .map_err(|e| e.to_string())
                .and_then(|b| crate::settings::write_atomic(&p, &b).map_err(|e| e.to_string()));
            if let Err(e) = r {
                self.error = Some(format!("{}: {e}", p.display()));
            }
        }
    }

    fn load_session(&mut self) {
        let Some(p) = rfd::FileDialog::new()
            .add_filter("session", &["json"])
            .pick_file()
        else {
            return;
        };
        let s: session::Session = match std::fs::read(&p)
            .map_err(|e| e.to_string())
            .and_then(|b| serde_json::from_slice(&b).map_err(|e| e.to_string()))
        {
            Ok(s) => s,
            Err(e) => {
                self.error = Some(format!("{}: {e}", p.display()));
                return;
            }
        };
        self.open(&s.file);
        if let Some(t) = self.tabs.iter_mut().find(|t| t.path == s.file) {
            let warn = session::apply(t, &s);
            if !warn.is_empty() {
                self.error = Some(warn.join("\n"));
            }
        }
    }

    fn export_forhunt_ids(&mut self) {
        let Some(t) = self.tab_ref() else { return };
        let Some(c) = forhunt_col(t) else { return };
        if t.tags.is_empty() {
            self.error = Some("no tagged rows".into());
            return;
        }
        let mut out = String::new();
        for (&r, note) in &t.tags {
            if let Ok(f) = t.table.row_fields(r as u64) {
                let id = f.get(c).cloned().unwrap_or_default();
                if note.is_empty() {
                    out.push_str(&format!("{id}\n"));
                } else {
                    out.push_str(&format!("{id}\t{}\n", note.replace(['\n', '\t'], " ")));
                }
            }
        }
        if let Some(p) = rfd::FileDialog::new()
            .set_file_name("forhunt-tagged-ids.txt")
            .save_file()
        {
            if let Some(e) = refuse_target(&self.tabs, &p) {
                self.error = Some(e);
            } else if let Err(e) = crate::settings::write_atomic(&p, out.as_bytes()) {
                self.error = Some(format!("{}: {e}", p.display()));
            }
        }
    }
}

enum HeaderAction {
    Sort(usize, bool),
    Hide(usize),
    Freeze(usize),
    CopyColumn(usize),
}

enum CellAction {
    CopyCell(u64, usize),
    CopyRow(u64),
    Tag(u64),
    Note(u64),
    FilterEq(u64, usize, bool),
    ForhuntShow(u64),
}

fn forhunt_col(t: &Tab) -> Option<usize> {
    t.table
        .columns
        .iter()
        .position(|c| c.eq_ignore_ascii_case("ForhuntId"))
}

/// Single-quotes a value for POSIX shells and PowerShell (a single quote
/// inside is closed, escaped and reopened: POSIX style).
fn shell_quote(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._-:\\".contains(c))
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// `<8 hex>:<digits>`, as forhunt writes ids.
fn valid_forhunt_id(id: &str) -> bool {
    match id.split_once(':') {
        Some((k, r)) => {
            k.len() == 8
                && k.bytes().all(|b| b.is_ascii_hexdigit())
                && !r.is_empty()
                && r.len() <= 20
                && r.bytes().all(|b| b.is_ascii_digit())
        }
        None => false,
    }
}

/// Refuses writing to a file that is open as an input (also through a
/// symlink, a relative path or a hard link): evidence is never modified.
pub fn refuse_target(tabs: &[Tab], target: &Path) -> Option<String> {
    let canon = |p: &Path| std::fs::canonicalize(p).ok();
    let t = canon(target);
    for tab in tabs {
        let same = match (&t, canon(&tab.path)) {
            (Some(a), Some(b)) => *a == b || same_inode(a, &b),
            _ => target == tab.path,
        };
        if same {
            return Some(format!(
                "refusing to write to {}: it is an open input file",
                target.display()
            ));
        }
    }
    None
}

#[cfg(unix)]
fn same_inode(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(x), Ok(y)) => x.dev() == y.dev() && x.ino() == y.ino(),
        _ => false,
    }
}

#[cfg(not(unix))]
fn same_inode(_: &Path, _: &Path) -> bool {
    false
}

fn row_tsv(t: &Tab, fields: &[String]) -> String {
    t.order
        .iter()
        .map(|&c| {
            fields
                .get(c)
                .map_or("", String::as_str)
                .replace(['\t', '\n', '\r'], " ")
        })
        .collect::<Vec<_>>()
        .join("\t")
}

fn copy_selected_cell(ctx: &egui::Context, t: &mut Tab) {
    let Some(r) = t.grid.selected.and_then(|p| t.view.row_at(p)) else {
        return;
    };
    let dc = t.grid.selected_col;
    let text = match dc {
        DC_TAG => t.tags.get(&r).cloned().unwrap_or_default(),
        DC_NUM => (r as u64 + 1).to_string(),
        _ => match (t.file_col(dc), t.table.row_fields(r as u64)) {
            (Some(c), Ok(f)) => f.get(c).cloned().unwrap_or_default(),
            _ => return,
        },
    };
    ctx.copy_text(text);
}

fn copy_selected_row(ctx: &egui::Context, t: &mut Tab) {
    let Some(r) = t.grid.selected.and_then(|p| t.view.row_at(p)) else {
        return;
    };
    if let Ok(f) = t.table.row_fields(r as u64) {
        ctx.copy_text(row_tsv(t, &f));
    }
}

/// Re-indents a JSON object/array for reading without changing it: keys
/// keep their order, duplicates stay, numbers keep their spelling (only
/// whitespace outside strings changes). None if it is not valid JSON.
fn pretty_json(v: &str) -> Option<String> {
    let s = v.trim();
    if !(s.starts_with('{') || s.starts_with('[')) || v.len() > 2_000_000 {
        return None;
    }
    serde_json::from_str::<serde::de::IgnoredAny>(s).ok()?;
    let mut out = String::with_capacity(s.len() * 2);
    let mut depth = 0usize;
    let (mut in_str, mut esc) = (false, false);
    let nl = |out: &mut String, d: usize| {
        out.push('\n');
        out.extend(std::iter::repeat_n("  ", d));
    };
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if in_str {
            out.push(c);
            if esc {
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' => {
                in_str = true;
                out.push(c);
            }
            '{' | '[' => {
                out.push(c);
                // Empty container stays on one line.
                while chars.peek().is_some_and(|c| c.is_whitespace()) {
                    chars.next();
                }
                if matches!(chars.peek(), Some('}') | Some(']')) {
                    out.push(chars.next().unwrap_or(' '));
                } else {
                    depth += 1;
                    nl(&mut out, depth);
                }
            }
            '}' | ']' => {
                depth = depth.saturating_sub(1);
                nl(&mut out, depth);
                out.push(c);
            }
            ',' => {
                out.push(c);
                nl(&mut out, depth);
            }
            ':' => out.push_str(": "),
            c if c.is_whitespace() => {}
            c => out.push(c),
        }
    }
    Some(out)
}

pub fn fmt_n(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn fmt_bytes(n: u64) -> String {
    match n {
        n if n >= 1 << 30 => format!("{:.2} GiB", n as f64 / (1u64 << 30) as f64),
        n if n >= 1 << 20 => format!("{:.1} MiB", n as f64 / (1u64 << 20) as f64),
        n => format!("{:.1} KiB", n as f64 / 1024.0),
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let t_frame = std::time::Instant::now();
        self.bench_step(&ctx, frame);
        // Files dropped on the window.
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .collect()
        });
        for p in dropped {
            self.open(&p);
        }
        #[cfg(target_os = "macos")]
        for p in crate::macos::take_opened() {
            self.open(&p);
        }
        let tz = self.tz.name.clone();
        let mut running = false;
        for t in &mut self.tabs {
            running |= t.poll(&tz);
            if let Some(text) = t.clipboard.take() {
                ctx.copy_text(text);
            }
        }
        if running {
            // Progress refresh only: background work does not need 60 fps.
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
        self.shortcuts(&ctx);
        let title = match self.tab_ref() {
            Some(t) => format!("{} — forhunt-viewer", t.path.display()),
            None => "forhunt-viewer".into(),
        };
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));

        egui::Panel::top("menu").show(ui, |ui| self.menu(ui));
        if !self.tabs.is_empty() {
            egui::Panel::top("tabs").show(ui, |ui| self.tabs_bar(ui));
            egui::Panel::top("toolbar").show(ui, |ui| self.toolbar(ui));
        }
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));
        if self.settings.detail_pane && !self.tabs.is_empty() {
            egui::Panel::right("detail")
                .resizable(true)
                .default_size(380.0)
                .min_size(240.0)
                .show(ui, |ui| self.detail_pane(ui));
        }
        egui::CentralPanel::default().show(ui, |ui| self.grid(ui));
        self.filter_editor(&ctx);
        self.columns_window(&ctx);
        self.export_window(&ctx);
        self.small_dialogs(&ctx);
        if let Some((_, ui_ms, _)) = &mut self.bench
            && self
                .tabs
                .first()
                .is_some_and(|t| t.ready() && t.job.is_none())
        {
            ui_ms.push(t_frame.elapsed().as_secs_f64() * 1e3);
        }
    }
}

impl App {
    /// Same pattern as the phase 1 spike: 40 frames of smooth scrolling (3
    /// rows/frame), then 10 jumps, once the first tab is ready.
    fn bench_step(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
        let Some((total, ui_ms, cpu_ms)) = &mut self.bench else {
            return;
        };
        let Some(t) = self.tabs.first_mut() else {
            return;
        };
        if !(t.ready() && t.job.is_none()) {
            return;
        }
        let k = ui_ms.len();
        if k > 10
            && let Some(c) = frame.info().cpu_usage
        {
            cpu_ms.push(c as f64 * 1e3);
        }
        let n = t.view.len().max(1) as f64;
        if k % 50 < 40 {
            t.grid.top += 3.0;
        } else {
            t.grid.top = (k as f64 * 7_919_993.0) % n;
        }
        ctx.request_repaint();
        if k == *total {
            let pct = |v: &mut Vec<f64>, p: f64| {
                v.sort_by(f64::total_cmp);
                v.get(((v.len().max(1) - 1) as f64 * p) as usize)
                    .copied()
                    .unwrap_or(0.0)
            };
            let mut u = ui_ms[10..].to_vec();
            println!(
                "frames={} rows={} ui_ms p50={:.3} p99={:.3} max={:.3} | cpu_usage_ms p50={:.3} p99={:.3} max={:.3}",
                u.len(),
                t.view.len(),
                pct(&mut u, 0.5),
                pct(&mut u, 0.99),
                pct(&mut u, 1.0),
                pct(cpu_ms, 0.5),
                pct(cpu_ms, 0.99),
                pct(cpu_ms, 1.0)
            );
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

#[cfg(test)]
pub fn pretty_json_for_test(v: &str) -> Option<String> {
    pretty_json(v)
}
#[cfg(test)]
pub fn valid_forhunt_id_for_test(v: &str) -> bool {
    valid_forhunt_id(v)
}
#[cfg(test)]
pub fn shell_quote_for_test(v: &str) -> String {
    shell_quote(v)
}
