//! Regressions found by differential testing against an independent
//! oracle on real event logs (no evidence committed: small generated
//! fixtures reproduce each case).

use std::sync::Arc;

use fhv_engine::filter::{Engine, Expr, Op, Pred, Target};
use fhv_engine::{OpenOptions, Table};

struct Fx {
    _dir: tempfile::TempDir,
    engine: Engine,
}

fn open(csv: &str) -> Fx {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.csv");
    std::fs::write(&path, csv).unwrap();
    let mut opts = OpenOptions::new(dir.path().join("cache"));
    opts.use_cache = false;
    let t = Table::open(&path, opts).unwrap();
    t.build().unwrap();
    Fx {
        _dir: dir,
        engine: Engine::new(Arc::new(t)),
    }
}

fn rows(fx: &Fx, c: usize, op: Op) -> Vec<u32> {
    fx.engine
        .eval(&Expr::Pred(Pred {
            target: Target::Column(c),
            op,
            case_sensitive: false,
        }))
        .unwrap()
        .iter()
        .collect()
}

/// `<=` and the upper bound of `between` cover the whole unit the operand
/// is written to: `between 2025-10-21 and 2025-10-21` is that day, and
/// `<= 13:08:19` includes 13:08:19.5. They used to stop at the first tick
/// (midnight, the whole second), silently dropping every event inside it.
#[test]
fn coarse_time_upper_bounds_cover_their_unit() {
    // Column 0 is the primary time column (time index path), column 1 a
    // second time column (scan path); same values in both.
    let times = [
        "2025-10-20 23:59:59.9999999",
        "2025-10-21 00:00:00.0000000",
        "2025-10-21 13:08:19.0000000",
        "2025-10-21 13:08:19.5000000",
        "2025-10-21 13:08:59.9000000",
        "2025-10-21 23:59:59.9999999",
        "2025-10-22 00:00:00.0000000",
    ];
    let mut csv = String::from("TimeCreated,Other,n\n");
    // Out of time order, so the primary column gets a sort permutation.
    for (i, t) in times.iter().enumerate().rev() {
        csv.push_str(&format!("{t},{t},{i}\n"));
    }
    let fx = open(&csv);
    let ci = fx.engine.table().column_index().unwrap();
    assert_eq!(ci.primary_time, Some(0));
    assert!(ci.cols[1].time.is_none() && ci.cols[1].dict.is_some());
    // File rows are in reverse order: row r holds times[6 - r].
    let want = |idx: &[usize]| {
        let mut v: Vec<u32> = idx.iter().map(|&i| (6 - i) as u32).collect();
        v.sort_unstable();
        v
    };
    for c in [0, 1] {
        let between = Op::Between("2025-10-21".into(), "2025-10-21".into());
        assert_eq!(rows(&fx, c, between), want(&[1, 2, 3, 4, 5]), "col {c}");
        assert_eq!(
            rows(&fx, c, Op::Le("2025-10-21".into())),
            want(&[0, 1, 2, 3, 4, 5]),
            "col {c}"
        );
        assert_eq!(
            rows(&fx, c, Op::Le("2025-10-21 13:08:19".into())),
            want(&[0, 1, 2, 3]),
            "col {c}"
        );
        assert_eq!(
            rows(&fx, c, Op::Le("2025-10-21T13:08Z".into())),
            want(&[0, 1, 2, 3, 4]),
            "col {c}"
        );
        assert_eq!(
            rows(&fx, c, Op::Le("2025-10-21 13:08:19.0".into())),
            want(&[0, 1, 2]),
            "col {c}"
        );
        // Lower bounds and strict bounds are unchanged: from the first tick.
        assert_eq!(
            rows(&fx, c, Op::Ge("2025-10-21 13:08:19".into())),
            want(&[2, 3, 4, 5, 6]),
            "col {c}"
        );
        assert_eq!(
            rows(&fx, c, Op::Lt("2025-10-21".into())),
            want(&[0]),
            "col {c}"
        );
        assert_eq!(
            rows(&fx, c, Op::Gt("2025-10-21 13:08:19".into())),
            want(&[3, 4, 5, 6]),
            "col {c}"
        );
        // Full precision: the exact tick, as before.
        assert_eq!(
            rows(&fx, c, Op::Le("2025-10-21 13:08:19.0000000".into())),
            want(&[0, 1, 2]),
            "col {c}"
        );
    }
}

/// A time or number column with few distinct values is a dictionary column;
/// sorting it walks the dictionary order, which was text order: `1/10/2024`
/// before `1/2/2024`, and `10` before `9` as soon as one cell of a number
/// column was not a number. It now follows the column's kind, like the
/// other sort paths (unparsable cells first, as blanks).
#[test]
fn dictionary_columns_sort_by_their_kind() {
    let seen = [
        "1/2/2024",
        "1/10/2024 9:00 PM",
        "12/1/2023",
        "",
        "2/1/2024",
        "1/10/2024 10:00 AM",
    ];
    let nums = ["10", "9", "100", "-3", "0x10"];
    let mut cells = Vec::new();
    let mut csv = String::from("TimeCreated,Seen,Num\n");
    for i in 0..300 {
        // One cell in 300 is not a number: still a number column (99 %).
        let n = if i == 7 { "n/a" } else { nums[i % 5] };
        let s = seen[i % 6];
        csv.push_str(&format!(
            "2024-01-01 00:{:02}:{:02},{s},{n}\n",
            i / 60,
            i % 60
        ));
        cells.push((s, n));
    }
    let fx = open(&csv);
    let t = fx.engine.table();
    let ci = t.column_index().unwrap();
    assert!(matches!(
        ci.cols[1].kind,
        fhv_engine::columns::Kind::Time(_)
    ));
    assert!(ci.cols[1].dict.is_some() && ci.cols[1].time.is_none());
    assert_eq!(ci.cols[2].kind, fhv_engine::columns::Kind::Number);
    assert!(ci.cols[2].dict.is_some());
    let all: roaring::RoaringBitmap = (0..300).collect();
    let order = |col: usize, desc: bool, single: bool| -> Vec<&str> {
        let mut keys = vec![fhv_engine::SortKey { col, desc }];
        if !single {
            keys.push(fhv_engine::SortKey {
                col: 0,
                desc: false,
            });
        }
        let v = fhv_engine::sort::sort(t, &all, &keys).unwrap();
        // Distinct values in display order; ties in file order.
        assert!(v.windows(2).all(|w| {
            let (a, b) = (cells[w[0] as usize], cells[w[1] as usize]);
            let (a, b) = if col == 1 { (a.0, b.0) } else { (a.1, b.1) };
            a != b || w[0] < w[1]
        }));
        let mut out: Vec<&str> = v
            .iter()
            .map(|&r| {
                let c = cells[r as usize];
                if col == 1 { c.0 } else { c.1 }
            })
            .collect();
        out.dedup();
        out
    };
    let times = [
        "",
        "12/1/2023",
        "1/2/2024",
        "1/10/2024 10:00 AM",
        "1/10/2024 9:00 PM",
        "2/1/2024",
    ];
    let numbers = ["n/a", "-3", "9", "10", "0x10", "100"];
    for single in [true, false] {
        assert_eq!(order(1, false, single), times, "single {single}");
        let mut rev = times;
        rev.reverse();
        assert_eq!(order(1, true, single), rev, "single {single}");
        assert_eq!(order(2, false, single), numbers, "single {single}");
        let mut rev = numbers;
        rev.reverse();
        assert_eq!(order(2, true, single), rev, "single {single}");
    }
}

/// Equal times / numbers spelled differently (merged timelines: `...Z` and
/// `... ` forms, `0x10` and `16`) tie: they keep file order and the next
/// sort key decides, as on the non-dictionary paths. The dictionary rank
/// used to order them by spelling.
#[test]
fn dictionary_ties_across_spellings_keep_file_order() {
    let mut csv = String::from("TimeCreated,Seen,Num,k\n");
    for i in 0..100 {
        let seen = ["2024-01-02 10:00:00", "2024-01-02T10:00:00Z", "2024-01-01"][i % 3];
        let num = ["16", "0x10", "1"][i % 3];
        csv.push_str(&format!(
            "2024-01-01 00:{:02}:{:02},{seen},{num},{}\n",
            i / 60,
            i % 60,
            99 - i
        ));
    }
    let fx = open(&csv);
    let t = fx.engine.table();
    let ci = t.column_index().unwrap();
    assert!(ci.cols[1].dict.is_some() && ci.cols[2].dict.is_some());
    let all: roaring::RoaringBitmap = (0..100).collect();
    let key = |col, desc| fhv_engine::SortKey { col, desc };
    for col in [1, 2] {
        for desc in [false, true] {
            let small: Vec<u32> = (0..100).filter(|r| r % 3 == 2).collect();
            let big: Vec<u32> = (0..100).filter(|r| r % 3 != 2).collect();
            // Single key: equal values in file order.
            let want: Vec<u32> = if desc {
                [big.clone(), small.clone()].concat()
            } else {
                [small.clone(), big.clone()].concat()
            };
            let got = fhv_engine::sort::sort(t, &all, &[key(col, desc)]).unwrap();
            assert_eq!(got, want, "col {col} desc {desc}");
            // Second key k ascending = file order reversed within ties.
            let rev = |v: &Vec<u32>| v.iter().rev().copied().collect::<Vec<u32>>();
            let want: Vec<u32> = if desc {
                [rev(&big), rev(&small)].concat()
            } else {
                [rev(&small), rev(&big)].concat()
            };
            let got = fhv_engine::sort::sort(t, &all, &[key(col, desc), key(3, false)]).unwrap();
            assert_eq!(got, want, "col {col} desc {desc}, then k");
        }
    }
}

/// A pasted list of IOCs is a case-insensitive `in` list: 2,000 values took
/// 95 s on 57 k events (regex alternation) and 20,000 failed with "regex
/// exceeds size limit". Large lists must work, with the same Unicode simple
/// case folding as the other text operators (D-012).
#[test]
fn large_case_insensitive_in_lists() {
    let mut x = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    let hashes: Vec<String> = (0..4000)
        .map(|_| format!("{:016x}{:016x}", next(), next()))
        .collect();
    let mut csv = String::from("Hash,Other\n");
    for (i, h) in hashes.iter().enumerate() {
        // Mixed case in the file.
        let h = if i % 3 == 0 {
            h.to_uppercase()
        } else {
            h.clone()
        };
        csv.push_str(&format!("{h},{i}\n"));
    }
    let fx = open(&csv);
    assert!(
        fx.engine.table().column_index().unwrap().cols[0]
            .dict
            .is_none()
    );
    // 20,000 values: every 4th hash of the file (in the other case), plus
    // values that are not in the file.
    let mut list: Vec<String> = hashes.iter().step_by(4).map(|h| h.to_uppercase()).collect();
    list.extend((0..19_000).map(|_| format!("{:016x}{:016x}", next(), next())));
    let want: Vec<u32> = (0..4000).step_by(4).collect();
    assert_eq!(rows(&fx, 0, Op::In(list.clone())), want);
    let not: Vec<u32> = (0..4000).filter(|i| i % 4 != 0).collect();
    assert_eq!(rows(&fx, 0, Op::NotIn(list)), not);

    // Unicode simple case folding, as for equals: Kelvin sign / K / k,
    // long s / S / s, ß / ẞ, É / é; but not full folding (ß is not "ss").
    let fx = open("v\nk\nK\n\u{212A}\nſ\ns\nß\nẞ\nss\nÉ\né\nE\n");
    let list = vec!["K".to_string(), "S".into(), "ß".into(), "é".into()];
    assert_eq!(
        rows(&fx, 0, Op::In(list.clone())),
        [0, 1, 2, 3, 4, 5, 6, 8, 9]
    );
    for v in &list {
        let eq: Vec<u32> = rows(&fx, 0, Op::Equals(v.clone()));
        let one: Vec<u32> = rows(&fx, 0, Op::In(vec![v.clone()]));
        assert_eq!(eq, one, "{v}");
    }
}

/// On a time column, a range operand that is not a date-time this column
/// understands is an error, not a silent number / text comparison:
/// `>= 2025` matched nothing (cells are not numbers), and a day-first date
/// on a month-first column compared as text.
#[test]
fn time_ranges_reject_operands_that_are_not_times() {
    let mut csv = String::from("TimeCreated,Seen\n");
    for i in 0..40 {
        csv.push_str(&format!(
            "2025-10-{:02} 10:00:00,10/{:02}/2025 1:00 PM\n",
            i % 28 + 1,
            i % 28 + 1
        ));
    }
    let fx = open(&csv);
    let ci = fx.engine.table().column_index().unwrap();
    assert!(matches!(
        ci.cols[1].kind,
        fhv_engine::columns::Kind::Time(f) if f.order == fhv_engine::time::Order::Mdy
    ));
    let eval = |c: usize, op: Op| {
        fx.engine.eval(&Expr::Pred(Pred {
            target: Target::Column(c),
            op,
            case_sensitive: false,
        }))
    };
    for (c, op) in [
        (0, Op::Ge("2025".into())),
        (0, Op::Lt("yesterday".into())),
        (0, Op::Between("2025-10-01".into(), "10/32/2025".into())),
        (1, Op::Le("21/10/2025".into())),
        (1, Op::Gt("5".into())),
    ] {
        let e = eval(c, op.clone()).expect_err(&format!("{op:?} on column {c}"));
        assert!(e.to_string().contains("date"), "{e}");
    }
    // Valid operands, in either the ISO or the column's own format.
    assert_eq!(
        eval(1, Op::Le("10/02/2025 1:00 PM".into())).unwrap().len(),
        4
    );
    assert_eq!(eval(1, Op::Le("2025-10-02".into())).unwrap().len(), 4);
    // Blank operands are no bound for the UI, but still not text: error.
    assert!(eval(0, Op::Gt(String::new())).is_err());
    // Text operators on time columns are unchanged.
    assert_eq!(eval(0, Op::Contains("2025-10-0".into())).unwrap().len(), 18);
}

/// Hex values above 2^53 (Windows Keywords such as 0x8020000000000000)
/// compare exactly: 0x8000000000000001 > 0x8000000000000000. As f64 they
/// were equal, so `>` missed rows and `<=` kept extra ones; sort tied them.
#[test]
fn large_hex_numbers_compare_exactly() {
    let base: u64 = 0x8000_0000_0000_0000;
    // Dictionary column (few values) and scanned column (many values).
    let mut csv = String::from("Keywords,Big\n");
    for i in 0..1200u64 {
        csv.push_str(&format!(
            "0x{:x},0x{:x}\n",
            base + (i % 3),
            base + (1199 - i)
        ));
    }
    let fx = open(&csv);
    let ci = fx.engine.table().column_index().unwrap();
    assert!(ci.cols[0].dict.is_some() && ci.cols[1].dict.is_none());
    assert_eq!(ci.cols[1].kind, fhv_engine::columns::Kind::Number);
    let n = |c, op| rows(&fx, c, op).len();
    assert_eq!(n(0, Op::Gt("0x8000000000000000".into())), 800);
    assert_eq!(n(0, Op::Ge("0x8000000000000001".into())), 800);
    assert_eq!(n(0, Op::Le("0x8000000000000001".into())), 800);
    assert_eq!(n(0, Op::Lt("9223372036854775809".into())), 400);
    assert_eq!(
        n(
            0,
            Op::Between("0x8000000000000001".into(), "0x8000000000000001".into())
        ),
        400
    );
    assert_eq!(n(1, Op::Gt(format!("0x{:x}", base + 1000))), 199);
    assert_eq!(n(1, Op::Le(format!("{}", base + 9))), 10);
    // Sort: numeric order, exact.
    let all: roaring::RoaringBitmap = (0..1200).collect();
    let t = fx.engine.table();
    let by = |c| {
        fhv_engine::sort::sort(
            t,
            &all,
            &[fhv_engine::SortKey {
                col: c,
                desc: false,
            }],
        )
        .unwrap()
    };
    let k0: Vec<u32> = by(0).iter().map(|r| r % 3).collect();
    assert!(k0.windows(2).all(|w| w[0] <= w[1]), "dictionary sort");
    let k1 = by(1);
    assert_eq!(k1, (0..1200u32).rev().collect::<Vec<_>>(), "scanned sort");
    let two = fhv_engine::sort::sort(
        t,
        &all,
        &[
            fhv_engine::SortKey { col: 1, desc: true },
            fhv_engine::SortKey {
                col: 0,
                desc: false,
            },
        ],
    )
    .unwrap();
    assert_eq!(two, (0..1200u32).collect::<Vec<_>>(), "multi-key sort");
}

/// Day-first dates whose days are all <= 12 cannot be told from month-first
/// ones: the profile assumes month first. The analyst can set the order of
/// a column; filters (also on the primary time column, whose time index is
/// then not used), sorts and the unparsable-cell count follow it.
#[test]
fn date_order_override_and_unparsable_counts() {
    // d/M/yyyy, days 1..12 of March and April 2025, out of order, plus
    // two cells that are not dates and one blank.
    let mut csv = String::from("TimeCreated,n\n");
    let mut want = Vec::new();
    for i in 0..48u32 {
        let (d, m) = (12 - i % 12, 3 + (i / 12) % 2);
        csv.push_str(&format!("{d}/{m}/2025 10:00,{i}\n"));
        want.push((m, d, i));
    }
    csv.push_str("n/a,48\n,49\nyesterday,50\n");
    let fx = open(&csv);
    let e = &fx.engine;
    // Undecided order: year-first, or month first for slash dates.
    assert!(matches!(
        e.kind(0),
        fhv_engine::columns::Kind::Time(f) if f.order == fhv_engine::time::Order::Ymd
    ));
    assert_eq!(e.unparsable(0).unwrap(), (1, 2));
    let between = Op::Between("2025-03-01".into(), "2025-03-31".into());
    // Month first: "12/3/2025" is 3 December, "3/4/2025" is 4 March...
    let march: Vec<u32> = want.iter().filter(|w| w.0 == 3).map(|w| w.2).collect();
    assert_ne!(rows(&fx, 0, between.clone()), march);
    e.set_kind(
        0,
        Some(fhv_engine::columns::Kind::Time(
            fhv_engine::time::Order::Dmy.into(),
        )),
    );
    assert_eq!(rows(&fx, 0, between.clone()), march);
    assert_eq!(e.unparsable(0).unwrap(), (1, 2));
    // Sort follows the override: unparsable first, then chronological.
    let got = fhv_engine::view::run(
        e,
        &fhv_engine::Query {
            filter: None,
            sort: vec![fhv_engine::SortKey {
                col: 0,
                desc: false,
            }],
        },
    )
    .unwrap();
    let mut sorted = want.clone();
    sorted.sort();
    let mut expect: Vec<u32> = vec![48, 49, 50];
    expect.extend(sorted.iter().map(|w| w.2));
    assert_eq!(got.iter().collect::<Vec<_>>(), expect);
    // Back to the profiled order.
    e.set_kind(0, None);
    assert_ne!(rows(&fx, 0, between), march);
}
