//! UI tests for issues #33 to #36: reopening the detail pane, auto-size to
//! the longest cell, the horizontal scrollbar and widening past the edge,
//! cut cells.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;

use crate::app::App;
use crate::settings::Settings;

/// 3,000 rows; row 1237 (not among the auto-size sample) holds a long
/// Message, row 0 a medium one that does not fit the default width.
fn sample(dir: &Path) -> PathBuf {
    let p = dir.join("events.csv");
    let mut s = String::from("Time,EventId,User,Message\n");
    for i in 0..3000u32 {
        let msg = match i {
            0 => "a message much wider than its column, cut at the edge".to_string(),
            1237 => "x".repeat(300),
            _ => format!("msg {i}"),
        };
        s.push_str(&format!(
            "2024-03-11 10:{:02}:{:02}.0000000,{},{},\"{msg}\"\n",
            i / 60 % 60,
            i % 60,
            4624 + i % 3,
            ["jdoe", "asmith"][(i % 2) as usize]
        ));
    }
    std::fs::write(&p, s).unwrap();
    p
}

fn harness(dir: &Path, file: &Path, detail: bool) -> Harness<'static, App> {
    let file = file.to_path_buf();
    let cache = dir.join("cache");
    let settings = Settings {
        detail_pane: detail,
        ..Settings::default()
    };
    Harness::builder()
        .with_size([1400.0, 800.0])
        .build_eframe(move |cc| App::with(cc, vec![file.clone()], settings.clone(), cache.clone()))
}

fn ready(h: &mut Harness<'static, App>) {
    let t0 = Instant::now();
    loop {
        let t = &h.state().tabs[0];
        if t.ready() && t.job.is_none() && !t.view.is_empty() {
            break;
        }
        assert!(t0.elapsed() < Duration::from_secs(20), "timed out");
        std::thread::sleep(Duration::from_millis(5));
        h.step();
    }
    h.run_steps(3);
}

fn ev(h: &mut Harness<'static, App>, e: egui::Event) {
    h.input_mut().events.push(e);
}

fn button(h: &mut Harness<'static, App>, pos: egui::Pos2, b: egui::PointerButton, pressed: bool) {
    ev(
        h,
        egui::Event::PointerButton {
            pos,
            button: b,
            pressed,
            modifiers: egui::Modifiers::NONE,
        },
    );
    h.step();
}

fn mv(h: &mut Harness<'static, App>, pos: egui::Pos2) {
    ev(h, egui::Event::PointerMoved(pos));
    h.step();
}

fn click(h: &mut Harness<'static, App>, p: egui::Pos2) {
    mv(h, p);
    button(h, p, egui::PointerButton::Primary, true);
    button(h, p, egui::PointerButton::Primary, false);
    h.run_steps(2);
}

#[test]
fn closed_detail_pane_has_a_button_to_reopen_it() {
    let dir = tempfile::tempdir().unwrap();
    let file = sample(dir.path());
    let mut h = harness(dir.path(), &file, false);
    ready(&mut h);
    assert!(!h.state().settings.detail_pane);
    h.get_by_label("Show detail pane").click();
    h.run_steps(2);
    assert!(h.state().settings.detail_pane);
    // Open: the strip is gone.
    assert!(h.query_by_label("Show detail pane").is_none());
}

#[test]
fn auto_size_fits_the_longest_cell_of_the_column() {
    let dir = tempfile::tempdir().unwrap();
    let file = sample(dir.path());
    let mut h = harness(dir.path(), &file, true);
    ready(&mut h);
    // Message is display column 5 (tag, #, Time, EventId, User, Message).
    let (x0, x1) = h.state().tabs[0].grid.col_x[5];
    let p = egui::pos2((x0 + x1) / 2.0, h.state().tabs[0].grid.header_y);
    mv(&mut h, p);
    button(&mut h, p, egui::PointerButton::Secondary, true);
    button(&mut h, p, egui::PointerButton::Secondary, false);
    h.run_steps(2);
    h.get_by_label("Auto-size column").click();
    h.run_steps(3);
    // 300 characters of the grid font: far wider than the sampled rows'
    // "msg 123" and the 800 px cap of before.
    let w = h.state().tabs[0].widths[&3];
    assert!(w > 1200.0, "width {w}");
}

#[test]
fn horizontal_scrollbar_and_widening_past_the_edge() {
    let dir = tempfile::tempdir().unwrap();
    let file = sample(dir.path());
    let mut h = harness(dir.path(), &file, false);
    ready(&mut h);
    // Make Message wider than the view: the horizontal scrollbar appears.
    h.state_mut().tabs[0].widths.insert(3, 3000.0);
    h.run_steps(3);
    let g = &h.state().tabs[0].grid;
    let (body, x0) = (g.body, g.scroll_x);
    assert_eq!(x0, 0.0);
    // A click on the track right of the thumb pages right.
    click(&mut h, egui::pos2(body.max.x - 4.0, body.max.y + 6.0));
    let x1 = h.state().tabs[0].grid.scroll_x;
    assert!(x1 > 100.0, "scroll_x {x1}");

    // Drag User's right edge past the view's edge: it keeps growing.
    h.state_mut().tabs[0].grid.scroll_x = 0.0;
    h.state_mut().tabs[0].widths.insert(3, 140.0);
    h.run_steps(3);
    let (_, ux1) = h.state().tabs[0].grid.col_x[4];
    let hy = h.state().tabs[0].grid.header_y;
    let w0 = h.state().tabs[0].widths[&2];
    let start = egui::pos2(ux1, hy);
    mv(&mut h, start);
    button(&mut h, start, egui::PointerButton::Primary, true);
    let edge = egui::pos2(body.max.x + 2.0, hy);
    for _ in 0..30 {
        mv(&mut h, edge);
    }
    button(&mut h, edge, egui::PointerButton::Primary, false);
    h.run_steps(2);
    let w1 = h.state().tabs[0].widths[&2];
    assert!(
        w1 > w0 + (edge.x - start.x) + 50.0,
        "grew {w0} -> {w1} for a {} px drag",
        edge.x - start.x
    );
    assert!(h.state().tabs[0].grid.scroll_x > 0.0);
}

#[test]
fn the_ellipsis_of_a_cut_cell_opens_the_detail_pane() {
    let dir = tempfile::tempdir().unwrap();
    let file = sample(dir.path());
    let mut h = harness(dir.path(), &file, false);
    ready(&mut h);
    let g = &h.state().tabs[0].grid;
    let (_, x1) = g.col_x[5];
    let row_h = h.state().style.row_h;
    let p = egui::pos2(x1 - 8.0, g.body.min.y + row_h / 2.0);
    click(&mut h, p);
    assert!(h.state().settings.detail_pane, "… opened the detail pane");
    // A cell that fits has no …: a click there selects only.
    let dir2 = tempfile::tempdir().unwrap();
    let file2 = sample(dir2.path());
    let mut h2 = harness(dir2.path(), &file2, false);
    ready(&mut h2);
    let g = &h2.state().tabs[0].grid;
    let (_, x1) = g.col_x[5];
    let p = egui::pos2(x1 - 8.0, g.body.min.y + row_h * 1.5);
    click(&mut h2, p);
    assert!(!h2.state().settings.detail_pane);
}
