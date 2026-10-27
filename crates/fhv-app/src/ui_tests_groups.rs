//! UI tests for grouping (D-075..D-078): header menu, group bar, sort by
//! count, collapse, group header menu, copy / export of data rows only,
//! sessions.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use fhv_engine::group::{GroupSort, Level};

use crate::app::App;
use crate::groups::Line;
use crate::settings::Settings;
use crate::tab::DC_FIRST;

const CHANNELS: [&str; 3] = ["Security", "System", "Application"];

fn channel(i: u32) -> &'static str {
    match i % 10 {
        0..5 => CHANNELS[0],
        5..8 => CHANNELS[1],
        _ => CHANNELS[2],
    }
}

fn user(i: u32) -> &'static str {
    if i.is_multiple_of(7) {
        ""
    } else {
        ["jdoe", "asmith"][(i % 2) as usize]
    }
}

fn sample(dir: &Path) -> PathBuf {
    let p = dir.join("events.csv");
    let mut s = String::from("Time,Channel,EventId,User\n");
    for i in 0..500u32 {
        s.push_str(&format!(
            "2024-03-11 10:{:02}:{:02}.0000000,{},{},{}\n",
            i / 60 % 60,
            i % 60,
            channel(i),
            4624 + i % 3,
            user(i),
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

/// Waits until the filters, sort and grouping shown are applied.
fn settle(h: &mut Harness<'static, App>) {
    let t0 = Instant::now();
    loop {
        h.step();
        let t = &h.state().tabs[0];
        if t.ready()
            && t.job.is_none()
            && !t.view.is_empty()
            && t.view_query_for_test() == Some(&t.query())
            && t.groups.applied == t.groups.levels
        {
            break;
        }
        assert!(t0.elapsed() < Duration::from_secs(20), "timed out");
        std::thread::sleep(Duration::from_millis(5));
    }
    h.run_steps(3);
}

fn click(h: &mut Harness<'static, App>, pos: egui::Pos2, b: egui::PointerButton) {
    h.input_mut().events.push(egui::Event::PointerMoved(pos));
    h.step();
    for pressed in [true, false] {
        h.input_mut().events.push(egui::Event::PointerButton {
            pos,
            button: b,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
        h.step();
    }
    h.run_steps(2);
}

/// Right-click on the header of file column `c` (order unchanged).
fn header_menu(h: &mut Harness<'static, App>, c: usize) {
    let g = &h.state().tabs[0].grid;
    let (x0, x1) = g.col_x[DC_FIRST + c];
    let p = egui::pos2((x0 + x1) / 2.0, g.header_y);
    click(h, p, egui::PointerButton::Secondary);
}

/// Centre of grid line `line` (on screen), in file column `c`.
fn line_pos(h: &Harness<'static, App>, line: u64, c: usize) -> egui::Pos2 {
    let t = &h.state().tabs[0];
    let row_h = h.state().style_row_h();
    let (x0, x1) = t.grid.col_x[DC_FIRST + c];
    egui::pos2(
        (x0 + x1) / 2.0,
        t.grid.body.min.y + (line as f32 - t.grid.top as f32 + 0.5) * row_h,
    )
}

fn root_values(h: &Harness<'static, App>) -> Vec<(String, u64)> {
    let g = h.state().tabs[0].groups.tree.clone().expect("grouped");
    (0..g.nodes.len())
        .filter(|&n| g.nodes[n].depth == 0)
        .map(|n| {
            (
                g.value(n).unwrap_or("(blank)").to_string(),
                g.nodes[n].count,
            )
        })
        .collect()
}

/// Groups by Channel then EventId, checks the defaults of a new grouping
/// (#18: collapsed, largest first), then sorts by value and opens all
/// groups for the tests below.
fn group_two_levels(h: &mut Harness<'static, App>) {
    header_menu(h, 1);
    h.get_by_label("Group by this column").click();
    settle(h);
    {
        let t = &h.state().tabs[0];
        assert_eq!(t.groups.levels[0].sort, GroupSort::CountDesc);
        assert_eq!(t.groups.lines.as_ref().unwrap().len(), 3, "all collapsed");
        assert_eq!(root_values(h)[0], ("Security".into(), 250));
    }
    // Open one group: adding a level collapses everything again.
    h.state_mut().tabs[0].groups.set_open(0, Some(true));
    h.state_mut().tabs[0].groups.rebuild();
    header_menu(h, 2);
    h.get_by_label("Group by this column (inner level)").click();
    settle(h);
    {
        let t = &h.state().tabs[0];
        assert!(
            t.groups
                .levels
                .iter()
                .all(|l| l.sort == GroupSort::CountDesc)
        );
        assert_eq!(t.groups.lines.as_ref().unwrap().len(), 3, "all collapsed");
    }
    let t = &mut h.state_mut().tabs[0];
    crate::groups::apply(t, crate::groups::Action::Sort(0, GroupSort::ValueAsc));
    crate::groups::apply(t, crate::groups::Action::Sort(1, GroupSort::ValueAsc));
    settle(h);
    crate::groups::apply(&mut h.state_mut().tabs[0], crate::groups::Action::All(true));
    h.run_steps(2);
}

#[test]
fn two_levels_then_sort_groups_by_count_desc() {
    let dir = tempfile::tempdir().unwrap();
    let file = sample(dir.path());
    let mut h = harness(dir.path(), &file);
    settle(&mut h);
    group_two_levels(&mut h);
    let t = &h.state().tabs[0];
    assert_eq!(
        t.groups.levels,
        vec![
            Level {
                col: 1,
                sort: GroupSort::ValueAsc
            },
            Level {
                col: 2,
                sort: GroupSort::ValueAsc
            }
        ]
    );
    // By value: Application, Security, System.
    assert_eq!(
        root_values(&h),
        vec![
            ("Application".into(), 100),
            ("Security".into(), 250),
            ("System".into(), 150)
        ]
    );
    // Lines: Channel header, EventId header, its rows...
    let t = &h.state().tabs[0];
    let l = t.groups.lines.clone().unwrap();
    assert!(matches!(l.at(0), Some(Line::Header(0))));
    assert!(matches!(l.at(1), Some(Line::Header(1))));
    assert_eq!(l.len(), 500 + 3 + 9, "every row, 3 + 9 headers");
    assert!(t.row_at(0).is_none() && t.row_at(2).is_some());
    let r = t.row_at(2).unwrap();
    assert_eq!(channel(r), "Application");
    // The group bar sorts the Channel level by count, largest first.
    h.get_by_label_contains("Channel  A→Z").click();
    h.run_steps(2);
    h.get_by_label("by count, largest first").click();
    settle(&mut h);
    assert_eq!(
        h.state().tabs[0].groups.levels[0].sort,
        GroupSort::CountDesc
    );
    assert_eq!(
        root_values(&h),
        vec![
            ("Security".into(), 250),
            ("System".into(), 150),
            ("Application".into(), 100)
        ]
    );
    // Rows of a leaf keep the row order (file order here), and the data
    // rows of the grouped view follow the group order.
    let t = &h.state().tabs[0];
    let rows: Vec<u32> = t.view.iter().collect();
    assert!(rows[..250].iter().all(|&r| channel(r) == "Security"));
    let first_leaf: Vec<u32> = rows.iter().copied().take_while(|&r| r % 3 == 0).collect();
    assert!(first_leaf.windows(2).all(|w| w[0] < w[1]));
    // Nested counts: each EventId group of Security, and their sum.
    let g = t.groups.tree.clone().unwrap();
    let kids: Vec<u64> = (1..g.nodes[0].end as usize)
        .map(|n| g.nodes[n].count)
        .collect();
    assert_eq!(kids.iter().sum::<u64>(), 250);
    assert_eq!(kids.len(), 3);
    // The header label shows value and count.
    assert_eq!(t.groups.label(t, 0), "Channel: Security   (250 rows)");
}

#[test]
fn collapse_keys_menu_filter_copy_and_session() {
    let dir = tempfile::tempdir().unwrap();
    let file = sample(dir.path());
    let mut h = harness(dir.path(), &file);
    settle(&mut h);
    group_two_levels(&mut h);
    // Click the first header (Channel: Application), Left closes it.
    let p = line_pos(&h, 0, 1);
    click(&mut h, p, egui::PointerButton::Primary);
    assert_eq!(h.state().tabs[0].grid.selected, Some(0));
    h.key_press(egui::Key::ArrowLeft);
    h.run_steps(2);
    let t = &h.state().tabs[0];
    assert_eq!(
        t.groups.lines.as_ref().unwrap().len(),
        500 + 3 + 9 - 100 - 3
    );
    assert!(matches!(t.groups.lines.as_ref().unwrap().at(1), Some(Line::Header(n)) if n == 4));
    h.key_press(egui::Key::ArrowRight);
    h.run_steps(2);
    assert_eq!(h.state().tabs[0].groups.lines.as_ref().unwrap().len(), 512);
    // Collapse all / expand all from the bar.
    h.get_by_label("Collapse all").click();
    h.run_steps(2);
    assert_eq!(h.state().tabs[0].groups.lines.as_ref().unwrap().len(), 3);
    h.get_by_label("Expand all").click();
    h.run_steps(2);
    // Copy of the first 4 lines (2 headers + 2 rows): data rows only.
    {
        let t = &mut h.state_mut().tabs[0];
        t.grid.rows.press(0, false, false);
        t.grid.rows.drag_to(3);
        t.grid.rows_active = true;
        let text = crate::copyfmt::copy(
            t,
            Some(crate::copyfmt::Format::Tsv),
            Default::default(),
            ',',
            false,
            &Default::default(),
        )
        .expect("copied")
        .text;
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "{text}");
        assert!(lines.iter().all(|l| l.contains("Application")));
        t.grid.clear_ranges();
    }
    // Export flattens in group order (data rows only).
    let out = dir.path().join("out.csv");
    {
        let t = &mut h.state_mut().tabs[0];
        t.start_export(
            out.clone(),
            false,
            vec![1, 2],
            fhv_engine::export::Format::Csv,
        );
    }
    let t0 = Instant::now();
    while h.state().tabs[0].job.is_some() {
        assert!(t0.elapsed() < Duration::from_secs(20));
        std::thread::sleep(Duration::from_millis(5));
        h.step();
    }
    let csv = std::fs::read_to_string(&out).unwrap();
    let body: Vec<&str> = csv.lines().skip(1).collect();
    assert_eq!(body.len(), 500);
    assert!(body[..100].iter().all(|l| l.starts_with("Application,")));
    assert!(body[0].ends_with(",4624"));
    // Group header menu: exclude the inner group EventId 4624 of
    // Application (a NOT query term), then filter to System.
    let p = line_pos(&h, 1, 1);
    click(&mut h, p, egui::PointerButton::Secondary);
    h.get_by_label("Exclude this group").click();
    settle(&mut h);
    let t = &h.state().tabs[0];
    assert_eq!(t.view.len(), 500 - 34, "{}", t.query.residual_text);
    assert!(t.query.residual.is_some());
    // Session keeps the levels.
    let s = crate::session::capture(t);
    assert_eq!(
        s.group_by,
        vec![
            ("Channel".to_string(), GroupSort::ValueAsc),
            ("EventId".to_string(), GroupSort::ValueAsc)
        ]
    );
    let json = serde_json::to_string(&s).unwrap();
    // Filter to the System group: a chip on Channel.
    let sys = {
        let g = t.groups.tree.clone().unwrap();
        (0..g.nodes.len())
            .find(|&n| g.value(n) == Some("System"))
            .unwrap()
    };
    assert!(t.groups.lines.as_ref().unwrap().line_of_node(sys).is_some());
    crate::groups::apply(
        &mut h.state_mut().tabs[0],
        crate::groups::Action::Filter(sys, false),
    );
    settle(&mut h);
    let t = &h.state().tabs[0];
    assert!(t.filters[1].values.is_some());
    assert_eq!(root_values(&h), vec![("System".into(), 150)]);
    // Clear grouping; restoring the session brings it back.
    h.get_by_label("Clear grouping").click();
    settle(&mut h);
    assert!(h.state().tabs[0].groups.lines.is_none());
    assert_eq!(h.state().tabs[0].line_count(), 150);
    let s: crate::session::Session = serde_json::from_str(&json).unwrap();
    {
        let t = &mut h.state_mut().tabs[0];
        let w = crate::session::apply(t, &s);
        assert!(w.is_empty(), "{w:?}");
    }
    settle(&mut h);
    assert_eq!(h.state().tabs[0].groups.levels.len(), 2);
    assert!(h.state().tabs[0].groups.tree.is_some());
}

/// "Hide grouped columns" (#29): the grouped columns leave the grid while
/// they are levels, come back at their place, and sessions keep both the
/// toggle and the full column layout.
#[test]
fn hide_grouped_columns_toggle() {
    let dir = tempfile::tempdir().unwrap();
    let file = sample(dir.path());
    let mut h = harness(dir.path(), &file);
    settle(&mut h);
    group_two_levels(&mut h);
    assert!(!h.state().tabs[0].groups.hide_cols, "off by default");
    assert_eq!(h.state().tabs[0].order, vec![0, 1, 2, 3]);
    h.get_by_label("Hide grouped columns").click();
    h.run_steps(2);
    let t = &h.state().tabs[0];
    assert_eq!(t.order, vec![0, 3]);
    assert!(t.hidden_after.is_empty(), "not the hidden-columns list");
    assert_eq!(t.layout_order(), vec![0, 1, 2, 3]);
    // The session keeps every column and the toggle.
    let s = crate::session::capture(t);
    assert_eq!(s.columns.len(), 4);
    assert!(s.hide_grouped);
    // Removing the inner level brings EventId back at its place.
    crate::groups::apply(&mut h.state_mut().tabs[0], crate::groups::Action::Remove(1));
    settle(&mut h);
    h.run_steps(2);
    assert_eq!(h.state().tabs[0].order, vec![0, 2, 3]);
    // Toggle off: all back.
    h.get_by_label("Hide grouped columns").click();
    h.run_steps(2);
    assert_eq!(h.state().tabs[0].order, vec![0, 1, 2, 3]);
    // Restoring the session hides both levels again.
    let w = crate::session::apply(&mut h.state_mut().tabs[0], &s);
    assert!(w.is_empty(), "{w:?}");
    settle(&mut h);
    h.run_steps(2);
    let t = &h.state().tabs[0];
    assert!(t.groups.hide_cols);
    assert_eq!(t.order, vec![0, 3]);
    // Ungrouping shows them again.
    crate::groups::apply(&mut h.state_mut().tabs[0], crate::groups::Action::Clear);
    settle(&mut h);
    h.run_steps(2);
    assert_eq!(h.state().tabs[0].order, vec![0, 1, 2, 3]);
}
