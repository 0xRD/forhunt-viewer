//! Filters and sorts must equal a naive row-by-row evaluation, whatever
//! path the engine takes (dictionary, time index, block scan with regex
//! prefilter, negation by complement).

mod common;

use std::cmp::Ordering;
use std::sync::Arc;

use fhv_engine::columns::Kind;
use fhv_engine::filter::{Engine, Expr, Op, Pred, Target};
use fhv_engine::sort::{SortKey, sort};
use fhv_engine::{OpenOptions, Table};
use proptest::prelude::*;

const COLS: [&str; 6] = ["cat", "ident", "num", "when", "text", "amount"];
const CATS: [&str; 6] = ["alpha", "Alpha", "BETA", "gamma ray", "", "émile"];
const WORDS: [&str; 10] = [
    "a",
    "B",
    "ab",
    "Ab",
    "x,y",
    "q\"t",
    "line\nbreak",
    "É",
    "é",
    " ",
];

/// Deterministic table: column kinds are known by construction.
fn make_rows(seed: u64, n: usize) -> Vec<Vec<String>> {
    let mut x = seed | 1;
    let mut next = move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    (0..n)
        .map(|i| {
            let mut r = vec![
                CATS[(next() % 6) as usize].to_string(),
                format!("id-{}-{}", next() % 5000, WORDS[(next() % 4) as usize]),
                (next() % 40).to_string(),
                if next() % 25 == 0 {
                    String::new()
                } else {
                    let s = next() % 86_400;
                    format!(
                        "2024-03-{:02} {:02}:{:02}:{:02}",
                        1 + next() % 3,
                        s / 3600,
                        s / 60 % 60,
                        s % 60
                    )
                },
                {
                    let mut t = (0..next() % 4)
                        .map(|_| WORDS[(next() % 10) as usize])
                        .collect::<Vec<_>>()
                        .join(" ");
                    if next() % 2 == 0 {
                        t.push_str(&format!(" {}", next() % 1000));
                    }
                    t
                },
                format!("{}.{:02}", next() % 100_000, next() % 100),
            ];
            // Some ragged rows.
            if i % 97 == 5 {
                r.truncate(3);
            }
            r
        })
        .collect()
}

fn csv(rows: &[Vec<String>]) -> Vec<u8> {
    let mut s = COLS.join(",");
    s.push('\n');
    for (i, r) in rows.iter().enumerate() {
        let cells: Vec<String> = r
            .iter()
            .map(|c| {
                // Some cells written as `"ab"c` (text after the closing
                // quote): decodes to `abc` (D-010), raw bytes differ.
                if i % 7 == 3 && c.len() >= 3 && c.is_ascii() && !c.contains([',', '"', '\n']) {
                    return format!("\"{}\"{}", &c[..2], &c[2..]);
                }
                if c.contains([',', '"', '\n']) {
                    format!("\"{}\"", c.replace('"', "\"\""))
                } else {
                    c.clone()
                }
            })
            .collect();
        s.push_str(&cells.join(","));
        s.push('\n');
    }
    s.into_bytes()
}

struct Fixture {
    _dir: tempfile::TempDir,
    rows: Vec<Vec<String>>,
    engine: Engine,
}

fn fixture(seed: u64) -> Fixture {
    let rows = make_rows(seed, 1500);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.csv");
    std::fs::write(&path, csv(&rows)).unwrap();
    let mut opts = OpenOptions::new(dir.path().join("cache"));
    opts.use_cache = false;
    let t = Table::open(&path, opts).unwrap();
    t.build().unwrap();
    let ci = t.column_index().unwrap();
    // The evaluation paths this test is meant to cover.
    assert!(ci.cols[0].dict.is_some(), "cat is a dictionary column");
    assert!(ci.cols[1].dict.is_none(), "ident is scanned");
    assert_eq!(ci.cols[2].kind, Kind::Number);
    assert!(ci.cols[3].time.is_some(), "when has a time index");
    assert!(ci.cols[4].dict.is_none(), "text is scanned");
    assert!(ci.cols[5].dict.is_none() && ci.cols[5].kind == Kind::Number);
    Fixture {
        _dir: dir,
        rows,
        engine: Engine::new(Arc::new(t)),
    }
}

// ---- naive reference -------------------------------------------------

fn cell(r: &[String], c: usize) -> &str {
    r.get(c).map(String::as_str).unwrap_or("")
}

fn low(s: &str) -> String {
    s.to_lowercase()
}

fn parse_num(s: &str) -> Option<f64> {
    s.trim().parse::<f64>().ok().filter(|v| v.is_finite())
}

fn parse_time(s: &str) -> Option<i64> {
    fhv_engine::time::parse_any(s.as_bytes())
}

/// Typed range test: time on the time column when the operand parses,
/// numbers when both parse, else case-insensitive text.
fn range(
    c: usize,
    v: &str,
    lo: Option<&str>,
    hi: Option<&str>,
    lo_incl: bool,
    hi_incl: bool,
    cs: bool,
) -> bool {
    let low = |s: &str| if cs { s.to_string() } else { low(s) };
    let cmp_ok =
        |o: Ordering, incl: bool, want: Ordering| o == want || (incl && o == Ordering::Equal);
    let bound = |b: &str| -> Option<Result<i64, f64>> {
        if c == 3
            && let Some(t) = parse_time(b)
        {
            return Some(Ok(t));
        }
        parse_num(b).map(Err)
    };
    // Typed only when every given bound parses the same way.
    let bl = lo.map(bound);
    let bh = hi.map(bound);
    let all = [bl, bh];
    let given: Vec<&Option<Result<i64, f64>>> = all.iter().flatten().collect();
    let typed = if given.iter().all(|b| matches!(b, Some(Ok(_)))) {
        given.first().and_then(|b| **b)
    } else if given.iter().all(|b| matches!(b, Some(Err(_)))) {
        given.first().and_then(|b| **b)
    } else {
        None
    };
    match typed {
        Some(Ok(_)) => {
            let Some(t) = parse_time(v) else { return false };
            let ok_lo = lo.is_none_or(|l| matches!(bound(l), Some(Ok(l)) if cmp_ok(t.cmp(&l), lo_incl, Ordering::Greater)));
            let ok_hi = hi.is_none_or(
                |h| matches!(bound(h), Some(Ok(h)) if cmp_ok(t.cmp(&h), hi_incl, Ordering::Less)),
            );
            ok_lo && ok_hi
        }
        Some(Err(_)) => {
            let Some(x) = parse_num(v) else { return false };
            let ok_lo = lo.is_none_or(|l| matches!(bound(l), Some(Err(l)) if cmp_ok(x.total_cmp(&l), lo_incl, Ordering::Greater)));
            let ok_hi = hi.is_none_or(|h| matches!(bound(h), Some(Err(h)) if cmp_ok(x.total_cmp(&h), hi_incl, Ordering::Less)));
            ok_lo && ok_hi
        }
        None => {
            let v = low(v);
            let ok_lo = lo.is_none_or(|l| {
                cmp_ok(v.as_str().cmp(low(l).as_str()), lo_incl, Ordering::Greater)
            });
            let ok_hi =
                hi.is_none_or(|h| cmp_ok(v.as_str().cmp(low(h).as_str()), hi_incl, Ordering::Less));
            ok_lo && ok_hi
        }
    }
}

fn naive_pred(p: &Pred, r: &[String]) -> bool {
    let test = |c: usize| {
        let v = cell(r, c);
        let (lv, cs) = (low(v), p.case_sensitive);
        let norm = |s: &str| if cs { s.to_string() } else { low(s) };
        let hv = if cs { v.to_string() } else { lv };
        match &p.op {
            Op::Equals(x) => hv == norm(x),
            Op::NotEquals(x) => hv != norm(x),
            Op::Contains(x) => hv.contains(&norm(x)),
            Op::NotContains(x) => !hv.contains(&norm(x)),
            Op::BeginsWith(x) => hv.starts_with(&norm(x)),
            Op::EndsWith(x) => hv.ends_with(&norm(x)),
            Op::IsBlank => v.trim_matches(|c: char| c.is_ascii_whitespace()).is_empty(),
            Op::IsNotBlank => !v.trim_matches(|c: char| c.is_ascii_whitespace()).is_empty(),
            Op::In(xs) => xs.iter().any(|x| hv == norm(x)),
            Op::NotIn(xs) => !xs.iter().any(|x| hv == norm(x)),
            Op::Regex(re) => regex::RegexBuilder::new(re)
                .case_insensitive(!cs)
                .build()
                .unwrap()
                .is_match(v),
            Op::Gt(x) => range(c, v, Some(x), None, false, true, cs),
            Op::Ge(x) => range(c, v, Some(x), None, true, true, cs),
            Op::Lt(x) => range(c, v, None, Some(x), true, false, cs),
            Op::Le(x) => range(c, v, None, Some(x), true, true, cs),
            Op::Between(a, b) => range(c, v, Some(a), Some(b), true, true, cs),
        }
    };
    match p.target {
        Target::Column(c) => test(c),
        Target::Any => {
            let pos = |op: &Op| -> Option<Op> {
                Some(match op {
                    Op::NotEquals(v) => Op::Equals(v.clone()),
                    Op::NotContains(v) => Op::Contains(v.clone()),
                    Op::NotIn(v) => Op::In(v.clone()),
                    Op::IsNotBlank => Op::IsBlank,
                    _ => return None,
                })
            };
            match pos(&p.op) {
                // Global negatives: no column matches the positive form.
                Some(op) => !(0..COLS.len()).any(|c| {
                    naive_pred(
                        &Pred {
                            target: Target::Column(c),
                            op: op.clone(),
                            case_sensitive: p.case_sensitive,
                        },
                        r,
                    )
                }),
                None => (0..COLS.len()).any(test),
            }
        }
    }
}

fn naive(e: &Expr, r: &[String]) -> bool {
    match e {
        Expr::And(xs) => xs.iter().all(|x| naive(x, r)),
        Expr::Or(xs) => xs.iter().any(|x| naive(x, r)),
        Expr::Not(x) => !naive(x, r),
        Expr::Pred(p) => naive_pred(p, r),
        Expr::Rows(_) => unreachable!(),
    }
}

// ---- strategies --------------------------------------------------------

fn operand() -> impl Strategy<Value = String> {
    prop_oneof![
        prop::sample::select(CATS.to_vec()).prop_map(String::from),
        prop::sample::select(WORDS.to_vec()).prop_map(String::from),
        (0u32..45).prop_map(|n| n.to_string()),
        (0u32..100_000).prop_map(|n| format!("{n}.5")),
        (1u32..4, 0u32..24).prop_map(|(d, h)| format!("2024-03-{d:02} {h:02}:00:00")),
        Just("id-1".to_string()),
        Just("ALP".to_string()),
        Just("".to_string()),
    ]
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        operand().prop_map(Op::Equals),
        operand().prop_map(Op::NotEquals),
        operand().prop_map(Op::Contains),
        operand().prop_map(Op::NotContains),
        operand().prop_map(Op::BeginsWith),
        operand().prop_map(Op::EndsWith),
        Just(Op::IsBlank),
        Just(Op::IsNotBlank),
        prop::collection::vec(operand(), 0..3).prop_map(Op::In),
        prop::collection::vec(operand(), 0..3).prop_map(Op::NotIn),
        prop::sample::select(vec![
            "^a",
            "b$",
            "[0-9]{2}",
            "^$",
            "id-\\d+-a",
            "id-1[0-9]+",
            "(?i)ALP",
            "^(alpha|beta)$",
            "q\"t",
            "a.c",
            "x,y",
            "line\\nbreak",
            "(É|é)",
            "gamma ray",
            "[A-Z]b",
            "ab|ray",
            "a?b",
            "(?i)id-4\\d",
            "\\.5$",
            "4\\d\\.",
            "bc",
        ])
        .prop_map(|s| Op::Regex(s.into())),
        operand().prop_map(Op::Gt),
        operand().prop_map(Op::Ge),
        operand().prop_map(Op::Lt),
        operand().prop_map(Op::Le),
        (operand(), operand()).prop_map(|(a, b)| Op::Between(a, b)),
    ]
}

fn pred() -> impl Strategy<Value = Pred> {
    (
        prop_oneof![9 => (0usize..COLS.len()).prop_map(Target::Column), 1 => Just(Target::Any)],
        op(),
        prop::bool::weighted(0.2),
    )
        .prop_map(|(target, op, case_sensitive)| Pred {
            target,
            op,
            case_sensitive,
        })
}

fn expr() -> impl Strategy<Value = Expr> {
    pred()
        .prop_map(Expr::Pred)
        .prop_recursive(3, 12, 3, |inner| {
            prop_oneof![
                prop::collection::vec(inner.clone(), 0..3).prop_map(Expr::And),
                prop::collection::vec(inner.clone(), 0..3).prop_map(Expr::Or),
                inner.prop_map(|e| Expr::Not(Box::new(e))),
            ]
        })
}

thread_local! {
    static FIX: Fixture = fixture(0x5EED);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(600))]

    #[test]
    fn filters_equal_naive(e in expr()) {
        FIX.with(|f| {
            let got: Vec<u32> = f.engine.eval(&e).unwrap().iter().collect();
            let want: Vec<u32> = f.rows.iter().enumerate().filter(|(_, r)| naive(&e, r)).map(|(i, _)| i as u32).collect();
            prop_assert_eq!(got, want, "{:?}", e);
            Ok(())
        })?;
    }

    #[test]
    fn sorts_equal_naive(keys in prop::collection::vec((0usize..COLS.len(), any::<bool>()), 1..4), filt in prop::option::of(pred())) {
        FIX.with(|f| {
            let keys: Vec<SortKey> = keys.into_iter().map(|(col, desc)| SortKey { col, desc }).collect();
            let rows = match &filt {
                Some(p) => f.engine.eval(&Expr::Pred(p.clone())).unwrap(),
                None => (0..f.rows.len() as u32).collect(),
            };
            let got = sort(f.engine.table(), &rows, &keys).unwrap();
            let mut want: Vec<u32> = rows.iter().collect();
            want.sort_by(|&a, &b| {
                for k in &keys {
                    let (x, y) = (cell(&f.rows[a as usize], k.col), cell(&f.rows[b as usize], k.col));
                    let o = match k.col {
                        2 => parse_num(x).unwrap_or(f64::NEG_INFINITY).total_cmp(&parse_num(y).unwrap_or(f64::NEG_INFINITY))
                            .then_with(|| fhv_engine::columns::text_cmp(x.as_bytes(), y.as_bytes())),
                        5 => parse_num(x).unwrap_or(f64::NEG_INFINITY).total_cmp(&parse_num(y).unwrap_or(f64::NEG_INFINITY)),
                        3 => parse_time(x).unwrap_or(i64::MIN).cmp(&parse_time(y).unwrap_or(i64::MIN)),
                        _ => fhv_engine::columns::text_cmp(x.as_bytes(), y.as_bytes()),
                    };
                    let o = if k.desc { o.reverse() } else { o };
                    if o != Ordering::Equal {
                        return o;
                    }
                }
                a.cmp(&b)
            });
            prop_assert_eq!(got, want, "{:?}", keys);
            Ok(())
        })?;
    }
}

/// Long values sharing long prefixes (window refinement over many levels),
/// mixed case, quotes with escapes, blanks; ascending and descending.
#[test]
fn long_shared_prefix_text_sort() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.csv");
    let mut x = 7u64;
    let mut vals = Vec::new();
    let mut s = String::from("v,w\n");
    for i in 0..3000 {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        let v = match x % 7 {
            0 => String::new(),
            1 => format!("Shared/Prefix/That/Is/Quite/Long/{:03}", x % 50),
            2 => format!("shared/prefix/that/is/quite/long/{:03}/\"q\"", x % 50),
            3 => format!(
                "SHARED/PREFIX/THAT/IS/QUITE/LONG/{:03}/tail/{}",
                x % 50,
                x % 3
            ),
            _ => format!(
                "shared/prefix/that/is/quite/long/{:03}{}",
                x % 50,
                "z".repeat((x % 40) as usize)
            ),
        };
        s.push_str(&format!("\"{}\",{i}\n", v.replace('"', "\"\"")));
        vals.push(v);
    }
    std::fs::write(&path, s).unwrap();
    let mut opts = OpenOptions::new(dir.path().join("cache"));
    opts.use_cache = false;
    let t = Table::open(&path, opts).unwrap();
    t.build().unwrap();
    let rows: roaring::RoaringBitmap = (0..vals.len() as u32).collect();
    for desc in [false, true] {
        let got = sort(
            &t,
            &rows,
            &[
                SortKey { col: 0, desc },
                SortKey {
                    col: 1,
                    desc: false,
                },
            ],
        )
        .unwrap();
        let mut want: Vec<u32> = (0..vals.len() as u32).collect();
        want.sort_by(|&a, &b| {
            let o = fhv_engine::columns::text_cmp(
                vals[a as usize].as_bytes(),
                vals[b as usize].as_bytes(),
            );
            (if desc { o.reverse() } else { o }).then(a.cmp(&b))
        });
        assert_eq!(got, want, "desc={desc}");
        let single = sort(&t, &rows, &[SortKey { col: 0, desc }]).unwrap();
        assert_eq!(single, want, "single key desc={desc}");
    }
}
