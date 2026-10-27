//! Appearance: theme (D-056), grid font and size, UI scale (D-057), the
//! View > Settings dialog, and the repaint helpers of D-055.

use std::sync::mpsc::{Receiver, channel};

use eframe::egui::{self, FontId, Key, KeyboardShortcut, Modifiers, RichText, Ui};

use crate::fonts;
use crate::grid::Style;
use crate::settings::{self, GridFont, Settings};
use crate::theme::{self, Theme};

/// A loaded font, None for a bundled one.
type FontLoad = Option<Result<egui::FontData, String>>;

pub struct State {
    pub settings_open: bool,
    /// Theme under the pointer in a picker this frame / the last one
    /// (previewed while hovered).
    hovered: Option<String>,
    preview: Option<String>,
    applied: String,
    pub user_themes: Vec<Theme>,
    theme_errors: Vec<String>,
    /// Installed families (name, monospaced), listed off the UI thread.
    catalog: Option<Receiver<Vec<(String, bool)>>>,
    families: Option<Vec<(String, bool)>>,
    family_search: String,
    /// Font being loaded, and the font shown until it is.
    loading: Option<(GridFont, Receiver<FontLoad>)>,
    font_applied: Option<GridFont>,
    pub font_error: Option<String>,
    /// Ctrl+wheel zoom: unrounded size.
    zoom_acc: Option<f32>,
    scale_applied: f32,
    scale_edit: Option<f32>,
}

impl State {
    pub fn new(config_dir: &std::path::Path) -> State {
        let (user_themes, theme_errors) = theme::load_dir(&config_dir.join("themes"));
        State {
            settings_open: false,
            hovered: None,
            preview: None,
            applied: String::new(),
            user_themes,
            theme_errors,
            catalog: None,
            families: None,
            family_search: String::new(),
            loading: None,
            font_applied: None,
            font_error: None,
            zoom_acc: None,
            scale_applied: 0.0,
            scale_edit: None,
        }
    }

    /// Once per frame before drawing: theme (OS setting, preview), grid
    /// font and size, UI scale. Changes only what changed.
    pub fn frame(&mut self, ctx: &egui::Context, s: &mut Settings, style: &mut Style) {
        self.preview = self.hovered.take();
        let want = self.preview.clone().unwrap_or_else(|| s.theme_name());
        let t = theme::resolve(&want, ctx.system_theme(), &self.user_themes);
        if t.name != self.applied {
            self.applied = t.name.clone();
            theme::apply(ctx, t);
            // The panels of this frame were styled before the change.
            ctx.request_repaint();
        }
        if self.scale_applied != s.ui_scale {
            self.scale_applied = s.ui_scale;
            ctx.set_zoom_factor(s.ui_scale);
            ctx.request_repaint();
        }
        let size = settings::clamp_size(s.grid_font_size);
        if style.font.size != size || style.font.family != fonts::grid_family() {
            style.font = FontId::new(size, fonts::grid_family());
            style.row_h = row_height(size);
        }
        self.font(ctx, s);
    }

    fn font(&mut self, ctx: &egui::Context, s: &mut Settings) {
        if let Some((choice, rx)) = &self.loading {
            let Ok(r) = rx.try_recv() else { return };
            let choice = choice.clone();
            self.loading = None;
            match r {
                Some(Err(e)) => {
                    // Keep the previous font.
                    self.font_error = Some(format!("{}: {e}", font_label(&choice)));
                    let prev = self.font_applied.clone().unwrap_or_default();
                    if s.grid_font == choice {
                        s.grid_font = prev;
                        s.save();
                    }
                }
                data => {
                    fonts::install(ctx, &choice, data.and_then(Result::ok));
                    self.font_applied = Some(choice);
                    ctx.request_repaint();
                }
            }
            return;
        }
        if self.font_applied.as_ref() == Some(&s.grid_font) {
            return;
        }
        let choice = s.grid_font.clone();
        if matches!(choice, GridFont::Bundled(_)) {
            fonts::install(ctx, &choice, None);
            self.font_applied = Some(choice);
            ctx.request_repaint();
            return;
        }
        // Installed fonts and files are read off the UI thread; the grid
        // keeps the current font until then.
        let (tx, rx) = channel();
        let (c, ctx2) = (choice.clone(), ctx.clone());
        let _ = std::thread::Builder::new()
            .name("fhv-font".into())
            .spawn(move || {
                let r = std::panic::catch_unwind(|| fonts::load(&c))
                    .unwrap_or_else(|_| Some(Err("the font could not be read".into())));
                let _ = tx.send(r);
                ctx2.request_repaint();
            });
        self.loading = Some((choice, rx));
    }

    /// Font size shortcuts: Ctrl+= / Ctrl+- / Ctrl+0 and Ctrl+wheel (Cmd on
    /// macOS). egui's own keyboard zoom is off (App::with).
    pub fn shortcuts(&mut self, ctx: &egui::Context, s: &mut Settings) {
        let cmd = |k| KeyboardShortcut::new(Modifiers::COMMAND, k);
        let (plus, minus, reset, zoom) = ctx.input_mut(|i| {
            (
                i.consume_shortcut(&cmd(Key::Equals)) || i.consume_shortcut(&cmd(Key::Plus)),
                i.consume_shortcut(&cmd(Key::Minus)),
                i.consume_shortcut(&cmd(Key::Num0)),
                i.zoom_delta(),
            )
        });
        let old = s.grid_font_size;
        if plus {
            s.grid_font_size = settings::clamp_size(old.floor() + 1.0);
        }
        if minus {
            s.grid_font_size = settings::clamp_size(old.ceil() - 1.0);
        }
        if reset {
            s.grid_font_size = settings::FONT_SIZE;
        }
        if zoom != 1.0 {
            let acc = self.zoom_acc.unwrap_or(old) * zoom;
            let acc = settings::clamp_size(acc);
            self.zoom_acc = Some(acc);
            s.grid_font_size = (acc * 2.0).round() / 2.0;
        } else {
            self.zoom_acc = None;
        }
        if s.grid_font_size != old {
            s.save();
        }
    }

    /// Theme entries (View > Theme, and the settings dialog): hover
    /// previews, click keeps.
    pub fn theme_picker(&mut self, ui: &mut Ui, s: &mut Settings) {
        let cur = s.theme_name();
        let names: Vec<String> = std::iter::once(theme::SYSTEM.to_string())
            .chain(theme::builtin().iter().map(|t| t.name.clone()))
            .chain(self.user_themes.iter().map(|t| t.name.clone()))
            .collect();
        for n in names {
            let label = if n == theme::SYSTEM {
                "System (follows the OS)".to_string()
            } else {
                n.clone()
            };
            let r = ui.selectable_label(cur == n, label);
            if r.hovered() {
                self.hovered = Some(n.clone());
            }
            if r.clicked() {
                s.dark = theme::resolve(&n, ui.ctx().system_theme(), &self.user_themes).dark;
                s.theme = Some(n);
                s.save();
            }
        }
        for e in &self.theme_errors {
            ui.label(RichText::new(e).color(ui.visuals().warn_fg_color).small());
        }
    }

    pub fn settings_window(&mut self, ctx: &egui::Context, s: &mut Settings) {
        if !self.settings_open {
            return;
        }
        let mut open = true;
        egui::Window::new("Settings")
            .open(&mut open)
            .default_width(420.0)
            .show(ctx, |ui| {
                egui::CollapsingHeader::new("Theme")
                    .default_open(true)
                    .show(ui, |ui| {
                        egui::ScrollArea::vertical()
                            .id_salt("themes")
                            .max_height(180.0)
                            .show(ui, |ui| self.theme_picker(ui, s));
                        let credit = &theme::current(ui.ctx()).credit;
                        if !credit.is_empty() {
                            ui.label(RichText::new(credit).small().weak());
                        }
                    });
                ui.separator();
                ui.strong("Grid font");
                self.font_picker(ui, s);
                ui.horizontal(|ui| {
                    ui.label("Size");
                    let mut v = s.grid_font_size;
                    if ui
                        .add(
                            egui::Slider::new(&mut v, settings::FONT_MIN..=settings::FONT_MAX)
                                .step_by(0.5)
                                .suffix(" pt"),
                        )
                        .on_hover_text("also Ctrl+wheel, Ctrl+= / Ctrl+- / Ctrl+0")
                        .changed()
                    {
                        s.grid_font_size = settings::clamp_size(v);
                        s.save();
                    }
                });
                let preview =
                    "2024-03-11 10:00:00.0000000  4624  C:\\Windows\\System32  0x8020000000000000";
                ui.label(
                    RichText::new(preview)
                        .font(FontId::new(s.grid_font_size, fonts::grid_family())),
                );
                ui.separator();
                ui.horizontal(|ui| {
                    ui.label("UI scale");
                    // Applied when the drag ends: zooming under the pointer
                    // would move the slider while it is dragged.
                    let mut v = self.scale_edit.unwrap_or(s.ui_scale);
                    let r = ui
                        .add(
                            egui::Slider::new(&mut v, 0.5..=3.0)
                                .step_by(0.05)
                                .custom_formatter(|v, _| format!("{:.0} %", v * 100.0)),
                        )
                        .on_hover_text("menus, panels and dialogs; the grid has its own size");
                    if r.dragged() {
                        self.scale_edit = Some(v);
                    } else if r.changed() || r.drag_stopped() {
                        self.scale_edit = None;
                        s.ui_scale = settings::clamp_scale(v);
                        s.save();
                    }
                });
                ui.separator();
                ui.horizontal(|ui| {
                    ui.label("Ctrl+C copies the selection as");
                    let before = (s.copy_format, s.copy_csv_delim);
                    egui::ComboBox::from_id_salt("copy_format")
                        .selected_text(s.copy_format.label())
                        .show_ui(ui, |ui| {
                            for f in crate::copyfmt::Format::ALL {
                                ui.selectable_value(&mut s.copy_format, f, f.label());
                            }
                        });
                    ui.label("CSV delimiter");
                    ui.radio_value(&mut s.copy_csv_delim, ',', ",");
                    ui.radio_value(&mut s.copy_csv_delim, ';', ";");
                    if before != (s.copy_format, s.copy_csv_delim) {
                        s.save();
                    }
                });
                ui.separator();
                if ui
                    .button("Reset to default")
                    .on_hover_text("Dark theme, JetBrains Mono 12.5 pt, UI scale 100 %")
                    .clicked()
                {
                    let d = Settings::default();
                    s.theme = d.theme;
                    s.dark = d.dark;
                    s.grid_font = d.grid_font;
                    s.grid_font_size = d.grid_font_size;
                    s.mono_only = d.mono_only;
                    s.ui_scale = d.ui_scale;
                    self.font_error = None;
                    s.save();
                }
            });
        if !open {
            self.settings_open = false;
        }
    }

    fn font_picker(&mut self, ui: &mut Ui, s: &mut Settings) {
        let mut pick: Option<GridFont> = None;
        ui.horizontal_wrapped(|ui| {
            for n in fonts::BUNDLED {
                let c = GridFont::Bundled(n.into());
                if ui
                    .selectable_label(s.grid_font == c, format!("{n} (bundled)"))
                    .clicked()
                {
                    pick = Some(c);
                }
            }
            if ui
                .button("Font file…")
                .on_hover_text(".ttf / .otf, checked before use")
                .clicked()
                && let Some(p) = rfd::FileDialog::new()
                    .add_filter("fonts", &["ttf", "otf", "ttc"])
                    .pick_file()
            {
                pick = Some(GridFont::File(p));
            }
        });
        ui.horizontal(|ui| {
            ui.label(format!("Current: {}", font_label(&s.grid_font)));
            if self.loading.is_some() {
                spinner(ui);
            }
        });
        if let Some(e) = &self.font_error {
            ui.colored_label(ui.visuals().error_fg_color, e);
        }
        // Installed fonts, listed once.
        if self.families.is_none() && self.catalog.is_none() {
            let (tx, rx) = channel();
            let ctx = ui.ctx().clone();
            let _ = std::thread::Builder::new()
                .name("fhv-fonts".into())
                .spawn(move || {
                    let _ = tx
                        .send(std::panic::catch_unwind(fonts::system_families).unwrap_or_default());
                    ctx.request_repaint();
                });
            self.catalog = Some(rx);
        }
        if let Some(rx) = &self.catalog
            && let Ok(f) = rx.try_recv()
        {
            self.families = Some(f);
            self.catalog = None;
        }
        ui.horizontal(|ui| {
            ui.label("Installed");
            ui.add(
                egui::TextEdit::singleline(&mut self.family_search)
                    .hint_text("search")
                    .desired_width(140.0),
            );
            if ui
                .checkbox(&mut s.mono_only, "monospaced only")
                .on_hover_text("aligned hex values and paths")
                .changed()
            {
                s.save();
            }
        });
        match &self.families {
            None => {
                ui.horizontal(|ui| {
                    spinner(ui);
                    ui.label("listing installed fonts…");
                });
            }
            Some(fams) => {
                let q = self.family_search.to_lowercase();
                let shown: Vec<&String> = fams
                    .iter()
                    .filter(|f| (!s.mono_only || f.1) && f.0.to_lowercase().contains(&q))
                    .map(|f| &f.0)
                    .collect();
                let h = ui.text_style_height(&egui::TextStyle::Body) + 4.0;
                egui::ScrollArea::vertical()
                    .id_salt("families")
                    .max_height(160.0)
                    .auto_shrink([false, true])
                    .show_rows(ui, h, shown.len(), |ui, range| {
                        for n in &shown[range] {
                            let c = GridFont::System((*n).clone());
                            if ui.selectable_label(s.grid_font == c, n.as_str()).clicked() {
                                pick = Some(c);
                            }
                        }
                    });
            }
        }
        if let Some(c) = pick {
            self.font_error = None;
            s.grid_font = c;
            s.save();
        }
    }

    #[cfg(test)]
    pub fn font_busy(&self) -> bool {
        self.loading.is_some() || self.font_applied.is_none()
    }
}

/// Row height for a grid font size (20 px at the default 12.5).
pub fn row_height(size: f32) -> f32 {
    (size * 1.6).round()
}

fn font_label(f: &GridFont) -> String {
    match f {
        GridFont::Bundled(n) | GridFont::System(n) => n.clone(),
        GridFont::File(p) => p.display().to_string(),
    }
}

/// egui's spinner asks for a repaint every frame (60-120 fps for as long
/// as it shows). This one only draws: the caller's throttled repaint
/// (D-055) moves it.
pub fn spinner(ui: &mut Ui) {
    let size = ui.spacing().interact_size.y;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let t = ui.input(|i| i.time);
    let (c, r) = (rect.center(), rect.height() / 2.5);
    let start = t * 6.0;
    let pts: Vec<egui::Pos2> = (0..=16)
        .map(|k| {
            let a = start + k as f64 / 16.0 * 4.0;
            c + r * egui::vec2(a.cos() as f32, a.sin() as f32)
        })
        .collect();
    ui.painter().add(egui::Shape::line(
        pts,
        egui::Stroke::new(2.0, ui.visuals().strong_text_color()),
    ));
}

/// Repaint delay while background work runs: 10 Hz for the progress
/// display, 1 Hz when the window is in the background or minimised (the
/// workers do not need frames; a debounced query starts on the next one).
pub fn busy_repaint(ctx: &egui::Context) -> std::time::Duration {
    let away = ctx.input(|i| !i.focused || i.viewport().minimized == Some(true));
    std::time::Duration::from_millis(if away { 1000 } else { 100 })
}
