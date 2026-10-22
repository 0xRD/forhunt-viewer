//! Index cache: hit on reopen, invalidated when the file changes, never
//! written next to the input.

use std::sync::atomic::Ordering;

use fhv_engine::{OpenOptions, Table};

fn open(path: &std::path::Path, cache: &std::path::Path) -> Table {
    let t = Table::open(path, OpenOptions::new(cache)).unwrap();
    t.build().unwrap();
    t
}

#[test]
fn reopen_hits_cache_and_changes_invalidate() {
    let dir = tempfile::tempdir().unwrap();
    let evidence = dir.path().join("evidence");
    let cache = dir.path().join("cache");
    std::fs::create_dir_all(&evidence).unwrap();
    let path = evidence.join("t.csv");
    let mut data = String::from("a,b\n");
    for i in 0..3000 {
        data.push_str(&format!("{i},\"x\ny\"\n"));
    }
    std::fs::write(&path, &data).unwrap();

    let t1 = open(&path, &cache);
    assert!(!t1.from_cache.load(Ordering::Relaxed));
    assert_eq!(t1.row_count(), 3000);

    let t2 = open(&path, &cache);
    assert!(t2.from_cache.load(Ordering::Relaxed));
    assert_eq!(t2.row_count(), 3000);
    assert_eq!(t2.row_fields(2999).unwrap(), ["2999", "x\ny"]);

    // Same size, different content: the sampled hash changes the key.
    let changed = data.replacen("0,", "9,", 1);
    std::fs::write(&path, &changed).unwrap();
    let t3 = open(&path, &cache);
    assert!(!t3.from_cache.load(Ordering::Relaxed));
    assert_eq!(t3.row_fields(0).unwrap(), ["9", "x\ny"]);

    // Appended rows.
    data.push_str("x,y\n");
    std::fs::write(&path, &data).unwrap();
    let t4 = open(&path, &cache);
    assert!(!t4.from_cache.load(Ordering::Relaxed));
    assert_eq!(t4.row_count(), 3001);

    // Nothing was written next to the evidence.
    let names: Vec<_> = std::fs::read_dir(&evidence)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names, ["t.csv"]);
}

#[test]
fn transcoded_files_are_cached_too() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("u16.csv");
    let mut b = vec![0xFF, 0xFE];
    for u in "a,b\n1,é\n2,ü\n".encode_utf16() {
        b.extend_from_slice(&u.to_le_bytes());
    }
    std::fs::write(&path, &b).unwrap();
    let cache = dir.path().join("cache");
    let t1 = open(&path, &cache);
    assert_eq!(t1.row_fields(1).unwrap(), ["2", "ü"]);
    let t2 = open(&path, &cache);
    assert!(t2.from_cache.load(Ordering::Relaxed));
    assert_eq!(t2.row_fields(0).unwrap(), ["1", "é"]);
}

#[test]
fn corrupt_cache_is_rebuilt() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.csv");
    std::fs::write(&path, "a\n1\n2\n").unwrap();
    let cache = dir.path().join("cache");
    open(&path, &cache);
    for e in std::fs::read_dir(cache.join("index")).unwrap() {
        let p = e.unwrap().path();
        let mut b = std::fs::read(&p).unwrap();
        let n = b.len();
        b[n / 2] ^= 0xFF;
        std::fs::write(&p, b).unwrap();
    }
    let t = open(&path, &cache);
    assert!(!t.from_cache.load(Ordering::Relaxed));
    assert_eq!(t.row_count(), 2);
}
