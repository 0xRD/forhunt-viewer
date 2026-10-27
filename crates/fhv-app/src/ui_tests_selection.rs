//! UI tests for cell / row selection (D-060), copying it (D-061) and the
//! detail pane highlight (D-062).

use std::path::Path;
use std::time::{Duration, Instant};

use eframe::egui::{self, Modifiers, Pos2, pos2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;

use crate::app::App;
use crate::selection::Block;
use crate::settings::Settings;
use crate::tab::DC_FIRST;

fn sample(dir: &Path) -> std::path::PathBuf {
    let p = dir.join("events.csv");
    let mut s = String::from("Time,EventId,User,Message\n");
    for i in 0..1000u32 {
        s.push_str(&format!(
            "2024-03-11 10:{:02}:{:02}.0000000,{},{},\"msg {i}, \"\"q\"\"\"\n",
            i / 60 % 60,
            i % 60,
            [4624, 4625, 4688][(i % 3) as usize],
            ["jdoe", "asmith", "SYSTEM", "bwilson"][(i % 4) as usize]
        ));
    }
    std::fs::write(&p, s).unwrap();
    p
}

fn harness(dir: &Path, file: &Path) -> Harness<'static, App> {
    let file = file.to_path_buf();
    let cache = dir.join("cache");
    let mut h = Harness::builder()
        .with_size([1400.0, 800.0])
        .build_eframe(move |cc| {
            App::with(cc, vec![file.clone()], Settings::default(), cache.clone())
        });
    let t0 = Instant::now();
    while !{
        let t = &h.state().tabs[0];
        t.ready() && t.job.is_none() && !t.view.is_empty()
    } {
        assert!(t0.elapsed() < Duration::from_secs(20), "timed out");
        std::thread::sleep(Duration::from_millis(5));
        h.step();
    }
    h.step();
    h
}

/// Centre of a cell (display row, display column), the grid unscrolled.
fn cell(h: &Harness<'static, App>, row: u64, dc: usize) -> Pos2 {
    let g = &h.state().tabs[0].grid;
    let (x0, x1) = g.col_x[dc];
    let rh = h.state().style_row_h();
    pos2((x0 + x1) / 2.0, g.body.min.y + (row as f32 + 0.5) * rh)
}

/// Point in the row gutter at a display row.
fn gutter(h: &Harness<'static, App>, row: u64) -> Pos2 {
    let p = cell(h, row, 0);
    pos2(h.state().tabs[0].grid.body.min.x - 5.0, p.y)
}

fn button(h: &mut Harness<'static, App>, pos: Pos2, pressed: bool, m: Modifiers) {
    h.input_mut().events.push(egui::Event::ModifiersChanged(m));
    h.input_mut().events.push(egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: m,
    });
    h.step();
}

fn click(h: &mut Harness<'static, App>, pos: Pos2, m: Modifiers) {
    h.input_mut().events.push(egui::Event::PointerMoved(pos));
    h.step();
    button(h, pos, true, m);
    button(h, pos, false, m);
    h.input_mut()
        .events
        .push(egui::Event::ModifiersChanged(Modifiers::NONE));
    h.step();
}

/// Ctrl+C and the text the app put on the clipboard in that frame.
fn copy(h: &mut Harness<'static, App>) -> Option<String> {
    h.input_mut().events.push(egui::Event::Copy);
    h.step();
    h.output()
        .platform_output
        .commands
        .iter()
        .find_map(|c| match c {
            egui::OutputCommand::CopyText(t) => Some(t.clone()),
            _ => None,
        })
}

#[test]
fn drag_selects_a_block_and_ctrl_click_toggles() {
    let dir = tempfile::tempdir().unwrap();
    let file = sample(dir.path());
    let mut h = harness(dir.path(), &file);
    let (a, b) = (cell(&h, 2, DC_FIRST), cell(&h, 4, DC_FIRST + 2));
    h.input_mut().events.push(egui::Event::PointerMoved(a));
    h.step();
    button(&mut h, a, true, Modifiers::NONE);
    for k in 1..=5 {
        let p = a + (b - a) * (k as f32 / 5.0);
        h.input_mut().events.push(egui::Event::PointerMoved(p));
        h.step();
    }
    button(&mut h, b, false, Modifiers::NONE);
    let g = &h.state().tabs[0].grid;
    assert_eq!(
        g.cells.bounds(),
        Some(Block {
            r0: 2,
            r1: 4,
            c0: DC_FIRST,
            c1: DC_FIRST + 2
        })
    );
    assert_eq!(g.cells.cell_count(), 9);
    // The active cell stays where the drag started.
    assert_eq!((g.selected, g.selected_col), (Some(2), DC_FIRST));
    assert!(!g.rows_active);

    let p = cell(&h, 8, DC_FIRST + 1);
    click(&mut h, p, Modifiers::COMMAND);
    let g = &h.state().tabs[0].grid;
    assert_eq!((g.cells.ranges(), g.cells.cell_count()), (2, 10));
    assert_eq!(g.selected, Some(8));
    let p = cell(&h, 3, DC_FIRST + 1);
    click(&mut h, p, Modifiers::COMMAND);
    let g = &h.state().tabs[0].grid;
    assert_eq!(g.cells.cell_count(), 9);
    assert!(!g.cells.contains((3, DC_FIRST + 1)));
    // Shift+arrows extend from the active cell; a plain arrow collapses.
    let p = cell(&h, 5, DC_FIRST);
    click(&mut h, p, Modifiers::NONE);
    h.key_press_modifiers(Modifiers::SHIFT, egui::Key::ArrowDown);
    h.step();
    h.key_press_modifiers(Modifiers::SHIFT, egui::Key::ArrowRight);
    h.step();
    let g = &h.state().tabs[0].grid;
    assert_eq!(g.cells.cell_count(), 4);
    assert_eq!(g.selected, Some(5));
    h.key_press(egui::Key::ArrowDown);
    h.step();
    let g = &h.state().tabs[0].grid;
    assert_eq!((g.cells.cell_count(), g.selected), (1, Some(6)));
    // Ctrl+A is one range, and a sort clears it.
    h.key_press_modifiers(Modifiers::COMMAND, egui::Key::A);
    h.step();
    assert_eq!(h.state().tabs[0].grid.cells.cell_count(), 1000 * 6);
    h.state_mut().tabs[0].sort = vec![fhv_engine::SortKey {
        col: 2,
        desc: false,
    }];
    h.state_mut().tabs[0].requery();
    let t0 = Instant::now();
    while h.state().tabs[0].grid.cells.is_multi() {
        assert!(t0.elapsed() < Duration::from_secs(20), "timed out");
        std::thread::sleep(Duration::from_millis(5));
        h.step();
    }
}

#[test]
fn ctrl_c_copies_a_2x3_block_as_tsv() {
    let dir = tempfile::tempdir().unwrap();
    let file = sample(dir.path());
    let mut h = harness(dir.path(), &file);
    let p = cell(&h, 1, DC_FIRST);
    click(&mut h, p, Modifiers::NONE);
    // One cell: its bare value.
    assert_eq!(copy(&mut h).as_deref(), Some("2024-03-11 10:00:01.0000000"));
    let p = cell(&h, 2, DC_FIRST + 2);
    click(&mut h, p, Modifiers::SHIFT);
    let text = copy(&mut h).expect("clipboard");
    assert_eq!(
        text,
        "2024-03-11 10:00:01.0000000\t4625\tasmith\n2024-03-11 10:00:02.0000000\t4688\tSYSTEM"
    );
    assert!(h.state().tabs[0].status.as_deref() == Some("copied 6 cells"));
    // Ctrl+Shift+C still copies the active row as JSON.
    let m = Modifiers::COMMAND | Modifiers::SHIFT;
    h.input_mut().events.extend([
        egui::Event::ModifiersChanged(m),
        egui::Event::Key {
            key: egui::Key::C,
            pressed: true,
            modifiers: m,
            repeat: false,
            physical_key: None,
        },
    ]);
    h.step();
    let json = h
        .output()
        .platform_output
        .commands
        .iter()
        .find_map(|c| match c {
            egui::OutputCommand::CopyText(t) => Some(t.clone()),
            _ => None,
        })
        .expect("row json");
    assert!(
        json.starts_with("{\n  \"Time\": \"2024-03-11 10:00:01"),
        "{json}"
    );
}

#[test]
fn row_gutter_selects_rows_and_copies_them_with_headers() {
    let dir = tempfile::tempdir().unwrap();
    let file = sample(dir.path());
    let mut h = harness(dir.path(), &file);
    let p = cell(&h, 7, DC_FIRST);
    click(&mut h, p, Modifiers::NONE);
    let p = gutter(&h, 0);
    click(&mut h, p, Modifiers::NONE);
    let p = gutter(&h, 2);
    click(&mut h, p, Modifiers::SHIFT);
    let p = gutter(&h, 5);
    click(&mut h, p, Modifiers::COMMAND);
    let g = &h.state().tabs[0].grid;
    assert!(g.rows_active);
    assert_eq!(g.rows.set.ranges(), [0..3, 5..6]);
    // The active cell is unchanged: row and cell selections are separate.
    assert_eq!(g.selected, Some(7));
    let text = copy(&mut h).expect("clipboard");
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], "Time\tEventId\tUser\tMessage");
    assert_eq!(lines.len(), 5);
    assert_eq!(
        lines[4],
        "2024-03-11 10:00:05.0000000\t4688\tasmith\t\"msg 5, \"\"q\"\"\""
    );
    // A click on a cell makes the cells the selection again.
    let p = cell(&h, 1, DC_FIRST + 1);
    click(&mut h, p, Modifiers::NONE);
    assert!(!h.state().tabs[0].grid.rows_active);
    assert_eq!(copy(&mut h).as_deref(), Some("4625"));
    // The gutter header selects all rows; tagging them tags the view.
    let t = &h.state().tabs[0].grid;
    let corner = pos2(t.body.min.x - 5.0, t.header_y);
    click(&mut h, corner, Modifiers::NONE);
    assert_eq!(h.state().tabs[0].grid.rows.set.len(), 1000);
    let all = h.state().tabs[0].grid.rows.set.ranges().to_vec();
    h.state_mut().tabs[0].tag_positions(&all);
    assert_eq!(h.state().tabs[0].tags.len(), 1000);
}

#[test]
fn detail_pane_highlights_the_active_cells_field() {
    let dir = tempfile::tempdir().unwrap();
    let file = sample(dir.path());
    let mut h = harness(dir.path(), &file);
    let p = cell(&h, 3, DC_FIRST + 2);
    click(&mut h, p, Modifiers::NONE);
    h.step();
    assert_eq!(h.state().detail_hl, Some((3, 2)));
    h.key_press(egui::Key::ArrowRight);
    h.step();
    h.step();
    assert_eq!(h.state().detail_hl, Some((3, 3)));
    // Clicking a field in the pane selects its cell.
    h.get_by_label("EventId").click();
    h.step();
    h.step();
    assert_eq!(h.state().tabs[0].grid.selected_col, DC_FIRST + 1);
    assert_eq!(h.state().detail_hl, Some((3, 1)));
    // A column hidden in the grid is still highlighted from the pane.
    h.state_mut().tabs[0].order.retain(|&c| c != 2);
    h.step();
    h.get_by_label("User").click();
    h.step();
    h.step();
    assert_eq!(h.state().detail_hl, Some((3, 2)));
    assert_eq!(h.state().tabs[0].grid.selected_col, DC_FIRST + 1);
}
