//! UI tests for coloring rules (D-065) and duplicate / unique highlighting
//! (D-066, D-067).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};

use crate::app::App;
use crate::dupes::{Mode, Scope as DupScope};
use crate::rules::{self, Rule, Scope};
use crate::settings::Settings;

fn sample(dir: &Path) -> PathBuf {
    let p = dir.join("events.csv");
    let mut s = String::from("Time,Info,Host,Msg\n");
    for i in 0..300u32 {
        let host = if i == 7 {
            "lonely"
        } else {
            ["dc01", "ws02"][(i % 2) as usize]
        };
        let msg = if i < 200 {
            format!("m{}", i / 2)
        } else {
            format!("u{i}")
        };
        s.push_str(&format!(
            "2024-03-11 10:{:02}:{:02},{},{host},{msg}\n",
            i / 60 % 60,
            i % 60,
            ["Critical error on disk", "ok", "service CRITICAL", "info"][(i % 4) as usize],
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
        assert!(t0.elapsed() < Duration::from_secs(20), "timed out");
        std::thread::sleep(Duration::from_millis(5));
        h.step();
    }
    h.run_steps(2);
}

fn idle(a: &App) -> bool {
    let t = &a.tabs[0];
    t.ready() && t.job.is_none() && !t.view.is_empty()
}

fn with_rules(rules: Vec<Rule>) -> Settings {
    Settings {
        coloring_rules: rules,
        ..Settings::default()
    }
}

#[test]
fn info_contains_critical_is_red() {
    let dir = tempfile::tempdir().unwrap();
    let file = sample(dir.path());
    let mut h = harness(
        dir.path(),
        &file,
        with_rules(vec![Rule {
            when: "Info contains Critical".into(),
            ..Rule::default()
        }]),
    );
    wait(&mut h, idle);
    let th = crate::theme::current(&h.ctx);
    let t = &h.state().tabs[0];
    for row in 0..20u64 {
        let critical = row % 4 == 0 || row % 4 == 2;
        let want = critical.then_some(th.rules.red.bg.0);
        assert_eq!(rules::row_color(t, row), want, "row {row}");
        // Text in the palette's color on it; cells of other rows untouched.
        let st = rules::cell_style(t, row, 1);
        assert_eq!(st.fg, critical.then_some(th.rules.red.fg.0));
        assert_eq!(st.bg, None, "the row fill is the row background");
    }
    assert!(t.colors.errors().iter().all(Option::is_none));
}

#[test]
fn first_rule_wins_per_cell_and_cell_scope() {
    let dir = tempfile::tempdir().unwrap();
    let file = sample(dir.path());
    let mut h = harness(
        dir.path(),
        &file,
        with_rules(vec![
            Rule {
                when: "* contains disk".into(),
                color: "blue".into(),
                scope: Scope::Cell,
                bold: true,
                ..Rule::default()
            },
            Rule {
                when: "Info contains critical".into(),
                ..Rule::default()
            },
            Rule {
                when: "Host = dc01 cs".into(),
                color: "green".into(),
                ..Rule::default()
            },
            Rule {
                when: "NoSuchColumn = 1".into(),
                ..Rule::default()
            },
            Rule {
                enabled: false,
                when: "Info = ok".into(),
                ..Rule::default()
            },
        ]),
    );
    wait(&mut h, idle);
    let th = crate::theme::current(&h.ctx);
    let t = &h.state().tabs[0];
    // Row 0: "Critical error on disk" / dc01: the Info cell is blue (cell
    // rule first), the row red (first row rule), the Host cell red too.
    let info = rules::cell_style(t, 0, 1);
    assert_eq!(info.bg, Some(th.rules.blue.bg.0));
    assert!(info.bold);
    assert_eq!(rules::row_color(t, 0), Some(th.rules.red.bg.0));
    assert_eq!(rules::cell_style(t, 0, 2).bg, None);
    assert!(
        rules::cell_tip(t, 0, 1)
            .unwrap()
            .contains("rule 1: * contains disk")
    );
    // Row 4 is like row 0; row 6 ("info", ws02): no rule; row 8? Host dc01
    // and "Critical...": red. Row 1 ("ok", ws02): the disabled rule is off.
    assert_eq!(rules::row_color(t, 1), None);
    assert_eq!(rules::row_color(t, 3), None);
    // Row 2: "service CRITICAL" (case ignored), dc01: red wins over green.
    assert_eq!(rules::row_color(t, 2), Some(th.rules.red.bg.0));
    // The unknown column is reported, the others apply.
    let errs = t.colors.errors();
    assert!(errs[3].as_deref().unwrap().contains("NoSuchColumn"));
    assert!(errs[..3].iter().all(Option::is_none) && errs[4].is_none());
}

#[test]
fn rules_persist_in_sessions_and_settings() {
    let dir = tempfile::tempdir().unwrap();
    let file = sample(dir.path());
    let r = vec![Rule {
        when: "Info contains Critical".into(),
        color: "purple".into(),
        scope: Scope::Cell,
        ..Rule::default()
    }];
    let mut h = harness(dir.path(), &file, with_rules(r.clone()));
    wait(&mut h, idle);
    let json = serde_json::to_string(h.state().settings_for_test()).unwrap();
    let back: Settings = serde_json::from_str(&json).unwrap();
    assert_eq!(back.coloring_rules, r);
    // Older settings files have no rules.
    let old: Settings = serde_json::from_str(r#"{"tz": "UTC"}"#).unwrap();
    assert!(old.coloring_rules.is_empty());

    h.state_mut().tabs[0]
        .dupes
        .set(2, Mode::Both, DupScope::File);
    let s = crate::session::capture(&h.state().tabs[0]);
    let text = serde_json::to_string(&s).unwrap();
    let s: crate::session::Session = serde_json::from_str(&text).unwrap();
    assert_eq!(s.rules.as_deref(), Some(&r[..]));
    assert_eq!(
        s.highlights,
        vec![("Host".into(), Mode::Both, DupScope::File)]
    );
    let t = &mut h.state_mut().tabs[0];
    t.dupes = Default::default();
    assert!(crate::session::apply(t, &s).is_empty());
    assert_eq!(t.dupes.get(2), Some((Mode::Both, DupScope::File)));
}

#[test]
fn editor_adds_a_rule_and_cell_menu_prefills() {
    let dir = tempfile::tempdir().unwrap();
    let file = sample(dir.path());
    let mut h = harness(dir.path(), &file, Settings::default());
    wait(&mut h, idle);
    h.state_mut().rules_dlg = Some(rules::Editor::with_draft("Host = dc01 cs".into()));
    h.run_steps(2);
    h.get_by_label("Add rule").click();
    h.run_steps(3);
    assert_eq!(h.state().settings_for_test().coloring_rules.len(), 1);
    assert_eq!(h.state().rules.len(), 1);
    let th = crate::theme::current(&h.ctx);
    assert_eq!(
        rules::row_color(&h.state().tabs[0], 0),
        Some(th.rules.red.bg.0)
    );
    assert_eq!(rules::row_color(&h.state().tabs[0], 1), None);
    // A bad condition cannot be added.
    h.get_by_label("new rule condition").focus();
    h.get_by_label("new rule condition").type_text("Host ==");
    h.run_steps(2);
    assert!(h.get_by_label("Add rule").accesskit_node().is_disabled());
}

#[test]
fn duplicates_and_unique_values_are_highlighted() {
    let dir = tempfile::tempdir().unwrap();
    let file = sample(dir.path());
    let mut h = harness(
        dir.path(),
        &file,
        with_rules(vec![Rule {
            when: "Info = ok".into(),
            ..Rule::default()
        }]),
    );
    wait(&mut h, idle);
    h.state_mut().tabs[0]
        .dupes
        .set(2, Mode::Both, DupScope::View);
    h.state_mut().tabs[0]
        .dupes
        .set(3, Mode::Unique, DupScope::View);
    wait(&mut h, |a| idle(a) && a.tabs[0].dupes.ready().len() == 2);
    let th = crate::theme::current(&h.ctx);
    let t = &h.state().tabs[0];
    // Host: "lonely" (row 7) is unique, dc01 duplicated.
    let lonely = rules::cell_style(t, 7, 2);
    assert_eq!(lonely.bg, Some(th.unique.bg.0));
    assert_eq!(lonely.marker, Some(th.unique.fg.0));
    assert_eq!(rules::cell_style(t, 0, 2).bg, Some(th.duplicate.bg.0));
    assert!(
        rules::cell_tip(t, 0, 2)
            .unwrap()
            .contains("150 occurrences in the filtered view")
    );
    // Row 1 is red by a rule: the rule wins, the marker stays (D-067).
    let ruled = rules::cell_style(t, 1, 2);
    assert_eq!(ruled.bg, None);
    assert_eq!(ruled.marker, Some(th.duplicate.fg.0));
    // Msg: m0 appears twice, only unique values are asked for.
    assert_eq!(
        rules::cell_style(t, 0, 3),
        crate::grid::CellStyle::default()
    );
    assert!(
        crate::dupes::Dupes::header_tip(&t.dupes, 2)
            .unwrap()
            .contains("1 unique")
    );

    // Counts follow the filtered view: only rows 0..100 (Msg m0..m49).
    h.state_mut().tabs[0].filters[3]
        .conds
        .push(fhv_engine::Op::Regex("^m[0-4]?[0-9]$".into()));
    h.state_mut().tabs[0].requery();
    wait(&mut h, |a| {
        idle(a) && a.tabs[0].view.len() == 100 && a.tabs[0].dupes.ready().len() == 2
    });
    let t = &h.state().tabs[0];
    assert!(rules::cell_tip(t, 0, 2).unwrap().contains("50 occurrences"));
    assert_eq!(t.dupes.header_mark(2), " [dup+uniq]");
}

/// Named rule sets (#20): save as, modified state, save, load, rename,
/// delete; the View menu switches sets.
#[test]
fn rule_sets_save_load_rename_delete() {
    let dir = tempfile::tempdir().unwrap();
    let file = sample(dir.path());
    let crit = Rule {
        when: "Info contains Critical".into(),
        ..Rule::default()
    };
    let mut h = harness(dir.path(), &file, with_rules(vec![crit.clone()]));
    wait(&mut h, idle);
    h.state_mut().rules_dlg = Some(Default::default());
    h.run_steps(2);
    h.get_by_label_contains("changes are saved as you make them");
    h.get_by_label("set name").focus();
    h.run_steps(1);
    h.get_by_label("set name").type_text("Logons");
    h.run_steps(1);
    h.get_by_label("Save as set").click();
    h.run_steps(2);
    let s = &h.state().settings;
    assert_eq!(s.rule_set.as_deref(), Some("Logons"));
    assert_eq!(s.rule_sets["Logons"], vec![crit.clone()]);
    // Editing the rules marks the set modified; Save updates it.
    h.state_mut().settings.coloring_rules[0].color = "blue".into();
    h.run_steps(2);
    h.get_by_label("Save").click();
    h.run_steps(2);
    assert_eq!(h.state().settings.rule_sets["Logons"][0].color, "blue");
    // Another set, then load the first one back from the View menu.
    h.state_mut().settings.coloring_rules = Vec::new();
    h.state_mut().settings.rule_set = None;
    h.state_mut()
        .settings
        .rule_sets
        .insert("Empty".into(), Vec::new());
    h.state_mut().rules_dlg = None;
    h.run_steps(2);
    h.get_by_label("View").click();
    h.run_steps(2);
    h.get_by_label("Rule set").hover();
    h.run_steps(2);
    h.get_by_label("Logons").click();
    h.run_steps(2);
    let s = &h.state().settings;
    assert_eq!(s.rule_set.as_deref(), Some("Logons"));
    assert_eq!(s.coloring_rules.len(), 1);
    assert_eq!(h.state().rules.len(), 1, "tabs use the loaded set");
    // Rename, then delete.
    h.state_mut().rules_dlg = Some(Default::default());
    h.run_steps(2);
    h.get_by_label("set name").focus();
    h.run_steps(1);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.get_by_label("set name").type_text("Logon events");
    h.run_steps(2);
    h.get_by_label("Rename").click();
    h.run_steps(2);
    let s = &h.state().settings;
    assert!(s.rule_sets.contains_key("Logon events") && !s.rule_sets.contains_key("Logons"));
    assert_eq!(s.rule_set.as_deref(), Some("Logon events"));
    h.get_by_label("Delete set").click();
    h.run_steps(2);
    let s = &h.state().settings;
    assert!(!s.rule_sets.contains_key("Logon events"));
    assert_eq!(s.coloring_rules.len(), 1, "the current rules stay");
}
