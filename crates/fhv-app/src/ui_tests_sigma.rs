//! Sigma detection in the UI (D-101..D-106): rules from a folder, run on a
//! forhunt-layout CSV, gutter levels, the panel filter and its chip, the
//! detail pane, and the CSV of any other layout.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;

use crate::app::App;
use crate::settings::Settings;

const RULES: &[(&str, &str)] = &[
    (
        "enc.yml",
        "title: Encoded PowerShell\nid: t-enc\nlevel: high\nstatus: test\nauthor: Test Author\n\
         description: encoded command line\ntags: [attack.t1059.001]\n\
         detection:\n  sel:\n    Channel: Security\n    EventID: 4688\n    CommandLine|contains|windash: ' -enc'\n  condition: sel\n",
    ),
    (
        "logon.yml",
        "title: Network Logon\nid: t-logon\nlevel: low\nstatus: test\nauthor: Someone\n\
         detection:\n  sel:\n    Channel: Security\n    EventID: 4624\n    LogonType: 3\n  condition: sel\n",
    ),
    (
        "any.yml",
        "title: Image Anywhere\nid: t-img\nlevel: medium\nstatus: test\nauthor: Someone\n\
         detection:\n  sel:\n    Image|endswith: '\\evil.exe'\n  condition: sel\n",
    ),
];

fn rules_dir(dir: &Path) -> PathBuf {
    let d = dir.join("myrules");
    std::fs::create_dir_all(&d).unwrap();
    for (n, t) in RULES {
        std::fs::write(d.join(n), t).unwrap();
    }
    d
}

/// forhunt CSV layout (header of `forhunt parse -F csv`) with JSON payloads.
fn forhunt_csv(dir: &Path) -> PathBuf {
    let p = dir.join("timeline.csv");
    let mut s = String::from(
        "RecordNumber,EventRecordId,TimeCreated,EventId,Level,Provider,Channel,ProcessId,ThreadId,Computer,UserId,MapDescription,UserName,RemoteHost,PayloadData1,PayloadData2,PayloadData3,PayloadData4,PayloadData5,PayloadData6,ExecutableInfo,HiddenRecord,SourceFile,Keywords,Payload,ForhuntId\n",
    );
    for i in 0..300u32 {
        let (eid, payload, pd1) = match i % 3 {
            0 => (
                4688,
                format!(
                    "{{\"\"CommandLine\"\":\"\"powershell.exe {} AAAA\"\",\"\"NewProcessName\"\":\"\"x\"\"}}",
                    if i % 30 == 0 { "/Enc" } else { "-nop" }
                ),
                "CommandLine: powershell.exe",
            ),
            1 => (
                4624,
                format!(
                    "{{\"\"TargetUserName\"\":\"\"bob\"\",\"\"LogonType\"\":{}}}",
                    if i % 2 == 0 { 3 } else { 2 }
                ),
                "TargetUserName: bob",
            ),
            _ => (
                4634,
                "{\"\"TargetUserName\"\":\"\"bob\"\"}".to_string(),
                "TargetUserName: bob",
            ),
        };
        s.push_str(&format!(
            "{i},{i},2024-03-11 10:00:{:02}.0000000,{eid},Info,Microsoft-Windows-Security-Auditing,Security,4,8,WS-07,,T,bob,,{pd1},,,,,,,False,Security.evtx,0x8020000000000000,\"{payload}\",abcd1234:{i}\n",
            i % 60
        ));
    }
    std::fs::write(&p, s).unwrap();
    p
}

fn harness_with(dir: &Path, file: &Path, settings: Settings) -> Harness<'static, App> {
    let file = file.to_path_buf();
    let cache = dir.join("cache");
    Harness::builder()
        .with_size([1500.0, 800.0])
        .build_eframe(move |cc| App::with(cc, vec![file.clone()], settings.clone(), cache.clone()))
}

fn wait(h: &mut Harness<'static, App>, what: &str, done: impl Fn(&App) -> bool) {
    let t0 = Instant::now();
    while !done(h.state()) {
        assert!(t0.elapsed() < Duration::from_secs(30), "timed out: {what}");
        std::thread::sleep(Duration::from_millis(5));
        h.step();
    }
    h.step();
}

fn settings_with(rules: &Path) -> Settings {
    let mut s = Settings::default();
    s.sigma.folders = vec![rules.to_path_buf()];
    s.sigma.min_level = 1;
    s
}

#[test]
fn detect_mark_filter_and_explain() {
    let dir = tempfile::tempdir().unwrap();
    let rules = rules_dir(dir.path());
    let file = forhunt_csv(dir.path());
    let mut h = harness_with(dir.path(), &file, settings_with(&rules));
    wait(&mut h, "index and rules", |a| {
        a.tabs[0].ready() && a.tabs[0].job.is_none() && a.sigma.ready().is_some()
    });
    assert_eq!(h.state().sigma.ready().unwrap().0.len(), 3);
    // Run from the toolbar.
    h.get_by_label("Detect").click();
    h.step();
    wait(&mut h, "detection", |a| {
        a.tabs[0].sigma.result.is_some() && !a.tabs[0].sigma.running()
    });
    let res = h.state().tabs[0].sigma.result.clone().unwrap();
    // 4688 rows with "/Enc" (i % 30 == 0): 10; 4624 with LogonType 3: 50.
    let count = |id: &str| {
        let i = res.set.rules().iter().position(|r| r.id == id).unwrap() as u32;
        res.hits
            .iter()
            .find(|(r, _)| *r == i)
            .map_or(0, |(_, b)| b.len())
    };
    assert_eq!(count("t-enc"), 10);
    assert_eq!(count("t-logon"), 50);
    assert_eq!(count("t-img"), 0);
    assert_eq!(res.all.len(), 60);
    // Only 4688 and 4624 rows were read (dictionary candidates): the
    // Image rule names no channel, so every row is a candidate for it.
    assert_eq!(res.read, 300);
    // The panel lists the rules; clicking one filters the view to it.
    h.get_by_label_contains("Encoded PowerShell (10)").click();
    h.step();
    wait(&mut h, "rule filter", |a| {
        a.tabs[0].job.is_none() && a.tabs[0].view.len() == 10
    });
    assert!(
        h.query_by_label_contains("Sigma: Encoded PowerShell")
            .is_some(),
        "chip shown"
    );
    // Select the first row: the detail pane names the rule and its author.
    {
        let t = &mut h.state_mut().tabs[0];
        t.grid.selected = Some(0);
    }
    h.step();
    h.step();
    assert!(h.query_by_label_contains("by Test Author").is_some());
    // The level of the visible rows is painted (gutter marker).
    assert_eq!(h.state().tabs[0].sigma.paint.level(0), 4);
    // Invert the chip: rows without that detection.
    h.get_by_label_contains("Sigma: Encoded PowerShell").click();
    h.step();
    wait(&mut h, "inverted", |a| {
        a.tabs[0].job.is_none() && a.tabs[0].view.len() == 290
    });
    // Clear filters removes it.
    h.get_by_label("Clear filters").click();
    h.step();
    wait(&mut h, "cleared", |a| {
        a.tabs[0].job.is_none() && a.tabs[0].view.len() == 300
    });
    assert!(h.state().tabs[0].sigma.focus.is_none());
    // Levels: "high+" keeps the 10 high rows.
    h.get_by_label_contains("high+ (10)").click();
    h.step();
    wait(&mut h, "level filter", |a| {
        a.tabs[0].job.is_none() && a.tabs[0].view.len() == 10
    });
}

#[test]
fn any_csv_columns_are_fields_and_settings_reload() {
    let dir = tempfile::tempdir().unwrap();
    let rules = rules_dir(dir.path());
    let file = dir.path().join("edr.csv");
    let mut s = String::from("Time,Host,Image,CommandLine\n");
    for i in 0..50 {
        s.push_str(&format!(
            "2024-03-11T10:00:{:02}Z,ws1,C:\\x\\{}.exe,run {i}\n",
            i % 60,
            if i % 10 == 0 { "evil" } else { "good" }
        ));
    }
    std::fs::write(&file, s).unwrap();
    let mut h = harness_with(dir.path(), &file, settings_with(&rules));
    wait(&mut h, "ready", |a| {
        a.tabs[0].ready() && a.tabs[0].job.is_none() && a.sigma.ready().is_some()
    });
    h.state_mut().run_sigma();
    wait(&mut h, "detection", |a| {
        a.tabs[0].sigma.result.is_some() && !a.tabs[0].sigma.running()
    });
    let res = h.state().tabs[0].sigma.result.clone().unwrap();
    assert_eq!(
        res.all.len(),
        5,
        "the Image column is the event's Image field"
    );
    assert!(
        res.note
            .as_deref()
            .unwrap()
            .contains("No Channel / EventId column")
    );
    // A tested field's cell is marked: column Image (2) of a detected row.
    wait(&mut h, "view", |a| {
        a.tabs[0].job.is_none() && a.tabs[0].view.len() == 50
    });
    let first = res.all.min().unwrap();
    let pos = h.state().tabs[0].view.position_of(first).unwrap();
    {
        let t = &mut h.state_mut().tabs[0];
        t.grid.top = pos as f64;
        t.repaint_cells();
    }
    h.step();
    h.step();
    assert!(h.state().tabs[0].sigma.paint.tested(pos, 2));
    // Raising the minimum level reloads the rules: results become stale.
    let generation = h.state().sigma.generation;
    h.state_mut().settings.sigma.min_level = 4;
    wait(&mut h, "reload", |a| {
        a.sigma.generation > generation && !a.sigma.loading()
    });
    assert_eq!(h.state().sigma.ready().unwrap().0.len(), 1);
    assert!(h.state().tabs[0].sigma.stale(h.state().sigma.generation));
    assert!(h.query_all_by_label_contains("Re-run").count() > 0);
}

#[test]
fn detect_needs_rules() {
    let dir = tempfile::tempdir().unwrap();
    let file = forhunt_csv(dir.path());
    let mut h = harness_with(dir.path(), &file, Settings::default());
    wait(&mut h, "ready", |a| {
        a.tabs[0].ready() && a.tabs[0].job.is_none()
    });
    assert!(h.state().sigma.ready().is_none());
    // Detect without rules opens Settings at the rules section.
    h.get_by_label("Detect").click();
    h.step();
    assert!(h.state().appearance.settings_open);
    h.step();
    assert!(h.query_by_label("Download rules").is_some());
}

/// Opt-in: the viewer and `forhunt sigma` give the same detections on the
/// same logs (`forhunt parse -F csv` of them). FHV_SIGMA_CSV=<csv>,
/// FHV_SIGMA_RULES=<hayabusa-rules>, FHV_SIGMA_CLI=<forhunt sigma -F jsonl -n 0 output>.
#[test]
#[ignore]
fn same_detections_as_forhunt_cli() {
    let (Ok(csv), Ok(rules)) = (
        std::env::var("FHV_SIGMA_CSV"),
        std::env::var("FHV_SIGMA_RULES"),
    ) else {
        return;
    };
    let cli = std::env::var("FHV_SIGMA_CLI").ok();
    let dir = tempfile::tempdir().unwrap();
    let mut s = Settings::default();
    s.sigma.folders = vec![PathBuf::from(&rules)];
    s.sigma.min_level = 2;
    let mut h = harness_with(dir.path(), Path::new(&csv), s);
    let t0 = Instant::now();
    while !(h.state().tabs[0].ready()
        && h.state().tabs[0].job.is_none()
        && h.state().sigma.ready().is_some())
    {
        assert!(t0.elapsed() < Duration::from_secs(300));
        std::thread::sleep(Duration::from_millis(20));
        h.step();
    }
    h.state_mut().run_sigma();
    while !(h.state().tabs[0].sigma.result.is_some() && !h.state().tabs[0].sigma.running()) {
        assert!(t0.elapsed() < Duration::from_secs(600));
        std::thread::sleep(Duration::from_millis(20));
        h.step();
    }
    let res = h.state().tabs[0].sigma.result.clone().unwrap();
    eprintln!(
        "viewer: {} rows, {} rows detected, {} rows read, {:.0} ms",
        h.state().tabs[0].table.row_count(),
        res.all.len(),
        res.read,
        res.ms
    );
    let Some(cli) = cli else {
        return;
    };
    // (ForhuntId, rule id) pairs on both sides.
    let t = &h.state().tabs[0];
    let idc = t
        .table
        .columns
        .iter()
        .position(|c| c == "ForhuntId")
        .unwrap();
    let mut mine = std::collections::BTreeSet::new();
    for (r, rows) in &res.hits {
        let rid = &res.set.rules()[*r as usize].id;
        for row in rows {
            let f = t.table.row_fields(row as u64).unwrap();
            mine.insert((f[idc].clone(), rid.clone()));
        }
    }
    let mut theirs = std::collections::BTreeSet::new();
    for l in std::fs::read_to_string(&cli).unwrap().lines() {
        let v: serde_json::Value = serde_json::from_str(l).unwrap();
        if let (Some(rid), Some(id)) = (v["rid"].as_str(), v["event"]["id"].as_str()) {
            theirs.insert((id.to_string(), rid.to_string()));
        }
    }
    let only_mine: Vec<_> = mine.difference(&theirs).take(10).collect();
    let only_theirs: Vec<_> = theirs.difference(&mine).take(10).collect();
    eprintln!("viewer {} pairs, cli {} pairs", mine.len(), theirs.len());
    assert!(
        only_mine.is_empty() && only_theirs.is_empty(),
        "viewer only: {only_mine:?}\ncli only: {only_theirs:?}"
    );
}
