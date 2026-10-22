//! Embeds the application icon in the Windows executable (Explorer, taskbar,
//! "Open with" list). Only when building on Windows for Windows: the
//! resource compiler is part of the Windows SDK.

fn main() {
    println!("cargo:rerun-if-changed=../../packaging/windows/app.rc");
    println!("cargo:rerun-if-changed=../../packaging/icons/icon.ico");
    let target = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target == "windows" && cfg!(windows) {
        embed_resource::compile("../../packaging/windows/app.rc", embed_resource::NONE)
            .manifest_optional()
            .unwrap_or_else(|e| println!("cargo:warning=icon not embedded: {e}"));
    }
}
