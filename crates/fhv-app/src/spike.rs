//! Phase 1 spike: the virtual grid over N synthetic rows, with an automated
//! scrolling benchmark (`--bench-frames N`) that reports frame CPU time.

use std::fmt::Write as _;
use std::ops::Range;
use std::time::Instant;

use eframe::egui;

use crate::grid::{self, GridSource, GridState, Style};

const COLS: &[&str] = &[
    "RecordNumber",
    "TimeCreated",
    "EventId",
    "Level",
    "Channel",
    "Computer",
    "UserName",
    "RemoteHost",
    "PayloadData1",
    "PayloadData2",
    "ExecutableInfo",
    "Payload",
    "ForhuntId",
];

/// Synthesises cells from the row number, formatting them per frame like a
/// real source decoding rows from the file would.
pub struct Synth {
    rows: u64,
    first: u64,
    cells: Vec<String>,
}

impl GridSource for Synth {
    fn row_count(&self) -> u64 {
        self.rows
    }
    fn column_count(&self) -> usize {
        COLS.len()
    }
    fn column_name(&self, col: usize) -> &str {
        COLS[col]
    }
    fn prepare(&mut self, rows: Range<u64>) {
        self.first = rows.start;
        let n = (rows.end - rows.start) as usize * COLS.len();
        self.cells.resize(n, String::new());
        for (k, r) in rows.enumerate() {
            let h = r.wrapping_mul(0x9E37_79B9_7F4A_7C15);
            for c in 0..COLS.len() {
                let s = &mut self.cells[k * COLS.len() + c];
                s.clear();
                let _ = match c {
                    0 => write!(s, "{r}"),
                    1 => write!(
                        s,
                        "2024-03-{:02} {:02}:{:02}:{:02}.{:07}",
                        1 + r % 28,
                        h % 24,
                        h >> 8 & 59,
                        h >> 16 & 59,
                        h % 10_000_000
                    ),
                    2 => write!(s, "{}", [4624, 4625, 4688, 4634, 7045][(h % 5) as usize]),
                    3 => write!(s, "Info"),
                    4 => write!(s, "Security"),
                    5 => write!(s, "WS-{:02}.corp.example", h % 40),
                    6 => write!(s, "user{}", h % 300),
                    7 => write!(s, "192.0.2.{}", h % 250),
                    8 => write!(s, "TargetUserName: user{}", h % 300),
                    9 => write!(s, "LogonType: {}", h % 11),
                    10 => write!(s, r"C:\Windows\System32\cmd.exe /c whoami /all {h:x}"),
                    11 => write!(
                        s,
                        r#"{{"TargetUserName":"user{}","LogonType":"3","IpAddress":"192.0.2.{}","Line":"a\nb"}}"#,
                        h % 300,
                        h % 250
                    ),
                    _ => write!(s, "{:08x}:{r}", h as u32),
                };
            }
        }
    }
    fn cell(&self, row: u64, col: usize) -> &str {
        &self.cells[(row - self.first) as usize * COLS.len() + col]
    }
}

pub struct Spike {
    src: Synth,
    grid: GridState,
    style: Style,
    bench: Option<usize>,
    frame: usize,
    ui_times: Vec<f64>,
    cpu_times: Vec<f64>,
}

impl Spike {
    pub fn new(rows: u64, bench: Option<usize>) -> Self {
        Spike {
            src: Synth {
                rows,
                first: 0,
                cells: Vec::new(),
            },
            grid: GridState::default(),
            style: Style::default(),
            bench,
            frame: 0,
            ui_times: Vec::new(),
            cpu_times: Vec::new(),
        }
    }
}

fn pct(v: &mut [f64], p: f64) -> f64 {
    v.sort_by(f64::total_cmp);
    v[((v.len() - 1) as f64 * p) as usize]
}

impl eframe::App for Spike {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let t = Instant::now();
        if let Some(total) = self.bench {
            // Alternate: smooth scroll (3 rows/frame), then big jumps
            // (scrollbar drags), across the whole range.
            self.frame += 1;
            if self.frame % 50 < 40 {
                self.grid.top += 3.0;
            } else {
                self.grid.top = (self.frame as f64 * 7_919_993.0) % self.src.rows as f64;
            }
            if self.frame > 10
                && let Some(c) = frame.info().cpu_usage
            {
                self.cpu_times.push(c as f64 * 1e3);
            }
            if self.frame == total {
                let n = self.ui_times.len();
                println!(
                    "frames={n} rows={} ui_ms p50={:.3} p99={:.3} max={:.3} | cpu_usage_ms p50={:.3} p99={:.3} max={:.3}",
                    self.src.rows,
                    pct(&mut self.ui_times, 0.5),
                    pct(&mut self.ui_times, 0.99),
                    pct(&mut self.ui_times, 1.0),
                    pct(&mut self.cpu_times, 0.5),
                    pct(&mut self.cpu_times, 0.99),
                    pct(&mut self.cpu_times, 1.0),
                );
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
            }
            ui.ctx().request_repaint();
        }
        egui::Panel::top("top").show(ui, |ui| {
            ui.label(format!(
                "spike: {} rows | top {:.1} | selected {:?}",
                self.src.rows, self.grid.top, self.grid.selected
            ));
        });
        egui::CentralPanel::default().show(ui, |ui| {
            grid::show(
                ui,
                egui::Id::unique("grid"),
                &mut self.grid,
                &mut self.src,
                &self.style,
            );
        });
        if self.bench.is_some() && self.frame > 10 {
            self.ui_times.push(t.elapsed().as_secs_f64() * 1e3);
        }
    }
}
