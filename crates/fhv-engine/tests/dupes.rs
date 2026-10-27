//! Duplicate / unique value counts (D-066) against a naive count over the
//! decoded rows, on a dictionary column and a scanned (high-cardinality)
//! column, over the whole file or a row subset, case-sensitive or not.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64};

use fhv_engine::dupes;
use fhv_engine::{OpenOptions, Table};
use proptest::prelude::*;
use roaring::RoaringBitmap;

const WORDS: &[&str] = &[
    "alpha", "Alpha", "ALPHA", "beta", "", "  ", "é", "É", "x,y", "q\"t", "gamma",
];

fn csv_field(s: &str) -> String {
    if s.contains([',', '"']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

fn naive(t: &Table, col: usize, within: Option<&RoaringBitmap>, cs: bool) -> HashMap<String, u32> {
    let mut m = HashMap::new();
    for r in 0..t.row_count() {
        if within.is_some_and(|w| !w.contains(r as u32)) {
            continue;
        }
        let f = t.row_fields(r).unwrap();
        let v = f.get(col).cloned().unwrap_or_default();
        if v.trim().is_empty() {
            continue;
        }
        let k = if cs { v } else { v.to_lowercase() };
        *m.entry(k).or_default() += 1;
    }
    m
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(24))]
    #[test]
    fn counts_match_naive(
        picks in prop::collection::vec((0..WORDS.len(), 0u32..700, any::<bool>()), 1..1500),
        mask in prop::collection::vec(any::<bool>(), 1500),
        scoped in any::<bool>(),
        cs in any::<bool>(),
    ) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.csv");
        let mut s = String::from("Small,Wide\n");
        for (w, n, short) in &picks {
            if *short {
                // Ragged row: the missing field is blank.
                s.push_str(&format!("{}\n", csv_field(WORDS[*w])));
            } else {
                let v = if n % 3 == 0 { "V" } else { "v" };
                s.push_str(&format!("{},{v}{}\n", csv_field(WORDS[*w]), n / 2));
            }
        }
        std::fs::write(&path, s).unwrap();
        let mut opts = OpenOptions::new(dir.path().join("cache"));
        opts.use_cache = false;
        let t = Table::open(&path, opts).unwrap();
        t.build().unwrap();
        let within: Option<RoaringBitmap> = scoped.then(|| {
            (0..t.row_count() as u32).filter(|&r| mask[r as usize]).collect()
        });
        let wide_dict = t.column_index().unwrap().cols[1].dict.is_some();
        prop_assume!(picks.len() < 1000 || !wide_dict);
        for col in 0..2 {
            let c = dupes::count(
                &t, col, within.as_ref(), cs, &AtomicBool::new(false), &AtomicU64::new(0),
            ).unwrap();
            let want = naive(&t, col, within.as_ref(), cs);
            prop_assert_eq!(c.distinct(), want.len());
            prop_assert!(!c.capped);
            for r in 0..t.row_count() {
                let f = t.row_fields(r).unwrap();
                let v = f.get(col).cloned().unwrap_or_default();
                let k = if cs { v.clone() } else { v.to_lowercase() };
                let expect = if v.trim().is_empty() { None } else { want.get(&k).copied() };
                prop_assert_eq!(c.count(&v), expect, "col {} value {:?}", col, v);
            }
        }
    }
}

#[test]
fn cancel_stops_a_scan() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.csv");
    let mut s = String::from("A\n");
    for i in 0..100_000 {
        s.push_str(&format!("v{i}\n"));
    }
    std::fs::write(&path, s).unwrap();
    let mut opts = OpenOptions::new(dir.path().join("cache"));
    opts.use_cache = false;
    let t = Table::open(&path, opts).unwrap();
    t.build().unwrap();
    let r = dupes::count(
        &t,
        0,
        None,
        true,
        &AtomicBool::new(true),
        &AtomicU64::new(0),
    );
    assert!(matches!(r, Err(fhv_engine::Error::Cancelled)));
    let done = AtomicU64::new(0);
    let c = dupes::count(&t, 0, None, true, &AtomicBool::new(false), &done).unwrap();
    assert_eq!(c.unique_and_duplicate(), (100_000, 0));
    assert_eq!(done.into_inner(), 100_000);
    assert_eq!(c.count("v5"), Some(1));
    assert_eq!(c.count(""), None);
}
