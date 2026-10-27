//! UI tests for dragging column headers (D-070) and auto-sizing (D-071).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;

use crate::app::App;
use crate::settings::Settings;

fn sample(dir: &Path) -> PathBuf {
    let p = dir.join("events.csv");
    let mut s = String::from("Time,EventId,User,Message\n");
    for i in 0..500u32 {
        s.push_str(&format!(
            "2024-03-11 10:{:02}:{:02}.0000000,{},{},\"msg {i}\"\n",
            i / 60 % 60,
            i % 60,
            4624 + i % 3,
            ["jdoe", "asmith"][(i % 2) as usize]
        ));
    }
    std::fs::write(&p, s).unwrap();
    p
}

fn harness(dir: &Path, file: &Path) -> Harness<'static, App> {
    let file = file.to_path_buf();
    let cache = dir.join("cache");
    Harness::builder()
        .with_size([1400.0, 800.0])
        .build_eframe(move |cc| {
            App::with(cc, vec![file.clone()], Settings::default(), cache.clone())
        })
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

/// Presses on the header of display column `dc`, moves to x `to_x` in
/// steps; the caller releases.
fn start_drag(h: &mut Harness<'static, App>, dc: usize, to_x: f32) -> egui::Pos2 {
    let g = &h.state().tabs[0].grid;
    let (x0, x1) = g.col_x[dc];
    let y = g.header_y;
    let from = egui::pos2((x0 + x1) / 2.0, y);
    mv(h, from);
    button(h, from, egui::PointerButton::Primary, true);
    for k in 1..=6 {
        mv(h, egui::pos2(from.x + (to_x - from.x) * k as f32 / 6.0, y));
    }
    egui::pos2(to_x, y)
}

fn right_half(h: &Harness<'static, App>, dc: usize) -> f32 {
    let (x0, x1) = h.state().tabs[0].grid.col_x[dc];
    x0 + (x1 - x0) * 0.8
}

#[test]
fn dragging_a_header_reorders_and_does_not_sort() {
    let dir = tempfile::tempdir().unwrap();
    let file = sample(dir.path());
    let mut h = harness(dir.path(), &file);
    ready(&mut h);
    assert_eq!(h.state().tabs[0].order, vec![0, 1, 2, 3]);
    // Time (display column 2) dropped on the right half of User (4).
    let to = right_half(&h, 4);
    let end = start_drag(&mut h, 2, to);
    assert!(
        h.state().tabs[0].order == vec![0, 1, 2, 3],
        "not before the drop"
    );
    button(&mut h, end, egui::PointerButton::Primary, false);
    h.run_steps(2);
    let t = &h.state().tabs[0];
    assert_eq!(t.order, vec![1, 2, 0, 3]);
    assert!(t.sort.is_empty(), "a drag must not sort");
    // Sessions keep the order (by name); menu reordering stays in sync.
    let s = crate::session::capture(t);
    let names: Vec<_> = s.columns.iter().map(|c| c.0.as_str()).collect();
    assert_eq!(names, ["EventId", "User", "Time", "Message"]);
}

#[test]
fn small_movement_is_still_a_click_and_esc_cancels() {
    let dir = tempfile::tempdir().unwrap();
    let file = sample(dir.path());
    let mut h = harness(dir.path(), &file);
    ready(&mut h);
    let (x0, x1) = h.state().tabs[0].grid.col_x[3];
    let y = h.state().tabs[0].grid.header_y;
    let p = egui::pos2((x0 + x1) / 2.0, y);
    mv(&mut h, p);
    button(&mut h, p, egui::PointerButton::Primary, true);
    mv(&mut h, egui::pos2(p.x + 2.0, y));
    button(&mut h, p, egui::PointerButton::Primary, false);
    h.run_steps(3);
    let t = &h.state().tabs[0];
    assert_eq!(t.sort.len(), 1, "click sorts");
    assert_eq!(t.sort[0].col, 1);
    // Esc cancels a drag in progress.
    let to = right_half(&h, 5);
    let end = start_drag(&mut h, 2, to);
    h.key_press(egui::Key::Escape);
    h.step();
    button(&mut h, end, egui::PointerButton::Primary, false);
    h.run_steps(2);
    let t = &h.state().tabs[0];
    assert_eq!(t.order, vec![0, 1, 2, 3]);
    assert_eq!(t.sort.len(), 1, "the cancelled drag did not sort");
    assert_eq!(t.sort[0].col, 1);
}

#[test]
fn dragging_across_the_frozen_edge_moves_the_boundary() {
    let dir = tempfile::tempdir().unwrap();
    let file = sample(dir.path());
    let mut h = harness(dir.path(), &file);
    ready(&mut h);
    // tag, #, Time, EventId frozen.
    h.state_mut().tabs[0].grid.frozen = 4;
    h.run_steps(2);
    let to = right_half(&h, 4);
    let end = start_drag(&mut h, 2, to);
    button(&mut h, end, egui::PointerButton::Primary, false);
    h.run_steps(2);
    let t = &h.state().tabs[0];
    assert_eq!(t.order, vec![1, 2, 0, 3]);
    assert_eq!(t.grid.frozen, 3, "Time left the frozen zone");
}

#[test]
fn hidden_columns_keep_their_place() {
    let dir = tempfile::tempdir().unwrap();
    let file = sample(dir.path());
    let mut h = harness(dir.path(), &file);
    ready(&mut h);
    let t = &mut h.state_mut().tabs[0];
    crate::colorder::hide(&mut t.order, &mut t.hidden_after, 1);
    assert_eq!(t.order, vec![0, 2, 3]);
    crate::colorder::show(&mut t.order, &mut t.hidden_after, 1);
    assert_eq!(t.order, vec![0, 1, 2, 3]);
}

#[test]
fn double_click_on_the_edge_auto_sizes() {
    let dir = tempfile::tempdir().unwrap();
    let file = sample(dir.path());
    let mut h = harness(dir.path(), &file);
    ready(&mut h);
    let w0 = h.state().tabs[0].widths[&0];
    let w1 = h.state().tabs[0].widths[&1];
    let (_, x1) = h.state().tabs[0].grid.col_x[2];
    let y = h.state().tabs[0].grid.header_y;
    let p = egui::pos2(x1, y);
    mv(&mut h, p);
    for pressed in [true, false, true, false] {
        ev(
            &mut h,
            egui::Event::PointerButton {
                pos: p,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            },
        );
    }
    h.step();
    h.run_steps(3);
    let t = &h.state().tabs[0];
    assert_ne!(t.widths[&0], w0, "the Time column was fitted");
    assert!(t.widths[&0] > 100.0 && t.widths[&0] < 400.0);
    assert_eq!(t.widths[&1], w1, "other columns untouched");
    assert!(t.sort.is_empty());
}

#[test]
fn header_menu_auto_sizes_one_or_all() {
    let dir = tempfile::tempdir().unwrap();
    let file = sample(dir.path());
    let mut h = harness(dir.path(), &file);
    ready(&mut h);
    let w: Vec<f32> = (0..4).map(|c| h.state().tabs[0].widths[&c]).collect();
    let (x0, x1) = h.state().tabs[0].grid.col_x[3];
    let p = egui::pos2((x0 + x1) / 2.0, h.state().tabs[0].grid.header_y);
    mv(&mut h, p);
    button(&mut h, p, egui::PointerButton::Secondary, true);
    button(&mut h, p, egui::PointerButton::Secondary, false);
    h.run_steps(2);
    h.get_by_label("Auto-size column").click();
    h.run_steps(3);
    let t = &h.state().tabs[0];
    assert_ne!(t.widths[&1], w[1], "EventId was fitted");
    assert_eq!(t.widths[&0], w[0], "Time untouched");
    button(&mut h, p, egui::PointerButton::Secondary, true);
    button(&mut h, p, egui::PointerButton::Secondary, false);
    h.run_steps(2);
    h.get_by_label("Auto-size all columns").click();
    h.run_steps(3);
    let t = &h.state().tabs[0];
    assert_ne!(t.widths[&0], w[0]);
    assert!(
        t.widths[&1] < 100.0 && t.widths[&3] < 100.0,
        "{:?}",
        t.widths
    );
}
