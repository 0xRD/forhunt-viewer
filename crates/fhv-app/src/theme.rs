//! Color themes (D-056): one JSON file per theme in `assets/themes/`,
//! embedded at build time (build.rs), plus user themes from
//! `<config>/themes/*.json`. A theme sets egui's visuals and carries the
//! colors egui has no slot for (grid lines, search hits, tagged rows, rule
//! palette, duplicate / unique highlights...). Code reads them with
//! [`current`].

use std::sync::{Arc, LazyLock};

use eframe::egui::{self, Color32, Context, Id};
use serde::Deserialize;

/// Theme name that follows the OS light / dark setting.
pub const SYSTEM: &str = "System";
/// Themes used by [`SYSTEM`].
pub const SYSTEM_DARK: &str = "Dark";
pub const SYSTEM_LIGHT: &str = "Light";

/// A color written `#rrggbb` or `#rrggbbaa` (alpha not premultiplied).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rgb(pub Color32);

impl<'de> Deserialize<'de> for Rgb {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        parse_hex(&s)
            .map(Rgb)
            .ok_or_else(|| serde::de::Error::custom(format!("bad color {s:?}")))
    }
}

pub fn parse_hex(s: &str) -> Option<Color32> {
    let h = s.strip_prefix('#')?;
    if !(h.len() == 6 || h.len() == 8) || !h.is_ascii() {
        return None;
    }
    let b = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
    let a = if h.len() == 8 { b(6)? } else { 255 };
    Some(Color32::from_rgba_unmultiplied(b(0)?, b(2)?, b(4)?, a))
}

/// Text color on a background color.
#[derive(Clone, Copy, Debug, Deserialize)]
pub struct Pair {
    pub bg: Rgb,
    pub fg: Rgb,
}

/// Named colors for coloring rules.
#[allow(dead_code)] // for coloring rules (upcoming)
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rules {
    pub red: Pair,
    pub orange: Pair,
    pub yellow: Pair,
    pub green: Pair,
    pub teal: Pair,
    pub blue: Pair,
    pub purple: Pair,
    pub gray: Pair,
}

#[allow(dead_code)]
impl Rules {
    pub fn named(&self) -> [(&'static str, Pair); 8] {
        [
            ("red", self.red),
            ("orange", self.orange),
            ("yellow", self.yellow),
            ("green", self.green),
            ("teal", self.teal),
            ("blue", self.blue),
            ("purple", self.purple),
            ("gray", self.gray),
        ]
    }

    pub fn get(&self, name: &str) -> Option<Pair> {
        self.named().into_iter().find(|n| n.0 == name).map(|n| n.1)
    }
}

// Slots for coloring rules, duplicate / unique values, range selection and
// the detail pane are read by features that build on this module.
#[allow(dead_code)]
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Theme {
    pub name: String,
    pub dark: bool,
    /// Checked against WCAG AAA (7:1) instead of AA (4.5:1).
    #[serde(default)]
    pub high_contrast: bool,
    /// Palette origin and licence (also in THIRD-PARTY.md).
    #[serde(default)]
    pub credit: String,
    /// Panels.
    pub bg: Rgb,
    /// Windows, menus, tooltips.
    pub bg_window: Rgb,
    /// Text fields, filter row, scrollbar track.
    pub bg_input: Rgb,
    /// Every other grid row.
    pub bg_stripe: Rgb,
    /// Grid header.
    pub bg_header: Rgb,
    pub text: Rgb,
    pub text_strong: Rgb,
    pub text_weak: Rgb,
    /// Links, frozen-column edge.
    pub accent: Rgb,
    /// Buttons at rest, hovered, pressed.
    pub widget: Rgb,
    pub hover: Rgb,
    pub active: Rgb,
    pub border: Rgb,
    pub grid_line: Rgb,
    /// Selected row (and selected text, selected toggles).
    pub selection: Rgb,
    /// Behind search matches in cells (text stays `text`).
    pub search_hit: Rgb,
    /// Tagged rows.
    pub tagged: Rgb,
    /// Filter chips, and inverted (NOT) ones.
    pub chip: Pair,
    pub chip_negated: Pair,
    pub error: Rgb,
    pub warning: Rgb,
    /// Outline of the selected cell.
    pub active_cell: Rgb,
    /// Selected range of cells (may be translucent).
    pub range_fill: Rgb,
    pub range_outline: Rgb,
    /// Duplicate / unique value highlighting.
    pub duplicate: Pair,
    pub unique: Pair,
    /// Highlighted field in the detail pane.
    pub detail_highlight: Pair,
    /// Group header rows of the grid (absent in older user themes: the
    /// chip colors).
    #[serde(default)]
    pub group_header: Option<Pair>,
    /// Coloring-rule palette.
    pub rules: Rules,
}

mod embedded {
    include!(concat!(env!("OUT_DIR"), "/themes.rs"));
}

fn parse(src: &str) -> Result<Theme, String> {
    serde_json::from_str(src).map_err(|e| e.to_string())
}

static BUILTIN: LazyLock<Vec<Theme>> = LazyLock::new(|| {
    let mut v: Vec<Theme> = embedded::THEMES
        .iter()
        .filter_map(|(_, src)| parse(src).ok())
        .collect();
    // Dark and light families, each from the plainest to the most specific.
    let rank = |t: &Theme| {
        let pos = [
            "Dark",
            "Dark soft",
            "Dark high contrast",
            "Light",
            "Light high contrast",
        ]
        .iter()
        .position(|n| *n == t.name)
        .unwrap_or(99);
        (!t.dark, pos)
    };
    v.sort_by(|a, b| rank(a).cmp(&rank(b)).then_with(|| a.name.cmp(&b.name)));
    v
});

/// Built-in themes, in menu order.
pub fn builtin() -> &'static [Theme] {
    &BUILTIN
}

/// Every embedded file parses (tests).
#[cfg(test)]
pub fn embedded_errors() -> Vec<String> {
    embedded::THEMES
        .iter()
        .filter_map(|(f, src)| parse(src).err().map(|e| format!("{f}: {e}")))
        .collect()
}

/// User themes in `dir/*.json` (a name already taken is skipped), and the
/// files that could not be read.
pub fn load_dir(dir: &std::path::Path) -> (Vec<Theme>, Vec<String>) {
    let (mut themes, mut errors) = (Vec::new(), Vec::new());
    let Ok(rd) = std::fs::read_dir(dir) else {
        return (themes, errors);
    };
    let mut paths: Vec<_> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    paths.sort();
    for p in paths {
        let r = std::fs::metadata(&p)
            .map_err(|e| e.to_string())
            .and_then(|m| {
                if m.len() > 1 << 20 {
                    Err("larger than 1 MiB".into())
                } else {
                    std::fs::read_to_string(&p).map_err(|e| e.to_string())
                }
            })
            .and_then(|s| parse(&s));
        match r {
            Ok(t) if t.name == SYSTEM || builtin().iter().any(|b| b.name == t.name) => {
                errors.push(format!("{}: name {:?} is taken", p.display(), t.name))
            }
            Ok(t) => themes.push(t),
            Err(e) => errors.push(format!("{}: {e}", p.display())),
        }
    }
    (themes, errors)
}

/// The theme a preference stands for (`System` follows the OS; unknown
/// names fall back to Dark).
pub fn resolve<'a>(pref: &str, system: Option<egui::Theme>, user: &'a [Theme]) -> &'a Theme
where
    'static: 'a,
{
    let name = match (pref, system) {
        (SYSTEM, Some(egui::Theme::Light)) => SYSTEM_LIGHT,
        (SYSTEM, _) => SYSTEM_DARK,
        (n, _) => n,
    };
    builtin()
        .iter()
        .chain(user)
        .find(|t| t.name == name)
        .or_else(|| builtin().iter().find(|t| t.name == SYSTEM_DARK))
        .unwrap_or(&builtin()[0])
}

impl Theme {
    pub fn visuals(&self) -> egui::Visuals {
        let mut v = if self.dark {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        };
        v.override_text_color = None;
        v.panel_fill = self.bg.0;
        v.window_fill = self.bg_window.0;
        v.window_stroke.color = self.border.0;
        v.extreme_bg_color = self.bg_input.0;
        v.text_edit_bg_color = Some(self.bg_input.0);
        v.code_bg_color = self.bg_input.0;
        v.faint_bg_color = self.bg_stripe.0;
        v.hyperlink_color = self.accent.0;
        v.warn_fg_color = self.warning.0;
        v.error_fg_color = self.error.0;
        v.weak_text_color = Some(self.text_weak.0);
        v.selection.bg_fill = self.selection.0;
        v.selection.stroke.color = self.text_strong.0;
        v.text_cursor.stroke.color = self.text_strong.0;
        // A blinking cursor repaints twice a second while a field has focus.
        v.text_cursor.blink = false;
        let w = &mut v.widgets;
        w.noninteractive.bg_fill = self.bg_header.0;
        w.noninteractive.weak_bg_fill = self.bg_header.0;
        w.noninteractive.bg_stroke.color = self.border.0;
        w.noninteractive.fg_stroke.color = self.text.0;
        for (s, bg, fg) in [
            (&mut w.inactive, self.widget, self.text),
            (&mut w.hovered, self.hover, self.text_strong),
            (&mut w.active, self.active, self.text_strong),
            (&mut w.open, self.hover, self.text_strong),
        ] {
            s.bg_fill = bg.0;
            s.weak_bg_fill = bg.0;
            s.fg_stroke.color = fg.0;
        }
        w.hovered.bg_stroke.color = self.border.0;
        w.active.bg_stroke.color = self.accent.0;
        if self.high_contrast {
            w.inactive.bg_stroke = egui::Stroke::new(1.0, self.border.0);
        }
        v
    }

    /// Text / background pairs the UI draws, for the contrast check.
    #[cfg(test)]
    pub fn text_pairs(&self) -> Vec<(String, Color32, Color32)> {
        let mut out = Vec::new();
        let mut add = |what: &str, fg: Rgb, bg: Color32| out.push((what.to_string(), fg.0, bg));
        for (n, bg) in [
            ("bg", self.bg),
            ("bg_window", self.bg_window),
            ("bg_input", self.bg_input),
            ("bg_stripe", self.bg_stripe),
            ("widget", self.widget),
            ("hover", self.hover),
            ("selection", self.selection),
            ("search_hit", self.search_hit),
            ("tagged", self.tagged),
        ] {
            add(&format!("text on {n}"), self.text, bg.0);
        }
        for (n, bg) in [
            ("bg_header", self.bg_header),
            ("bg", self.bg),
            ("widget", self.widget),
            ("hover", self.hover),
            ("active", self.active),
            ("selection", self.selection),
        ] {
            add(&format!("text_strong on {n}"), self.text_strong, bg.0);
        }
        for (n, fg) in [
            ("text_weak", self.text_weak),
            ("accent", self.accent),
            ("error", self.error),
            ("warning", self.warning),
        ] {
            add(&format!("{n} on bg"), fg, self.bg.0);
            add(&format!("{n} on bg_window"), fg, self.bg_window.0);
        }
        add("warning on chip", self.warning, self.chip.bg.0);
        add(
            "warning on chip_negated",
            self.warning,
            self.chip_negated.bg.0,
        );
        add(
            "text on range_fill",
            self.text,
            over(self.range_fill.0, self.bg.0),
        );
        for (n, p) in [
            ("chip", self.chip),
            ("chip_negated", self.chip_negated),
            ("duplicate", self.duplicate),
            ("unique", self.unique),
            ("detail_highlight", self.detail_highlight),
            ("group_header", self.group_header.unwrap_or(self.chip)),
        ]
        .into_iter()
        .chain(self.rules.named())
        {
            add(n, p.fg, p.bg.0);
        }
        out
    }
}

/// `fg` (unmultiplied alpha) drawn over an opaque `bg`.
#[allow(dead_code)]
pub fn over(fg: Color32, bg: Color32) -> Color32 {
    let [r, g, b, a] = fg.to_srgba_unmultiplied();
    let a = a as f32 / 255.0;
    let mix = |x: u8, y: u8| (x as f32 * a + y as f32 * (1.0 - a)).round() as u8;
    Color32::from_rgb(mix(r, bg.r()), mix(g, bg.g()), mix(b, bg.b()))
}

/// WCAG 2 contrast ratio of two opaque colors (1 to 21).
#[allow(dead_code)]
pub fn contrast(a: Color32, b: Color32) -> f32 {
    fn lum(c: Color32) -> f32 {
        let ch = |v: u8| {
            let v = v as f32 / 255.0;
            if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * ch(c.r()) + 0.7152 * ch(c.g()) + 0.0722 * ch(c.b())
    }
    let (x, y) = (lum(a), lum(b));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

fn key() -> Id {
    Id::unique("fhv-theme")
}

/// Installs a theme: egui visuals, and the colors [`current`] returns.
pub fn apply(ctx: &Context, t: &Theme) {
    ctx.set_theme(if t.dark {
        egui::Theme::Dark
    } else {
        egui::Theme::Light
    });
    ctx.set_visuals(t.visuals());
    let t = Arc::new(t.clone());
    ctx.data_mut(|d| d.insert_temp(key(), t));
}

/// The theme in use (Dark before any [`apply`]).
pub fn current(ctx: &Context) -> Arc<Theme> {
    ctx.data(|d| d.get_temp::<Arc<Theme>>(key()))
        .unwrap_or_else(|| Arc::new(resolve(SYSTEM_DARK, None, &[]).clone()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_themes_parse_and_are_unique() {
        assert_eq!(embedded_errors(), Vec::<String>::new());
        let names: Vec<&str> = builtin().iter().map(|t| t.name.as_str()).collect();
        for n in [
            "Dark",
            "Dark soft",
            "Dark high contrast",
            "Dracula",
            "Light",
            "Light high contrast",
            "Solarized dark",
            "Solarized light",
            "Nord",
            "Gruvbox dark",
        ] {
            assert!(names.contains(&n), "{n} missing from {names:?}");
        }
        let mut u = names.clone();
        u.sort();
        u.dedup();
        assert_eq!(u.len(), names.len());
        assert_eq!(names[0], "Dark");
    }

    /// WCAG 2.1: AA (4.5:1) for normal text in every theme, AAA (7:1) in
    /// the high-contrast ones.
    #[test]
    fn text_contrast_meets_wcag() {
        let mut bad = Vec::new();
        for t in builtin() {
            let need = if t.high_contrast { 7.0 } else { 4.5 };
            for (what, fg, bg) in t.text_pairs() {
                let c = contrast(fg, bg);
                if c < need {
                    bad.push(format!("{}: {what} = {c:.2} < {need}", t.name));
                }
            }
        }
        assert!(bad.is_empty(), "{bad:#?}");
        assert!(builtin().iter().filter(|t| t.high_contrast).count() >= 2);
    }

    #[test]
    fn contrast_reference_values() {
        assert!((contrast(Color32::BLACK, Color32::WHITE) - 21.0).abs() < 0.01);
        assert!((contrast(Color32::WHITE, Color32::WHITE) - 1.0).abs() < 1e-6);
        // #777777 on white is the classic 4.48:1.
        let c = contrast(Color32::from_gray(0x77), Color32::WHITE);
        assert!((c - 4.48).abs() < 0.01, "{c}");
        assert_eq!(
            over(
                Color32::from_rgba_unmultiplied(255, 255, 255, 128),
                Color32::BLACK
            ),
            Color32::from_gray(128)
        );
    }

    #[test]
    fn colors_and_system_resolution() {
        assert_eq!(parse_hex("#ff8000"), Some(Color32::from_rgb(255, 128, 0)));
        assert_eq!(
            parse_hex("#ff800080"),
            Some(Color32::from_rgba_unmultiplied(255, 128, 0, 128))
        );
        assert_eq!(parse_hex("ff8000"), None);
        assert_eq!(parse_hex("#ff80"), None);
        assert_eq!(parse_hex("#gg8000"), None);
        assert_eq!(resolve(SYSTEM, Some(egui::Theme::Light), &[]).name, "Light");
        assert_eq!(resolve(SYSTEM, Some(egui::Theme::Dark), &[]).name, "Dark");
        assert_eq!(resolve(SYSTEM, None, &[]).name, "Dark");
        assert_eq!(resolve("no such theme", None, &[]).name, "Dark");
        assert_eq!(resolve("Nord", None, &[]).name, "Nord");
    }

    #[test]
    fn user_themes_load_and_bad_files_are_reported() {
        let dir = tempfile::tempdir().unwrap();
        let mut mine: serde_json::Value = serde_json::from_str(embedded::THEMES[0].1).unwrap();
        mine["name"] = "Mine".into();
        std::fs::write(dir.path().join("mine.json"), mine.to_string()).unwrap();
        std::fs::write(dir.path().join("broken.json"), "{ not json").unwrap();
        mine["name"] = "Dark".into();
        std::fs::write(dir.path().join("taken.json"), mine.to_string()).unwrap();
        std::fs::write(dir.path().join("notes.txt"), "ignored").unwrap();
        let (themes, errors) = load_dir(dir.path());
        assert_eq!(themes.len(), 1);
        assert_eq!(themes[0].name, "Mine");
        assert_eq!(errors.len(), 2, "{errors:?}");
        assert_eq!(resolve("Mine", None, &themes).name, "Mine");
        assert!(load_dir(&dir.path().join("missing")).0.is_empty());
    }
}
