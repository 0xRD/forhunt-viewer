//! Bundled fonts: Inter (UI) and JetBrains Mono (grid), both SIL OFL 1.1,
//! unmodified upstream files (see assets/fonts/OFL-*.txt and DECISIONS.md).
//! egui's default fonts are not used: one of them is under the Ubuntu Font
//! Licence, which is outside the project's licence policy.

use std::sync::Arc;

use eframe::egui::{Context, FontData, FontDefinitions, FontFamily};

const INTER: &[u8] = include_bytes!("../assets/fonts/Inter-Regular.ttf");
const MONO: &[u8] = include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf");

pub fn install(ctx: &Context) {
    let mut fonts = FontDefinitions::empty();
    fonts
        .font_data
        .insert("inter".into(), Arc::new(FontData::from_static(INTER)));
    fonts
        .font_data
        .insert("jbmono".into(), Arc::new(FontData::from_static(MONO)));
    fonts.families.insert(
        FontFamily::Proportional,
        vec!["inter".into(), "jbmono".into()],
    );
    fonts
        .families
        .insert(FontFamily::Monospace, vec!["jbmono".into(), "inter".into()]);
    ctx.set_fonts(fonts);
    // Data can hold any character: draw a box rather than fail. (Tests keep
    // kittest's panic policy so missing glyphs in our own UI are caught.)
    if !cfg!(test) {
        ctx.set_missing_glyph_policy(eframe::egui::epaint::text::MissingGlyphPolicy::Tofu);
    }
}
