//! Grouping (`fhv_engine::group`) against a naive implementation: the
//! groups, their counts at every level, their order, and the rows of each
//! group in the view's order, through the dictionary and the scan paths.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use fhv_engine::columns::text_cmp;
use fhv_engine::filter::{Engine, Expr};
use fhv_engine::group::{GroupSort, Groups, Level, group, group_with_cap};
use fhv_engine::sort::SortKey;
use fhv_engine::view::{Query, run};
use fhv_engine::{OpenOptions, Table};
use proptest::prelude::*;
use roaring::RoaringBitmap;

const CATS: [&str; 7] = ["alpha", "Alpha", "BETA", "", " ", "gamma, ray", "é"];

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
                CATS[(next() % 7) as usize].to_string(),
                match next() % 10 {
                    0 => String::new(),
                    1 => "  ".into(),
                    2 => "q\"x".into(),
                    3 => "a,b".into(),
                    _ => format!("w{}", next() % 1500),
                },
                (next() % 12).to_string(),
            ];
            if i % 89 == 7 {
                r.truncate(2);
            }
            r
        })
        .collect()
}

fn csv(rows: &[Vec<String>]) -> String {
    let mut s = String::from("cat,word,num\n");
    for r in rows {
        let cells: Vec<String> = r
            .iter()
            .map(|c| {
                if c.contains([',', '"']) {
                    format!("\"{}\"", c.replace('"', "\"\""))
                } else {
                    c.clone()
                }
            })
            .collect();
        s.push_str(&cells.join(","));
        s.push('\n');
    }
    s
}

fn key(r: &[String], c: usize) -> Option<String> {
    let v = r.get(c).map(String::as_str).unwrap_or("");
    (!v.bytes().all(|b| b.is_ascii_whitespace())).then(|| v.to_string())
}

fn path_values(g: &Groups, n: usize) -> Vec<Option<String>> {
    g.path(n)
        .into_iter()
        .map(|k| g.value(k).map(str::to_string))
        .collect()
}

fn check(
    engine: &Engine,
    rows: &[Vec<String>],
    keep: &RoaringBitmap,
    sort: Option<SortKey>,
    levels: &[Level],
) {
    let q = Query {
        filter: Some(Expr::Rows(Arc::new(keep.clone()))),
        sort: sort.into_iter().collect(),
    };
    let base = run(engine, &q).unwrap();
    let (v, g) = group(engine, &base, levels, &AtomicU64::new(0)).unwrap();
    assert_eq!(v.rows, base.rows);
    // Naive counts of every prefix of every row's path.
    let mut want: BTreeMap<Vec<Option<String>>, u64> = BTreeMap::new();
    for r in keep {
        let p: Vec<Option<String>> = levels
            .iter()
            .map(|l| key(&rows[r as usize], l.col))
            .collect();
        for d in 1..=p.len() {
            *want.entry(p[..d].to_vec()).or_default() += 1;
        }
    }
    let got: BTreeMap<Vec<Option<String>>, u64> = (0..g.nodes.len())
        .map(|n| (path_values(&g, n), g.nodes[n].count))
        .collect();
    assert_eq!(got.len(), g.nodes.len(), "a group appears twice");
    assert_eq!(got, want);
    // Rows of each leaf: contiguous, with that path, in the view's order.
    let order: Vec<u32> = v.iter().collect();
    let base_pos: BTreeMap<u32, usize> = base.iter().enumerate().map(|(i, r)| (r, i)).collect();
    let mut covered = 0u64;
    for n in 0..g.nodes.len() {
        let nd = g.nodes[n];
        let p = path_values(&g, n);
        let span = &order[nd.start as usize..(nd.start + nd.count) as usize];
        for &r in span {
            let rp: Vec<Option<String>> = levels[..p.len()]
                .iter()
                .map(|l| key(&rows[r as usize], l.col))
                .collect();
            assert_eq!(rp, p);
        }
        if g.is_leaf(n) {
            covered += nd.count;
            assert!(span.windows(2).all(|w| base_pos[&w[0]] < base_pos[&w[1]]));
        }
        // Children: preorder layout, and the level's order.
        let kids: Vec<usize> = {
            let mut k = Vec::new();
            let mut c = n + 1;
            while c < nd.end as usize {
                k.push(c);
                c = g.nodes[c].end as usize;
            }
            k
        };
        if !g.is_leaf(n) {
            assert_eq!(
                kids.iter().map(|&k| g.nodes[k].count).sum::<u64>(),
                nd.count
            );
            sorted(&g, &kids, levels[nd.depth as usize + 1]);
        }
    }
    assert_eq!(covered, keep.len());
    let roots: Vec<usize> = {
        let mut k = Vec::new();
        let mut c = 0;
        while c < g.nodes.len() {
            k.push(c);
            c = g.nodes[c].end as usize;
        }
        k
    };
    if !roots.is_empty() {
        sorted(&g, &roots, levels[0]);
    }
}

/// Siblings follow the level's sort (ties by value).
fn sorted(g: &Groups, sib: &[usize], lv: Level) {
    let val_cmp = |a: usize, b: usize| {
        let (x, y) = (g.value(a), g.value(b));
        match (x, y) {
            (None, None) => std::cmp::Ordering::Equal,
            (None, _) => std::cmp::Ordering::Less,
            (_, None) => std::cmp::Ordering::Greater,
            (Some(x), Some(y)) if lv.col == 2 => x
                .parse::<f64>()
                .unwrap()
                .total_cmp(&y.parse::<f64>().unwrap()),
            (Some(x), Some(y)) => text_cmp(x.as_bytes(), y.as_bytes()),
        }
    };
    for w in sib.windows(2) {
        let (a, b) = (w[0], w[1]);
        let (ca, cb) = (g.nodes[a].count, g.nodes[b].count);
        let ok = match lv.sort {
            GroupSort::ValueAsc => val_cmp(a, b).is_lt(),
            GroupSort::ValueDesc => val_cmp(a, b).is_gt(),
            GroupSort::CountDesc => ca > cb || ca == cb && val_cmp(a, b).is_lt(),
            GroupSort::CountAsc => ca < cb || ca == cb && val_cmp(a, b).is_lt(),
        };
        assert!(ok, "{:?} then {:?} under {lv:?}", g.value(a), g.value(b));
    }
}

fn open(rows: &[Vec<String>]) -> (tempfile::TempDir, Engine) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.csv");
    std::fs::write(&path, csv(rows)).unwrap();
    let mut opts = OpenOptions::new(dir.path().join("cache"));
    opts.use_cache = false;
    let t = Table::open(&path, opts).unwrap();
    t.build().unwrap();
    let ci = t.column_index().unwrap();
    assert!(ci.cols[0].dict.is_some(), "cat is a dictionary column");
    assert!(ci.cols[1].dict.is_none(), "word is scanned");
    (dir, Engine::new(Arc::new(t)))
}

fn sort_of(k: u8) -> GroupSort {
    [
        GroupSort::ValueAsc,
        GroupSort::ValueDesc,
        GroupSort::CountDesc,
        GroupSort::CountAsc,
    ][k as usize % 4]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(24))]
    #[test]
    fn groups_match_naive(
        seed in 1u64..1_000_000,
        keep_mod in 1u32..5,
        cols in proptest::collection::vec((0usize..3, 0u8..4), 1..4),
        sort in proptest::option::of((0usize..3, any::<bool>())),
    ) {
        let rows = make_rows(seed, 2500);
        let (_d, e) = open(&rows);
        let keep: RoaringBitmap = (0..rows.len() as u32).filter(|r| r % keep_mod != 1 || keep_mod == 1).collect();
        let levels: Vec<Level> = cols.iter().map(|&(col, s)| Level { col, sort: sort_of(s) }).collect();
        let sort = sort.map(|(col, desc)| SortKey { col, desc });
        check(&e, &rows, &keep, sort, &levels);
    }
}

#[test]
fn empty_view_and_cap() {
    let rows = make_rows(7, 2500);
    let (_d, e) = open(&rows);
    let lv = [Level {
        col: 0,
        sort: GroupSort::CountDesc,
    }];
    check(&e, &rows, &RoaringBitmap::new(), None, &lv);
    let q = Query::default();
    let base = run(&e, &q).unwrap();
    assert!(
        group(
            &e,
            &base,
            &[Level {
                col: 9,
                sort: GroupSort::ValueAsc
            }],
            &AtomicU64::new(0)
        )
        .is_err()
    );
    // Past the cap: an error, through the scan and the dictionary paths.
    for col in [1, 0] {
        let lv = [Level {
            col,
            sort: GroupSort::ValueAsc,
        }];
        let e2 = group_with_cap(&e, &base, &lv, &AtomicU64::new(0), 3).unwrap_err();
        assert!(e2.to_string().contains("more than 3 groups"), "{e2}");
    }
}
