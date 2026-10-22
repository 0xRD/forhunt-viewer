//! UI smoke tests (egui_kittest, headless): open a file, wait for the
//! index, filter, sort, tag, export, session round trip.

use std::path::Path;
use std::time::{Duration, Instant};

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use fhv_engine::filter::Op;

use crate::app::App;
use crate::settings::Settings;

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
    Harness::builder()
        .with_size([1400.0, 800.0])
        .build_eframe(move |cc| {
            App::with(cc, vec![file.clone()], Settings::default(), cache.clone())
        })
}

/// Steps the UI until `done` holds (background threads need real time).
fn wait(h: &mut Harness<'static, App>, done: impl Fn(&App) -> bool) {
    let t0 = Instant::now();
    while !done(h.state()) {
        assert!(t0.elapsed() < Duration::from_secs(20), "timed out");
        std::thread::sleep(Duration::from_millis(5));
        h.step();
    }
    h.step();
}

fn idle(a: &App) -> bool {
    let t = &a.tabs[0];
    t.ready() && t.job.is_none() && !t.view.is_empty()
}

#[test]
fn open_filter_sort_tag_export_session() {
    let dir = tempfile::tempdir().unwrap();
    let file = sample(dir.path());
    let mut h = harness(dir.path(), &file);
    wait(&mut h, idle);
    h.get_by_label_contains("showing 1,000 of 1,000 rows");

    // Quick filter from the filter row state: User contains "jd".
    h.state_mut().tabs[0].filters[2].quick = "jd".into();
    h.state_mut().tabs[0].requery();
    wait(&mut h, |a| idle(a) && a.tabs[0].view.len() == 250);
    h.get_by_label_contains("showing 250 of 1,000 rows");

    // A condition on a dictionary column, then clear filters with the button.
    h.state_mut().tabs[0].filters[1]
        .conds
        .push(Op::In(vec!["4625".into()]));
    h.state_mut().tabs[0].requery();
    wait(&mut h, |a| idle(a) && a.tabs[0].view.len() == 83);
    h.get_by_label("Clear filters").click();
    h.step();
    wait(&mut h, |a| idle(a) && a.tabs[0].view.len() == 1000);

    // Sort by EventId descending: first row is a 4688.
    h.state_mut().tabs[0].sort = vec![fhv_engine::SortKey { col: 1, desc: true }];
    h.state_mut().tabs[0].requery();
    wait(&mut h, |a| idle(a) && a.tabs[0].view.order.is_some());
    let first = h.state().tabs[0].view.row_at(0).unwrap();
    assert_eq!(first % 3, 2);

    // Tag two rows, show tagged only with the toolbar toggle.
    h.state_mut().tabs[0].toggle_tag(5);
    h.state_mut().tabs[0].toggle_tag(7);
    h.step();
    h.get_by_label_contains("tagged (2)").click();
    h.step();
    wait(&mut h, |a| idle(a) && a.tabs[0].view.len() == 2);

    // Export the view.
    let out = dir.path().join("out.csv");
    let (rows, cols): (Vec<u32>, Vec<usize>) = {
        let t = &h.state().tabs[0];
        (t.view.iter().collect(), t.order.clone())
    };
    h.state_mut().tabs[0].start_export(out.clone(), rows, cols, fhv_engine::export::Format::Csv);
    wait(&mut h, |a| a.tabs[0].job.is_none());
    let text = std::fs::read_to_string(&out).unwrap();
    assert_eq!(text.lines().count(), 3, "{text}");
    assert!(text.starts_with("Time,EventId,User,Message\n"));
    assert!(text.contains("\"msg 7, \"\"q\"\"\""));

    // Session round trip onto a fresh tab state.
    let s = crate::session::capture(&h.state().tabs[0]);
    let json = serde_json::to_string(&s).unwrap();
    let s2: crate::session::Session = serde_json::from_str(&json).unwrap();
    let t = &mut h.state_mut().tabs[0];
    t.tags.clear();
    t.sort.clear();
    t.tagged_only = false;
    let warn = crate::session::apply(t, &s2);
    assert!(warn.is_empty(), "{warn:?}");
    assert_eq!(t.tags.len(), 2);
    assert!(t.tagged_only);
    assert_eq!(t.sort.len(), 1);
    // The input file was never modified.
    assert!(
        std::fs::read_to_string(&file)
            .unwrap()
            .starts_with("Time,EventId,User,Message\n")
    );
}

/// Every window and pane renders without panicking (missing glyphs panic
/// under kittest).
#[test]
fn windows_and_panes_render() {
    let dir = tempfile::tempdir().unwrap();
    let file = sample(dir.path());
    let mut h = harness(dir.path(), &file);
    wait(&mut h, idle);
    h.state_mut().tabs[0].grid.selected = Some(3);
    h.state_mut().tabs[0].toggle_tag(3);
    h.state_mut().open_filter_editor(1);
    h.state_mut().columns_dlg = true;
    h.state_mut().help = true;
    h.run_steps(3);
    h.get_by_label("Row 4");
    h.get_by_label_contains("Values (3)");
    h.get_by_label("Keyboard shortcuts");
    h.get_by_label("Show all");
}

#[test]
fn global_search_highlights_and_navigates() {
    let dir = tempfile::tempdir().unwrap();
    let file = sample(dir.path());
    let mut h = harness(dir.path(), &file);
    wait(&mut h, idle);
    h.state_mut().tabs[0].search = "msg 99".into();
    wait(&mut h, |a| {
        a.tabs[0].search_hits.is_some() && a.tabs[0].job.is_none()
    });
    // msg 99, msg 990..999 -> 11 rows.
    assert_eq!(h.state().tabs[0].search_hits.as_ref().unwrap().len(), 11);
    let p = h.state().tabs[0].find_hit(true).unwrap();
    assert_eq!(h.state().tabs[0].view.row_at(p), Some(99));
}

#[test]
fn broken_file_does_not_crash() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("bad.csv");
    std::fs::write(&file, b"a,b\n1,\"open\n2,3\n\xff\xfe,\x00\n4,5,6\n").unwrap();
    let mut h = harness(dir.path(), &file);
    wait(&mut h, |a| a.tabs[0].ready() && a.tabs[0].job.is_none());
    h.step();
    assert!(h.state().tabs[0].load_error.is_none());
}
