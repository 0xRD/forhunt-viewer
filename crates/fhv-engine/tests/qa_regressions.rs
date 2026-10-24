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
