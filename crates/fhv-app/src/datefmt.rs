//! Column menu "Date format…": as detected, not a date, a preset or a
//! custom `strftime`-style format, with the zone of values without an
//! offset and the year of year-less dates, and a live preview (D-051).

use std::path::PathBuf;

use eframe::egui::{self, RichText};
use fhv_engine::columns::Kind;
use fhv_engine::time::{self, Fmt, Hint, Order, Pattern, Shape, Zone};

use crate::tab::Tab;

/// Values shown in the preview.
pub const PREVIEW: usize = 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Auto,
    NotDate,
    Preset,
    Custom,
}

pub struct DateFmtDlg {
    pub path: PathBuf,
    pub col: usize,
    pub mode: Mode,
    /// Index in `Shape::PRESETS`.
    pub preset: usize,
    pub order: Order,
    pub pattern: String,
    pub zone: String,
    pub year: String,
    /// First non-blank values of the column.
    pub sample: Vec<String>,
    /// "Learn more" was clicked: the app shows the documentation.
    pub want_docs: bool,
}

impl DateFmtDlg {
    /// Starts from the column's current kind (override, else as detected).
    pub fn new(t: &Tab, col: usize) -> DateFmtDlg {
        let detected = t.detected_kind(col);
        let cur = t.engine.kind_override(col);
        let f = match (cur, detected) {
            (Some(Kind::Time(f)), _) | (None, Kind::Time(f)) => f,
            _ => Fmt::default(),
        };
        let mode = match cur {
            None => Mode::Auto,
            Some(Kind::Time(Fmt {
                shape: Shape::Custom(_),
                ..
            })) => Mode::Custom,
            Some(Kind::Time(_)) => Mode::Preset,
            Some(_) => Mode::NotDate,
        };
        let mut sample = Vec::new();
        let n = t.table.row_count().min(2000);
        for r in 0..n {
            if sample.len() >= PREVIEW {
                break;
            }
            if let Ok(f) = t.table.row_fields(r)
                && let Some(v) = f.get(col)
                && !v.trim().is_empty()
            {
                sample.push(v.clone());
            }
        }
        DateFmtDlg {
            path: t.path.clone(),
            col,
            mode,
            preset: Shape::PRESETS
                .iter()
                .position(|p| p.0 == f.shape)
                .unwrap_or(0),
            order: f.order,
            pattern: match f.shape {
                Shape::Custom(p) => p.src.clone(),
                _ => String::new(),
            },
            zone: f.zone.map_or_else(|| "UTC".into(), |z| z.name.clone()),
            year: if f.year != 0 {
                f.year.to_string()
            } else {
                String::new()
            },
            sample,
            want_docs: false,
        }
    }

    /// The kind to set (None: as detected).
    pub fn kind(&self, t: &Tab) -> Result<Option<Kind>, String> {
        let detected = t.detected_kind(self.col);
        let base = match detected {
            Kind::Time(f) => f,
            _ => Fmt::default(),
        };
        let shape = match self.mode {
            Mode::NotDate => {
                let numeric = t
                    .table
                    .column_index()
                    .and_then(|ci| ci.cols.get(self.col).map(|c| c.numeric))
                    .unwrap_or(false);
                return Ok(Some(if numeric { Kind::Number } else { Kind::Text }));
            }
            Mode::Auto => base.shape,
            Mode::Preset => Shape::PRESETS[self.preset.min(Shape::PRESETS.len() - 1)].0,
            Mode::Custom => Shape::Custom(Pattern::intern(self.pattern.trim())?),
        };
        let y = self.year.trim();
        let year: i32 = if y.is_empty() {
            0
        } else {
            y.parse()
                .ok()
                .filter(|y| (1..=9999).contains(y))
                .ok_or_else(|| format!("year: {y:?} is not a year"))?
        };
        let keep_roll = year == base.year;
        let f = Fmt {
            shape,
            order: self.order,
            hint: if shape == Shape::Text && base.shape == Shape::Text {
                base.hint
            } else {
                Hint::Iso
            },
            year,
            roll: if keep_roll { base.roll } else { 12 },
            zone: Zone::intern(&self.zone)?,
        };
        Ok(match (self.mode, detected) {
            (Mode::Auto, Kind::Time(d)) if d == f => None,
            (Mode::Auto, k) if !matches!(k, Kind::Time(_)) => None,
            _ => Some(Kind::Time(f)),
        })
    }
}

/// What each mode does, under the mode buttons (#19).
fn mode_help(m: Mode) -> &'static str {
    match m {
        Mode::Auto => {
            "Auto: the format detected from the values (ISO 8601, numeric d/M or M/d, \
             month names, syslog, epochs...). Keep it unless the preview shows values \
             that are not dates."
        }
        Mode::NotDate => {
            "Not a date: the column is read as text, or numbers if it holds numbers \
             (no date filters or date sort). For ids or counters taken for epochs."
        }
        Mode::Preset => {
            "Preset: a known shape, e.g. an epoch unit the header does not name \
             (Excel serial, Cocoa, Unix days) or another one than detected."
        }
        Mode::Custom => {
            "Custom: a strftime pattern matched against the whole cell, for any \
             layout the presets miss (two-digit years, odd separators)."
        }
    }
}

/// Examples of custom patterns: (cell, pattern).
const EXAMPLES: [(&str, &str); 4] = [
    ("01/03/24 14:05", "%d/%m/%y %H:%M"),
    ("2024.03.01-14h05m09", "%Y.%m.%d-%Hh%Mm%S"),
    (
        "Mar 1 2024 2:05:09.123 PM +0100",
        "%b %d %Y %I:%M:%S%.f %p %z",
    ),
    ("2024-061 14:05", "%Y-%j %H:%M"),
];

/// The dialog; returns false when it is closed (applied or cancelled).
pub fn show(dlg: &mut DateFmtDlg, t: &mut Tab, ctx: &egui::Context) -> bool {
    let mut open = true;
    let mut apply = false;
    let mut cancel = false;
    let detected = match t.detected_kind(dlg.col) {
        Kind::Time(f) => f.describe(),
        _ => "not a date".into(),
    };
    egui::Window::new(format!("Date format: {}", t.table.columns[dlg.col]))
        .id(egui::Id::unique(("date-format", dlg.col)))
        .open(&mut open)
        .collapsible(false)
        .default_width(460.0)
        .show(ctx, |ui| {
            ui.label(RichText::new(format!("Detected: {detected}")).weak());
            ui.horizontal_wrapped(|ui| {
                ui.radio_value(&mut dlg.mode, Mode::Auto, "Auto (as detected)");
                ui.radio_value(&mut dlg.mode, Mode::NotDate, "Not a date");
                ui.radio_value(&mut dlg.mode, Mode::Preset, "Preset");
                ui.radio_value(&mut dlg.mode, Mode::Custom, "Custom format");
            });
            ui.label(RichText::new(mode_help(dlg.mode)).small().weak());
            if dlg.mode == Mode::Preset {
                egui::ComboBox::from_id_salt(("date-preset", dlg.col))
                    .width(380.0)
                    .selected_text(Shape::PRESETS[dlg.preset].1)
                    .show_ui(ui, |ui| {
                        for (i, p) in Shape::PRESETS.iter().enumerate() {
                            ui.selectable_value(&mut dlg.preset, i, p.1);
                        }
                    });
            }
            if dlg.mode == Mode::Custom {
                ui.add(
                    egui::TextEdit::singleline(&mut dlg.pattern)
                        .hint_text("%d/%m/%Y %H:%M:%S")
                        .desired_width(f32::INFINITY),
                );
                ui.label(
                    RichText::new(
                        "%Y %y %m %b %B %d %e %j %H %I %M %S %f %.f %3f %p %z %Z %a %s %T %F, \
                         spaces match any run of spaces",
                    )
                    .small()
                    .weak(),
                );
                egui::CollapsingHeader::new("Cheat sheet and examples")
                    .id_salt(("date-cheat", dlg.col))
                    .show(ui, |ui| {
                        egui::Grid::new(("date-specs", dlg.col))
                            .num_columns(2)
                            .striped(true)
                            .show(ui, |ui| {
                                for (k, v) in [
                                    ("%Y  %y", "year 2024; two digits 24 (00-69 = 20xx)"),
                                    ("%m  %b  %B", "month 03; Mar / mars; March"),
                                    ("%d  %e", "day 01; day with a leading space"),
                                    ("%j", "day of the year 1-366"),
                                    ("%H  %I  %p", "hour 0-23; hour 1-12 with AM / PM"),
                                    ("%M  %S", "minute, second"),
                                    ("%f  %.f  %3f", "fraction digits; optional .123; exactly 3"),
                                    ("%z  %Z", "+01:00 / +0100 / Z; UTC, GMT, CET..."),
                                    ("%a", "weekday name (ignored)"),
                                    ("%s", "Unix seconds"),
                                    ("%T  %F", "%H:%M:%S; %Y-%m-%d"),
                                ] {
                                    ui.monospace(k);
                                    ui.label(v);
                                    ui.end_row();
                                }
                            });
                        ui.label(RichText::new("Examples (click to use)").strong());
                        for (cell, pat) in EXAMPLES {
                            ui.horizontal(|ui| {
                                ui.monospace(cell);
                                ui.label("→");
                                if ui.link(RichText::new(pat).monospace()).clicked() {
                                    dlg.pattern = pat.to_string();
                                }
                            });
                        }
                    });
            }
            if dlg.mode != Mode::NotDate {
                egui::Grid::new(("date-opts", dlg.col))
                    .num_columns(2)
                    .show(ui, |ui| {
                        ui.label("d/M or M/d");
                        ui.horizontal(|ui| {
                            ui.radio_value(&mut dlg.order, Order::Ymd, "unsure (M/d)");
                            ui.radio_value(&mut dlg.order, Order::Dmy, "day first");
                            ui.radio_value(&mut dlg.order, Order::Mdy, "month first");
                        });
                        ui.end_row();
                        ui.label("Zone of times without offset").on_hover_text(
                            "the zone the file's times were written in when they carry no \
                             offset (Z, +02:00): UTC, an IANA name with DST (Europe/Paris) or \
                             a fixed offset (+05:30). Times with an offset and epochs are \
                             never shifted. Not the display zone (View > Time zone), which only \
                             changes how times are shown",
                        );
                        ui.add(
                            egui::TextEdit::singleline(&mut dlg.zone)
                                .hint_text("UTC, Europe/Paris, +02:00")
                                .desired_width(200.0),
                        );
                        ui.end_row();
                        ui.label("Year of dates without one").on_hover_text(
                            "syslog (Mar  1 12:34:56) and logcat (03-01 12:34:56) write no \
                             year: by default it is the year of the file's last change, the \
                             months after that change going to the year before; the header \
                             then shows (year?). Set it here when you know it",
                        );
                        ui.add(
                            egui::TextEdit::singleline(&mut dlg.year)
                                .hint_text("e.g. 2024")
                                .desired_width(80.0),
                        );
                        ui.end_row();
                    });
            }
            ui.separator();
            let kind = dlg.kind(t);
            match &kind {
                Err(e) => {
                    ui.colored_label(ui.visuals().warn_fg_color, e);
                }
                Ok(k) => {
                    let k = k.unwrap_or_else(|| t.detected_kind(dlg.col));
                    preview(ui, dlg, k);
                }
            }
            ui.horizontal(|ui| {
                apply = ui
                    .add_enabled(kind.is_ok(), egui::Button::new("Apply format"))
                    .clicked();
                cancel = ui.button("Cancel").clicked();
                if ui
                    .link("Learn more")
                    .on_hover_text("date formats documentation (Settings window)")
                    .clicked()
                {
                    dlg.want_docs = true;
                }
            });
            if apply && let Ok(k) = kind {
                t.set_kind(dlg.col, k);
            }
        });
    open && !apply && !cancel
}

fn preview(ui: &mut egui::Ui, dlg: &DateFmtDlg, k: Kind) {
    let Kind::Time(f) = k else {
        ui.label(RichText::new("Read as text / numbers: no date filters or date sort.").weak());
        return;
    };
    let bad = dlg
        .sample
        .iter()
        .filter(|v| time::parse(v.as_bytes(), f).is_none())
        .count();
    let text = format!(
        "Preview of the first {} values: {bad} not dates",
        dlg.sample.len()
    );
    ui.label(if bad > 0 {
        RichText::new(text).color(ui.visuals().warn_fg_color)
    } else {
        RichText::new(text)
    });
    if bad > 0 {
        ui.label(
            RichText::new(
                "To fix: compare a value marked \"not a date\" with the format; try \
                 the other day/month order, a preset, or a custom pattern (cheat \
                 sheet). Blank-like values (-, N/A) can stay: they are counted as \
                 excluded cells, not as dates.",
            )
            .small()
            .weak(),
        );
    }
    egui::ScrollArea::vertical()
        .max_height(150.0)
        .show(ui, |ui| {
            egui::Grid::new(("date-preview", dlg.col))
                .num_columns(2)
                .striped(true)
                .show(ui, |ui| {
                    for v in &dlg.sample {
                        ui.monospace(crate::grid::display_text(v));
                        match time::parse(v.as_bytes(), f) {
                            Some(t) => ui.monospace(format!("{}Z", time::format(t, 0))),
                            None => ui.colored_label(ui.visuals().warn_fg_color, "not a date"),
                        };
                        ui.end_row();
                    }
                });
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The examples of the cheat sheet parse their cell.
    #[test]
    fn cheat_sheet_examples_parse() {
        for (cell, pat) in EXAMPLES {
            let f = Fmt {
                shape: Shape::Custom(Pattern::intern(pat).unwrap()),
                ..Fmt::default()
            };
            assert!(
                time::parse(cell.as_bytes(), f).is_some(),
                "{pat} does not read {cell}"
            );
        }
    }
}
