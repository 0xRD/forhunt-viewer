//! forhunt-viewer: a fast timeline / CSV viewer for forensic analysts.
//!
//! ```text
//! forhunt-viewer [FILE...]
//! ```

mod app;
mod fonts;
mod grid;
mod session;
mod settings;
mod spike;
mod tab;
mod tz;
#[cfg(test)]
mod ui_tests;

use std::path::PathBuf;

use eframe::egui;

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("forhunt-viewer {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!(
            "forhunt-viewer {}: fast timeline / CSV viewer\n\nusage: forhunt-viewer [FILE...]\n\nFiles are opened read-only; indexes are cached in the OS cache directory.",
            env!("CARGO_PKG_VERSION")
        );
        return Ok(());
    }
    // Hidden: the phase 1 grid benchmark (docs/PERF.md).
    let spike = flag(&args, "--spike-rows");
    let bench = flag(&args, "--bench-frames").map(|n| n as usize);
    let files: Vec<PathBuf> = args
        .iter()
        .enumerate()
        .filter(|(i, a)| !a.starts_with("--") && !(*i > 0 && args[i - 1].starts_with("--")))
        .map(|(_, a)| PathBuf::from(a))
        .collect();
    let mut opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("forhunt-viewer")
            .with_inner_size([1500.0, 900.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };
    opts.glow_options.vsync = bench.is_none() && std::env::var_os("FHV_BENCH_SCROLL").is_none();
    eframe::run_native(
        "forhunt-viewer",
        opts,
        Box::new(move |cc| match spike {
            Some(rows) => {
                fonts::install(&cc.egui_ctx);
                Ok(Box::new(spike::Spike::new(rows, bench)) as Box<dyn eframe::App>)
            }
            None => Ok(Box::new(app::App::new(cc, files))),
        }),
    )
}

fn flag(args: &[String], name: &str) -> Option<u64> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok())
}
