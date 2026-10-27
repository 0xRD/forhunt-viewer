//! One case per supported date format (docs/DATE-FORMATS.md): the column
//! is detected (or set by hand), every cell parses, the parsed time shown
//! as `yyyy-MM-dd HH:mm:ss.fffffff` reads back as the same instant, a sort
//! follows the instants, and a range filter keeps exactly the rows inside.

use std::sync::Arc;

use fhv_engine::columns::Kind;
use fhv_engine::filter::{Engine, Expr, Op, Pred, Target};
use fhv_engine::time::{self, Fmt, Order, Pattern, Shape, TICKS_PER_SEC};
use fhv_engine::{OpenOptions, Query, SortKey, Table};

const N: i64 = 40;
const DAY: i64 = 86_400 * TICKS_PER_SEC;
/// 2024-03-01T00:00:00Z
const T0: i64 = 1_709_251_200 * TICKS_PER_SEC;

struct C {
    y: i64,
    mo: u32,
    d: u32,
    h: u32,
    mi: u32,
    s: u32,
    /// 100 ns
    f: i64,
}

fn civil(t: i64) -> C {
    let secs = t.div_euclid(TICKS_PER_SEC);
    let (y, mo, d) = time::civil_from_days(secs.div_euclid(86_400));
    let sod = secs.rem_euclid(86_400) as u32;
    C {
        y,
        mo,
        d,
        h: sod / 3600,
        mi: sod / 60 % 60,
        s: sod % 60,
        f: t.rem_euclid(TICKS_PER_SEC),
    }
}

const MON: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
const MOIS: [&str; 12] = [
    "janvier",
    "février",
    "mars",
    "avril",
    "mai",
    "juin",
    "juillet",
    "août",
    "septembre",
    "octobre",
    "novembre",
    "décembre",
];
const WD: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];

fn wd(t: i64) -> &'static str {
    WD[t.div_euclid(DAY).rem_euclid(7) as usize]
}

fn h12(c: &C) -> (u32, &'static str) {
    let h = if c.h.is_multiple_of(12) { 12 } else { c.h % 12 };
    (h, if c.h < 12 { "AM" } else { "PM" })
}

struct Case {
    name: &'static str,
    header: &'static str,
    /// Spacing of the generated instants (also their precision).
    step: i64,
    write: fn(i64) -> String,
    /// Expected detection (None: the analyst sets `set` by hand).
    detect: Option<fn(Fmt) -> bool>,
    set: Option<fn() -> Fmt>,
}

fn cases() -> Vec<Case> {
    let s = TICKS_PER_SEC;
    let text = |f: Fmt| f.shape == Shape::Text;
    vec![
        Case {
            name: "iso-z",
            header: "Time",
            step: 3_601 * s,
            write: |t| {
                let c = civil(t);
                format!(
                    "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
                    c.y, c.mo, c.d, c.h, c.mi, c.s
                )
            },
            detect: Some(text),
            set: None,
        },
        Case {
            name: "iso-offset",
            header: "Time",
            step: 3_601 * s,
            write: |t| {
                let c = civil(t + 2 * 3600 * TICKS_PER_SEC);
                format!(
                    "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}+02:00",
                    c.y, c.mo, c.d, c.h, c.mi, c.s
                )
            },
            detect: Some(text),
            set: None,
        },
        Case {
            name: "iso-offset-basic",
            header: "Time",
            step: 3_601 * s,
            write: |t| {
                let c = civil(t - 5 * 3600 * TICKS_PER_SEC);
                format!(
                    "{:04}-{:02}-{:02} {:02}:{:02}:{:02}-0500",
                    c.y, c.mo, c.d, c.h, c.mi, c.s
                )
            },
            detect: Some(text),
            set: None,
        },
        Case {
            name: "iso-ms",
            header: "Time",
            step: 77_770_000,
            write: |t| {
                let c = civil(t);
                let (y, mo, d, h, mi, sec, f) = (c.y, c.mo, c.d, c.h, c.mi, c.s, c.f / 10_000);
                format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{sec:02}.{f:03}Z")
            },
            detect: Some(text),
            set: None,
        },
        Case {
            name: "iso-us",
            header: "Time",
            step: 77_777_770,
            write: |t| {
                let c = civil(t);
                let (y, mo, d, h, mi, sec, f) = (c.y, c.mo, c.d, c.h, c.mi, c.s, c.f / 10);
                format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{sec:02}.{f:06}")
            },
            detect: Some(text),
            set: None,
        },
        Case {
            name: "iso-7",
            header: "TimeCreated",
            step: 77_777_777,
            write: |t| time::format(t, 0),
            detect: Some(text),
            set: None,
        },
        Case {
            name: "iso-ns",
            header: "Time",
            step: 77_777_777,
            write: |t| {
                let c = civil(t);
                let (y, mo, d, h, mi, sec, f) = (c.y, c.mo, c.d, c.h, c.mi, c.s, c.f * 100);
                format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{sec:02}.{f:09}Z")
            },
            detect: Some(text),
            set: None,
        },
        Case {
            name: "iso-date",
            header: "Day",
            step: DAY,
            write: |t| {
                let c = civil(t);
                format!("{:04}-{:02}-{:02}", c.y, c.mo, c.d)
            },
            detect: Some(text),
            set: None,
        },
        Case {
            name: "iso-basic",
            header: "Time",
            step: 3_601 * s,
            write: |t| {
                let c = civil(t);
                format!(
                    "{:04}{:02}{:02}T{:02}{:02}{:02}Z",
                    c.y, c.mo, c.d, c.h, c.mi, c.s
                )
            },
            detect: Some(|f| f.hint == time::Hint::IsoBasic),
            set: None,
        },
        Case {
            name: "rfc2822",
            header: "Date",
            step: 3_601 * s,
            write: |t| {
                let c = civil(t);
                let m = MON[c.mo as usize - 1];
                format!(
                    "{}, {:02} {m} {:04} {:02}:{:02}:{:02} GMT",
                    wd(t),
                    c.d,
                    c.y,
                    c.h,
                    c.mi,
                    c.s
                )
            },
            detect: Some(|f| f.hint == time::Hint::Rfc2822),
            set: None,
        },
        Case {
            name: "slash-dmy",
            header: "Date",
            step: 86_461 * s,
            write: |t| {
                let c = civil(t);
                format!(
                    "{:02}/{:02}/{:04} {:02}:{:02}:{:02}",
                    c.d, c.mo, c.y, c.h, c.mi, c.s
                )
            },
            detect: Some(|f| f.order == Order::Dmy),
            set: None,
        },
        Case {
            name: "slash-mdy-12h",
            header: "Date",
            step: 86_461 * s,
            write: |t| {
                let c = civil(t);
                let (h, ap) = h12(&c);
                format!(
                    "{}/{}/{:04} {h}:{:02}:{:02} {ap}",
                    c.mo, c.d, c.y, c.mi, c.s
                )
            },
            detect: Some(|f| f.order == Order::Mdy),
            set: None,
        },
        Case {
            name: "dots",
            header: "Datum",
            step: 86_461 * s,
            write: |t| {
                let c = civil(t);
                format!(
                    "{:02}.{:02}.{:04} {:02}:{:02}:{:02}",
                    c.d, c.mo, c.y, c.h, c.mi, c.s
                )
            },
            detect: Some(text),
            set: None,
        },
        Case {
            name: "dd-mon-yyyy",
            header: "Date",
            step: DAY,
            write: |t| {
                let c = civil(t);
                format!(
                    "{:02}-{}-{:04}",
                    c.d,
                    MON[c.mo as usize - 1].to_uppercase(),
                    c.y
                )
            },
            detect: Some(|f| f.hint == time::Hint::Textual),
            set: None,
        },
        Case {
            name: "mon-d-yyyy",
            header: "Date",
            step: 3_601 * s,
            write: |t| {
                let c = civil(t);
                let (h, ap) = h12(&c);
                let m = MON[c.mo as usize - 1];
                format!("{m} {}, {:04} {h}:{:02}:{:02} {ap}", c.d, c.y, c.mi, c.s)
            },
            detect: Some(|f| f.hint == time::Hint::Textual),
            set: None,
        },
        Case {
            name: "french",
            header: "Date",
            step: 86_461 * s,
            write: |t| {
                let c = civil(t);
                let m = MOIS[c.mo as usize - 1];
                format!("{} {m} {:04} {:02}:{:02}:{:02}", c.d, c.y, c.h, c.mi, c.s)
            },
            detect: Some(|f| f.hint == time::Hint::Textual),
            set: None,
        },
        Case {
            name: "asctime",
            header: "Date",
            step: 3_601 * s,
            write: |t| {
                let c = civil(t);
                let m = MON[c.mo as usize - 1];
                format!(
                    "{} {m} {:2} {:02}:{:02}:{:02} {:04}",
                    wd(t),
                    c.d,
                    c.h,
                    c.mi,
                    c.s,
                    c.y
                )
            },
            detect: Some(|f| f.hint == time::Hint::Textual),
            set: None,
        },
        Case {
            name: "clf",
            header: "time",
            step: 3_601 * s,
            write: |t| {
                let c = civil(t + 3600 * TICKS_PER_SEC);
                let m = MON[c.mo as usize - 1];
                format!(
                    "[{:02}/{m}/{:04}:{:02}:{:02}:{:02} +0100]",
                    c.d, c.y, c.h, c.mi, c.s
                )
            },
            detect: Some(|f| f.hint == time::Hint::Clf),
            set: None,
        },
        Case {
            name: "syslog",
            header: "Timestamp",
            step: 3_601 * s,
            write: |t| {
                let c = civil(t);
                let m = MON[c.mo as usize - 1];
                format!("{m} {:2} {:02}:{:02}:{:02}", c.d, c.h, c.mi, c.s)
            },
            detect: Some(|f| f.hint == time::Hint::YearLess && f.year_assumed()),
            set: Some(|| Fmt {
                year: 2024,
                hint: time::Hint::YearLess,
                ..Fmt::default()
            }),
        },
        Case {
            name: "epoch-s",
            header: "ts",
            step: 3_601 * s,
            write: |t| (t / TICKS_PER_SEC).to_string(),
            detect: Some(|f| f.shape == Shape::EpochS),
            set: None,
        },
        Case {
            name: "epoch-s-decimals",
            header: "ts",
            step: 36_015 * s / 10,
            write: |t| format!("{}.{}", t / TICKS_PER_SEC, t % TICKS_PER_SEC / 1_000_000),
            detect: Some(|f| f.shape == Shape::EpochS),
            set: None,
        },
        Case {
            name: "epoch-ms",
            header: "created",
            step: 3_601_001 * 10_000,
            write: |t| (t / 10_000).to_string(),
            detect: Some(|f| f.shape == Shape::EpochMs),
            set: None,
        },
        Case {
            name: "epoch-us",
            header: "x",
            step: 3_601_000_001 * 10,
            write: |t| (t / 10).to_string(),
            detect: Some(|f| f.shape == Shape::EpochUs),
            set: None,
        },
        Case {
            name: "epoch-ns",
            header: "x",
            step: 3_601_000_001,
            write: |t| (t as i128 * 100).to_string(),
            detect: Some(|f| f.shape == Shape::EpochNs),
            set: None,
        },
        Case {
            name: "filetime",
            header: "x",
            step: 3_601_000_001,
            write: |t| (t + 116_444_736_000_000_000).to_string(),
            detect: Some(|f| f.shape == Shape::FileTime),
            set: None,
        },
        Case {
            name: "dotnet-ticks",
            header: "x",
            step: 3_601_000_001,
            write: |t| (t + 621_355_968_000_000_000).to_string(),
            detect: Some(|f| f.shape == Shape::Ticks),
            set: None,
        },
        Case {
            name: "webkit",
            header: "last_visit_time",
            step: 3_601_000_001 * 10,
            write: |t| ((t + 116_444_736_000_000_000) / 10).to_string(),
            detect: Some(|f| f.shape == Shape::WebKit),
            set: None,
        },
        Case {
            name: "excel",
            header: "ExcelDate",
            step: DAY,
            write: |t| (t / DAY + 25_569).to_string(),
            detect: Some(|f| f.shape == Shape::Excel),
            set: None,
        },
        Case {
            name: "cocoa",
            header: "ZCOCOA_TIME",
            step: 3_601 * s,
            write: |t| (t / TICKS_PER_SEC - 978_307_200).to_string(),
            detect: Some(|f| f.shape == Shape::Cocoa),
            set: None,
        },
        Case {
            name: "epoch-days",
            header: "epoch_days",
            step: DAY,
            write: |t| (t / DAY).to_string(),
            detect: Some(|f| f.shape == Shape::EpochDays),
            set: None,
        },
        Case {
            name: "cocoa-by-hand",
            header: "value",
            step: 3_601 * s,
            write: |t| (t / TICKS_PER_SEC - 978_307_200).to_string(),
            // Read as Unix seconds of 1993 without a header that says Cocoa.
            detect: Some(|f| f.shape == Shape::EpochS),
            set: Some(|| Fmt::shape(Shape::Cocoa)),
        },
        Case {
            name: "custom",
            header: "when",
            step: 3_601 * s,
            write: |t| {
                let c = civil(t);
                format!(
                    "{:02}h{:02}m{:02} le {:02}|{:02}|{:04}",
                    c.h, c.mi, c.s, c.d, c.mo, c.y
                )
            },
            detect: None,
            set: Some(|| {
                Fmt::shape(Shape::Custom(
                    Pattern::intern("%Hh%Mm%S le %d|%m|%Y").unwrap(),
                ))
            }),
        },
    ]
}

#[test]
fn every_format_parses_sorts_and_filters() {
    let dir = tempfile::tempdir().unwrap();
    for case in cases() {
        // Instants in a scrambled order (permutation of 0..N).
        let ticks: Vec<i64> = (0..N).map(|i| T0 + ((i * 17) % N) * case.step).collect();
        let mut csv = format!("{},n\n", case.header);
        for (i, &t) in ticks.iter().enumerate() {
            let v = (case.write)(t);
            if v.contains(',') {
                csv.push_str(&format!("\"{v}\",{i}\n"));
            } else {
                csv.push_str(&format!("{v},{i}\n"));
            }
        }
        let path = dir.path().join(format!("{}.csv", case.name));
        std::fs::write(&path, &csv).unwrap();
        let mut opts = OpenOptions::new(dir.path().join("cache"));
        opts.use_cache = false;
        let t = Table::open(&path, opts).unwrap();
        t.build().unwrap();
        let e = Engine::new(Arc::new(t));
        let detected = e.kind(0);
        match (case.detect, detected) {
            (Some(ok), Kind::Time(f)) => assert!(ok(f), "{}: detected {}", case.name, f.spec()),
            (Some(_), k) => panic!("{}: not detected as dates: {k:?}", case.name),
            (None, Kind::Time(f)) => panic!("{}: detected {} (should not be)", case.name, f.spec()),
            (None, _) => {}
        }
        if let Some(set) = case.set {
            e.set_kind(0, Some(Kind::Time(set())));
        }
        let Kind::Time(f) = e.kind(0) else {
            unreachable!()
        };
        // Parse, and display round trip.
        for &want in &ticks {
            let v = (case.write)(want);
            assert_eq!(
                time::parse(v.as_bytes(), f),
                Some(want),
                "{}: {v:?}",
                case.name
            );
            let shown = time::format(want, 0);
            assert_eq!(
                time::parse_any(shown.as_bytes()),
                Some(want),
                "{}",
                case.name
            );
        }
        assert_eq!(e.unparsable(0).unwrap(), (0, 0), "{}", case.name);
        // Sort: by instant.
        let sorted = fhv_engine::view::run(
            &e,
            &Query {
                filter: None,
                sort: vec![SortKey {
                    col: 0,
                    desc: false,
                }],
            },
        )
        .unwrap();
        let got: Vec<i64> = sorted.iter().map(|r| ticks[r as usize]).collect();
        let mut want = ticks.clone();
        want.sort_unstable();
        assert_eq!(got, want, "{}: sort", case.name);
        // Range filter, operands written as the grid shows them.
        let (lo, hi) = (want[10], want[29]);
        let between = Op::Between(time::format(lo, 0), time::format(hi, 0));
        let rows = e
            .eval(&Expr::Pred(Pred {
                target: Target::Column(0),
                op: between,
                case_sensitive: false,
            }))
            .unwrap();
        let mut expect: Vec<u32> = (0..N as u32)
            .filter(|&r| (lo..=hi).contains(&ticks[r as usize]))
            .collect();
        expect.sort_unstable();
        assert_eq!(
            rows.iter().collect::<Vec<_>>(),
            expect,
            "{}: between",
            case.name
        );
        assert_eq!(expect.len(), 20);
    }
}

/// Numbers that are not times stay numbers: ids, sizes, counters, hashes,
/// and values in an epoch range only by chance for some rows.
#[test]
fn numbers_are_not_dates() {
    let dir = tempfile::tempdir().unwrap();
    let mut csv = String::from("EventRecordID,Size,Pid,Hash32,Mixed,Zeroes,Year\n");
    let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
    for i in 0..2000u64 {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        let size = if i % 3 == 0 {
            1_500_000_000
        } else {
            x % 5_000_000_000
        };
        let mixed = if i % 2 == 0 { 1_709_296_496 + i } else { i };
        csv.push_str(&format!(
            "{},{size},{},{},{mixed},0,{}\n",
            100_000 + i,
            x % 65_536,
            x as u32,
            1990 + i % 40
        ));
    }
    let path = dir.path().join("n.csv");
    std::fs::write(&path, csv).unwrap();
    let mut opts = OpenOptions::new(dir.path().join("cache"));
    opts.use_cache = false;
    let t = Table::open(&path, opts).unwrap();
    t.build().unwrap();
    let ci = t.column_index().unwrap();
    for (c, name) in t.columns.iter().enumerate() {
        assert_eq!(ci.cols[c].kind, Kind::Number, "{name}");
        assert!(ci.cols[c].numeric, "{name}");
    }
}

/// csv-gen's `dates` layout: every date column is detected with its shape,
/// and the columns agree on each row's instant (to the precision written).
#[test]
fn csv_gen_dates_layout_is_detected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("dates.csv");
    let spec = csv_gen::Spec {
        layout: csv_gen::Layout::Dates,
        seed: 3,
        rows: Some(5000),
        bytes: None,
        delim: b',',
        crlf: false,
    };
    let mut f = std::fs::File::create(&path).unwrap();
    csv_gen::generate(&spec, &mut f).unwrap();
    let mut opts = OpenOptions::new(dir.path().join("cache"));
    opts.use_cache = false;
    let t = Arc::new(Table::open(&path, opts).unwrap());
    t.build().unwrap();
    let e = Engine::new(t.clone());
    let ci = t.column_index().unwrap();
    let ncols = t.columns.len();
    let fmts: Vec<Fmt> = (1..ncols - 1)
        .map(|c| match ci.cols[c].kind {
            Kind::Time(f) => f,
            k => panic!("{}: {k:?}", t.columns[c]),
        })
        .collect();
    let shape = |n: &str| fmts[t.columns.iter().position(|c| c == n).unwrap() - 1].shape;
    assert_eq!(shape("epoch_s"), Shape::EpochS);
    assert_eq!(shape("epoch_ms"), Shape::EpochMs);
    assert_eq!(shape("filetime"), Shape::FileTime);
    assert_eq!(shape("webkit"), Shape::WebKit);
    assert_eq!(shape("excel_date"), Shape::Excel);
    assert_eq!(ci.primary_time, Some(1));
    assert!(matches!(e.kind(ncols - 1), Kind::Text));
    for r in (0..5000u64).step_by(97) {
        let row = t.row_fields(r).unwrap();
        let t0 = time::parse(row[1].as_bytes(), fmts[0]).unwrap();
        for (k, f) in fmts.iter().enumerate() {
            let c = k + 1;
            let got = time::parse(row[c].as_bytes(), *f)
                .unwrap_or_else(|| panic!("{}: {:?}", t.columns[c], row[c]));
            // The syslog year is assumed (the file's modification year).
            if t.columns[c] == "syslog" {
                continue;
            }
            let tol = if t.columns[c] == "eu_date" { 60 } else { 1 } * TICKS_PER_SEC;
            assert!(
                (got - t0).abs() < tol,
                "{} row {r}: {:?}",
                t.columns[c],
                row[c]
            );
        }
    }
}
