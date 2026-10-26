//! Bundled fonts: Inter (UI) and JetBrains Mono (grid), both SIL OFL 1.1,
//! unmodified upstream files (see assets/fonts/OFL-*.txt and docs/DECISIONS.md).
//! egui's default fonts are not used: one of them is under the Ubuntu Font
//! Licence, which is outside the project's licence policy.
//!
//! The grid draws with its own family ([`grid_family`]): a bundled font, an
//! installed one (enumerated with fontique, already used by eframe's
//! fallback provider) or a font file, then JetBrains Mono / Inter, then the
//! OS fallback for other scripts (D-057).

use std::sync::Arc;

use eframe::egui::{Context, FontData, FontDefinitions, FontFamily};

use crate::settings::GridFont;

const INTER: &[u8] = include_bytes!("../assets/fonts/Inter-Regular.ttf");
const MONO: &[u8] = include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf");

pub const BUNDLED: [&str; 2] = ["JetBrains Mono", "Inter"];
/// Font files above this are refused.
pub const MAX_FONT_BYTES: u64 = 64 << 20;

pub fn grid_family() -> FontFamily {
    FontFamily::Name("grid".into())
}

/// Installs the fonts; `grid` is the loaded user font (None: a bundled one).
pub fn install(ctx: &Context, choice: &GridFont, grid: Option<FontData>) {
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
    let mut chain: Vec<String> = match (choice, grid) {
        (_, Some(d)) => {
            fonts.font_data.insert("grid-user".into(), Arc::new(d));
            vec!["grid-user".into(), "jbmono".into()]
        }
        (GridFont::Bundled(n), None) if n == "Inter" => vec!["inter".into(), "jbmono".into()],
        _ => vec!["jbmono".into()],
    };
    chain.push("inter".into());
    fonts.families.insert(grid_family(), chain);
    ctx.set_fonts(fonts);
    // Data can hold any character: draw a box rather than fail. (Tests keep
    // kittest's panic policy so missing glyphs in our own UI are caught.)
    if !cfg!(test) {
        ctx.set_missing_glyph_policy(eframe::egui::epaint::text::MissingGlyphPolicy::Tofu);
    }
}

/// Checks that `bytes` is a font egui can draw digits and letters with.
pub fn validate(bytes: &[u8], index: u32) -> Result<(), String> {
    use skrifa::MetadataProvider as _;
    if bytes.len() as u64 > MAX_FONT_BYTES {
        return Err(format!(
            "font file too large ({} MiB, limit {} MiB)",
            bytes.len() >> 20,
            MAX_FONT_BYTES >> 20
        ));
    }
    let f = skrifa::FontRef::from_index(bytes, index).map_err(|e| format!("not a font: {e}"))?;
    let (cmap, outlines) = (f.charmap(), f.outline_glyphs());
    for c in ['0', 'A', 'a'] {
        let g = cmap
            .map(c)
            .ok_or_else(|| format!("the font has no glyph for {c:?}"))?;
        if outlines.get(g).is_none() {
            return Err(format!("the font cannot draw {c:?} (no outline)"));
        }
    }
    Ok(())
}

/// Reads and checks a font file (bounded size, never panics).
pub fn load_file(path: &std::path::Path) -> Result<FontData, String> {
    let len = std::fs::metadata(path).map_err(|e| e.to_string())?.len();
    if len > MAX_FONT_BYTES {
        return Err(format!(
            "font file too large ({} MiB, limit {} MiB)",
            len >> 20,
            MAX_FONT_BYTES >> 20
        ));
    }
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    validate(&bytes, 0)?;
    Ok(FontData::from_owned(bytes))
}

fn collection() -> fontique::Collection {
    fontique::Collection::new(fontique::CollectionOptions {
        shared: false,
        system_fonts: true,
    })
}

/// The regular face of an installed family.
pub fn load_system(family: &str) -> Result<FontData, String> {
    let mut c = collection();
    let fam = c
        .family_by_name(family)
        .ok_or_else(|| format!("font {family:?} is not installed"))?;
    let font = fam
        .default_font()
        .ok_or_else(|| format!("font {family:?} has no regular face"))?;
    let blob = font
        .load(None)
        .ok_or_else(|| format!("cannot read font {family:?}"))?;
    validate(blob.data(), font.index())?;
    let (data, _) = blob.into_raw_parts();
    Ok(FontData::from_blob(data, font.index()))
}

/// Loads a choice off the UI thread (None: bundled, nothing to load).
pub fn load(choice: &GridFont) -> Option<Result<FontData, String>> {
    match choice {
        GridFont::Bundled(_) => None,
        GridFont::System(f) => Some(load_system(f)),
        GridFont::File(p) => Some(load_file(p)),
    }
}

/// Installed families, sorted, with "is monospaced" (same advance for
/// `i` and `W`). Reads every family's regular face: run off the UI thread.
pub fn system_families() -> Vec<(String, bool)> {
    use skrifa::MetadataProvider as _;
    let mut c = collection();
    let names: Vec<String> = c.family_names().map(str::to_string).collect();
    let mut out: Vec<(String, bool)> = names
        .into_iter()
        .filter(|n| !n.starts_with('.'))
        .map(|n| {
            let mono = c
                .family_by_name(&n)
                .and_then(|f| f.default_font().cloned())
                .and_then(|f| {
                    let blob = f.load(None)?;
                    let r = skrifa::FontRef::from_index(blob.data(), f.index()).ok()?;
                    let m = r.glyph_metrics(
                        skrifa::instance::Size::unscaled(),
                        skrifa::instance::LocationRef::default(),
                    );
                    let cm = r.charmap();
                    let w = |ch: char| cm.map(ch).and_then(|g| m.advance_width(g));
                    Some(matches!((w('i'), w('W')), (Some(a), Some(b)) if a == b && a > 0.0))
                })
                .unwrap_or(false);
            (n, mono)
        })
        .collect();
    out.sort_by_key(|f| f.0.to_lowercase());
    out.dedup_by(|a, b| a.0 == b.0);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bad_and_huge_font_files_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let junk = dir.path().join("junk.ttf");
        std::fs::write(&junk, b"definitely not a font").unwrap();
        assert!(load_file(&junk).unwrap_err().contains("not a font"));
        // Truncated real font.
        let cut = dir.path().join("cut.ttf");
        std::fs::write(&cut, &MONO[..4096]).unwrap();
        assert!(load_file(&cut).is_err());
        let huge = dir.path().join("huge.ttf");
        std::fs::File::create(&huge)
            .unwrap()
            .set_len(MAX_FONT_BYTES + 1)
            .unwrap();
        assert!(load_file(&huge).unwrap_err().contains("too large"));
        assert!(load_file(&dir.path().join("missing.ttf")).is_err());
        let good = dir.path().join("inter.ttf");
        std::fs::write(&good, INTER).unwrap();
        assert!(load_file(&good).is_ok());
        assert!(validate(MONO, 0).is_ok());
        assert!(validate(MONO, 7).is_err());
    }
}
