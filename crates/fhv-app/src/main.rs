//! forhunt-viewer: a fast timeline / CSV viewer for forensic analysts.

mod fonts;
mod grid;
mod spike;

use eframe::egui;

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let rows = flag(&args, "--spike-rows").unwrap_or(100_000_000);
    let bench = flag(&args, "--bench-frames").map(|n| n as usize);
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("forhunt-viewer")
            .with_inner_size([1600.0, 1000.0]),
        ..Default::default()
    };
    let mut opts = opts;
    opts.glow_options.vsync = bench.is_none();
    eframe::run_native(
        "forhunt-viewer",
        opts,
        Box::new(move |cc| {
            fonts::install(&cc.egui_ctx);
            Ok(Box::new(spike::Spike::new(rows, bench)))
        }),
    )
}

fn flag(args: &[String], name: &str) -> Option<u64> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok())
}
