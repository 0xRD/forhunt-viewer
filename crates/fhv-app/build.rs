//! Embeds the application icon in the Windows executable (Explorer, taskbar,
//! "Open with" list). Only when building on Windows for Windows: the
//! resource compiler is part of the Windows SDK. Also embeds every theme
//! file of assets/themes (D-056).

fn main() {
    themes();
    println!("cargo:rerun-if-changed=../../packaging/windows/app.rc");
    println!("cargo:rerun-if-changed=../../packaging/icons/icon.ico");
    let target = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target == "windows" && cfg!(windows) {
        embed_resource::compile("../../packaging/windows/app.rc", embed_resource::NONE)
            .manifest_optional()
            .unwrap_or_else(|e| println!("cargo:warning=icon not embedded: {e}"));
    }
}

/// `OUT_DIR/themes.rs`: `THEMES: &[(file name, JSON)]`, sorted by name.
fn themes() {
    println!("cargo:rerun-if-changed=assets/themes");
    let dir =
        std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("assets/themes");
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .map(|rd| rd.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    files.retain(|p: &std::path::PathBuf| p.extension().is_some_and(|x| x == "json"));
    files.sort();
    let mut out = String::from("pub const THEMES: &[(&str, &str)] = &[\n");
    for p in files {
        println!("cargo:rerun-if-changed={}", p.display());
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        out.push_str(&format!(
            "    ({name:?}, include_str!({:?})),\n",
            p.display().to_string()
        ));
    }
    out.push_str("];\n");
    let dst = std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("themes.rs");
    std::fs::write(dst, out).unwrap();
}
