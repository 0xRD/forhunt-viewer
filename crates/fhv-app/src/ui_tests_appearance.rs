//! UI tests for repaint frequency (D-055), themes (D-056) and grid font
//! sizes (D-057).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;

use crate::app::App;
use crate::settings::{GridFont, Settings};

fn big(dir: &Path, rows: u32) -> PathBuf {
    let p = dir.join("big.csv");
    let mut s = String::from("Time,EventId,User,Message\n");
    for i in 0..rows {
        s.push_str(&format!(
            "2024-03-11 10:{:02}:{:02}.0000000,{},user{},message number {i} with some text\n",
            i / 60 % 60,
            i % 60,
            4600 + i % 50,
            i % 97
        ));
    }
    std::fs::write(&p, s).unwrap();
    p
}

fn harness(dir: &Path, file: &Path, settings: Settings) -> Harness<'static, App> {
    let file = file.to_path_buf();
    let cache = dir.join("cache");
    Harness::builder()
        .with_size([1400.0, 800.0])
        .build_eframe(move |cc| App::with(cc, vec![file.clone()], settings.clone(), cache.clone()))
}

fn wait(h: &mut Harness<'static, App>, done: impl Fn(&App) -> bool) {
    let t0 = Instant::now();
    while !done(h.state()) {
        assert!(t0.elapsed() < Duration::from_secs(30), "timed out");
        std::thread::sleep(Duration::from_millis(5));
        h.step();
    }
    h.step();
}

fn idle(a: &App) -> bool {
    let t = &a.tabs[0];
    t.ready() && t.job.is_none() && !t.view.is_empty() && !a.appearance.font_busy()
}

fn repaint_delay(h: &Harness<'static, App>) -> Duration {
    h.output()
        .viewport_output
        .get(&egui::ViewportId::ROOT)
        .map_or(Duration::MAX, |v| v.repaint_delay)
}

/// Frames a native event loop would run during `dur` without input: it
/// sleeps for the requested repaint delay and runs a frame when it ends.
fn frames_without_input(h: &mut Harness<'static, App>, dur: Duration) -> u32 {
    let t0 = Instant::now();
    let mut n = 0;
    loop {
        let left = dur.saturating_sub(t0.elapsed());
        let d = repaint_delay(h);
        if d >= left {
            return n;
        }
        std::thread::sleep(d);
        h.step();
        n += 1;
        assert!(n < 10_000);
    }
}

/// Idle with a large file open: no frame at all (was ~120 per second:
/// the window title was sent every frame, and a viewport command repaints).
#[test]
fn idle_window_does_not_repaint() {
    let dir = tempfile::tempdir().unwrap();
    let file = big(dir.path(), 200_000);
    let mut h = harness(dir.path(), &file, Settings::default());
    wait(&mut h, idle);
    // Selection, detail pane and a search highlight on screen.
    h.state_mut().tabs[0].grid.selected = Some(3);
    h.state_mut().tabs[0].search = "number 1".into();
    wait(&mut h, |a| idle(a) && a.tabs[0].hits_in_view.is_some());
    h.run_ok();
    let n = frames_without_input(&mut h, Duration::from_secs(2));
    assert!(n <= 2, "{n} frames in 2 s while idle");
}

/// While indexing, the progress display repaints on a timer (never
/// "as fast as possible", e.g. by egui's spinner).
#[test]
fn busy_window_repaints_at_a_throttled_rate() {
    let dir = tempfile::tempdir().unwrap();
    let file = big(dir.path(), 400_000);
    let mut h = harness(dir.path(), &file, Settings::default());
    h.input_mut().focused = true;
    let t0 = Instant::now();
    let mut busy_frames = 0;
    while !idle(h.state()) {
        assert!(t0.elapsed() < Duration::from_secs(30), "timed out");
        h.step();
        let a = h.state();
        if a.tabs[0].loading() || a.tabs[0].job.is_some() {
            busy_frames += 1;
            let d = repaint_delay(&h);
            assert!(
                d >= Duration::from_millis(99),
                "repaint after {d:?} while busy"
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(busy_frames > 0 || h.state().tabs[0].ready());
    assert_eq!(
        crate::appearance::busy_repaint(&h.ctx),
        Duration::from_millis(100)
    );
    h.input_mut().focused = false;
    h.step();
    assert_eq!(
        crate::appearance::busy_repaint(&h.ctx),
        Duration::from_secs(1)
    );
}

/// Every built-in theme renders every window without panicking, and the
/// grid's colors come from it.
#[test]
fn every_theme_renders() {
    let dir = tempfile::tempdir().unwrap();
    let file = big(dir.path(), 500);
    let mut h = harness(dir.path(), &file, Settings::default());
    wait(&mut h, idle);
    let t = &mut h.state_mut().tabs[0];
    t.grid.selected = Some(2);
    t.toggle_tag(1);
    t.search = "user1".into();
    h.state_mut().appearance.settings_open = true;
    h.state_mut().help = true;
    let mut names: Vec<String> = crate::theme::builtin()
        .iter()
        .map(|t| t.name.clone())
        .collect();
    names.push(crate::theme::SYSTEM.into());
    for n in names {
        h.state_mut().settings.theme = Some(n.clone());
        h.run_steps(3);
        let cur = crate::theme::current(&h.ctx);
        let want = if n == crate::theme::SYSTEM {
            "Dark"
        } else {
            &n
        };
        assert_eq!(cur.name, want);
        assert_eq!(h.ctx.global_style().visuals.panel_fill, cur.bg.0, "{n}");
        assert_eq!(h.ctx.global_style().visuals.dark_mode, cur.dark, "{n}");
    }
}

/// Older settings (only `dark`) keep their theme.
#[test]
fn old_dark_flag_maps_to_a_theme() {
    let s: Settings = serde_json::from_str(r#"{"dark": false}"#).unwrap();
    assert_eq!(s.theme_name(), "Light");
    let s: Settings = serde_json::from_str(r#"{"dark": true}"#).unwrap();
    assert_eq!(s.theme_name(), "Dark");
    let s: Settings =
        serde_json::from_str(r#"{"theme": "Nord", "grid_font": {"System": "Menlo"}}"#).unwrap();
    assert_eq!(s.theme_name(), "Nord");
    assert_eq!(s.grid_font, GridFont::System("Menlo".into()));
}

fn click(h: &mut Harness<'static, App>, p: egui::Pos2) {
    h.event(egui::Event::PointerMoved(p));
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton {
            pos: p,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        });
    }
    h.run_steps(2);
}

/// Hit-testing, scrolling and paging follow the row height at a small and
/// a large font size.
#[test]
fn grid_follows_the_font_size() {
    let dir = tempfile::tempdir().unwrap();
    let file = big(dir.path(), 5000);
    for size in [9.0, 24.0] {
        let s = Settings {
            grid_font_size: size,
            ..Settings::default()
        };
        let mut h = harness(dir.path(), &file, s);
        wait(&mut h, idle);
        h.run_ok();
        let row_h = crate::appearance::row_height(size);
        let g = &h.state().tabs[0].grid;
        let body = g.body;
        assert!(body.height() > 100.0, "{body:?}");
        assert_eq!(g.page_rows, (body.height() / row_h).floor() as u64);
        assert_eq!(g.top, 0.0);
        // Click in the middle of row 5.
        let x = body.min.x + 300.0;
        click(&mut h, egui::pos2(x, body.min.y + row_h * 5.5));
        assert_eq!(h.state().tabs[0].grid.selected, Some(5), "size {size}");
        // Scroll by 7 rows with the wheel.
        h.event(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, -row_h * 7.0),
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Unknown,
            modifiers: Default::default(),
        });
        h.run_ok();
        let top = h.state().tabs[0].grid.top;
        assert!((top - 7.0).abs() < 0.05, "size {size}: top {top}");
        // The first visible rows are where the clicks land.
        h.state_mut().tabs[0].grid.top = 7.0;
        h.step();
        click(&mut h, egui::pos2(x, body.min.y + row_h * 2.5));
        assert_eq!(h.state().tabs[0].grid.selected, Some(9), "size {size}");
        // Page down moves by the rows that fit.
        let page = h.state().tabs[0].grid.page_rows;
        h.key_press(egui::Key::PageDown);
        h.run_steps(2);
        assert_eq!(h.state().tabs[0].grid.selected, Some(9 + page));
    }
}

/// Ctrl+= / Ctrl+- / Ctrl+0 change the grid font size, within 8-32.
#[test]
fn font_size_shortcuts() {
    let dir = tempfile::tempdir().unwrap();
    let file = big(dir.path(), 100);
    let mut h = harness(dir.path(), &file, Settings::default());
    wait(&mut h, idle);
    let size = |h: &Harness<'static, App>| h.state().settings.grid_font_size;
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Equals);
    h.run_steps(2);
    assert_eq!(size(&h), 13.0);
    assert_eq!(h.state().style_row_h(), crate::appearance::row_height(13.0));
    for _ in 0..40 {
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Minus);
        h.step();
    }
    assert_eq!(size(&h), crate::settings::FONT_MIN);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Num0);
    h.run_steps(2);
    assert_eq!(size(&h), crate::settings::FONT_SIZE);
    // The UI itself did not zoom.
    assert_eq!(h.ctx.zoom_factor(), 1.0);
}

/// A bad font file is refused with a message, and the grid keeps the font
/// it had.
#[test]
fn bad_font_file_keeps_the_previous_font() {
    let dir = tempfile::tempdir().unwrap();
    let file = big(dir.path(), 100);
    let junk = dir.path().join("junk.ttf");
    std::fs::write(&junk, vec![0u8; 10_000]).unwrap();
    let mut h = harness(dir.path(), &file, Settings::default());
    wait(&mut h, idle);
    h.state_mut().settings.grid_font = GridFont::Bundled("Inter".into());
    wait(&mut h, idle);
    h.state_mut().settings.grid_font = GridFont::File(junk);
    h.state_mut().appearance.settings_open = true;
    wait(&mut h, |a| idle(a) && a.appearance.font_error.is_some());
    h.run_steps(2);
    assert_eq!(
        h.state().settings.grid_font,
        GridFont::Bundled("Inter".into())
    );
    assert!(
        h.state()
            .appearance
            .font_error
            .as_deref()
            .unwrap()
            .contains("not a font")
    );
    // A real font file is accepted.
    let good = dir.path().join("mono.ttf");
    std::fs::write(
        &good,
        include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf"),
    )
    .unwrap();
    h.state_mut().settings.grid_font = GridFont::File(good.clone());
    wait(&mut h, idle);
    assert_eq!(h.state().settings.grid_font, GridFont::File(good));
}

/// The date dialog carries its own help; the Settings window has no
/// documentation tab.
#[test]
fn date_help_is_the_cheat_sheet() {
    let dir = tempfile::tempdir().unwrap();
    let file = big(dir.path(), 50);
    let mut h = harness(dir.path(), &file, Settings::default());
    wait(&mut h, idle);
    h.get_by_label("Help").click();
    h.run_steps(2);
    assert!(h.query_by_label("Date formats").is_none());
    h.state_mut().open_date_format(0);
    h.state_mut().datefmt.as_mut().unwrap().mode = crate::datefmt::Mode::Custom;
    h.run_steps(2);
    h.get_by_label_contains("Cheat sheet and examples");
    assert!(h.query_by_label("Learn more").is_none());
    assert!(!h.state().appearance.settings_open);
}
