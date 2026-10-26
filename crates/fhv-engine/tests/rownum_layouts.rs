//! Row-number column detection on the csv-gen layouts and edge shapes
//! (D-042): detected only where the file really has a counter.

use fhv_engine::{OpenOptions, Table, rownum};

fn open(data: &[u8]) -> (tempfile::TempDir, Table) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.csv");
    std::fs::write(&path, data).unwrap();
    let mut opts = OpenOptions::new(dir.path().join("cache"));
    opts.use_cache = false;
    let t = Table::open(&path, opts).unwrap();
    t.build().unwrap();
    (dir, t)
}

fn generated(layout: csv_gen::Layout, rows: u64) -> Vec<u8> {
    let spec = csv_gen::Spec {
        layout,
        seed: 3,
        rows: Some(rows),
        bytes: None,
        delim: b',',
        crlf: false,
    };
    let mut data = Vec::new();
    csv_gen::generate(&spec, &mut data).unwrap();
    data
}

#[test]
fn generated_layouts() {
    use csv_gen::Layout::*;
    // Forhunt's RecordNumber / EventRecordId start at a per-file offset and
    // restart per source file; MFT EntryNumber starts at 16; Hayabusa and
    // plaso have no counter. The generic table's `id` is 1..N.
    for (layout, rows, want) in [
        (Forhunt, 3000, None),
        (Forhunt, 300_000, None),
        (Mft, 3000, None),
        (Hayabusa, 3000, None),
        (Plaso, 3000, None),
        (Generic, 3000, Some("id")),
        (Generic, 5, Some("id")),
    ] {
        let (_d, t) = open(&generated(layout, rows));
        let got = rownum::detect_table(&t).map(|c| t.columns[c].as_str());
        assert_eq!(got, want, "{layout:?} {rows}");
    }
}

#[test]
fn detected_column_is_found_by_value() {
    let (_d, t) = open(&generated(csv_gen::Layout::Generic, 5000));
    let c = rownum::detect_table(&t).unwrap();
    for v in [1u64, 2, 777, 4999, 5000] {
        assert_eq!(rownum::find_in_table(&t, c, v), Some(v - 1));
    }
    assert_eq!(rownum::find_in_table(&t, c, 0), None);
    assert_eq!(rownum::find_in_table(&t, c, 5001), None);
}

/// A tool-written counter in front of the data (EvtxECmd-like `RecordNumber`
/// 1..N, plaso-like zero-based `Index`) is found; the same name over gappy,
/// time-sorted ids is not.
#[test]
fn counter_shapes() {
    let mk = |head: &str, f: &dyn Fn(u64) -> u64| {
        let mut s = format!("{head},TimeCreated,EventId\n");
        for i in 0..4000u64 {
            s.push_str(&format!("{},2024-03-11 10:00:{:02},4624\n", f(i), i % 60));
        }
        s
    };
    #[allow(clippy::type_complexity)]
    let cases: [(&str, &dyn Fn(u64) -> u64, bool); 6] = [
        ("RecordNumber", &|i| i + 1, true),
        ("Index", &|i| i, true),
        ("No.", &|i| i + 1, true),
        ("EventRecordId", &|i| i + 1, true),
        ("EventRecordId", &|i| 1 + i * 3 + (i % 5), false),
        ("EventRecordId", &|i| 90_000 + i, false),
    ];
    for (head, f, want) in cases {
        let (_d, t) = open(mk(head, f).as_bytes());
        assert_eq!(rownum::detect_table(&t).is_some(), want, "{head}");
    }
    // Repeated ids (events of one record sharing it).
    let (_d, t) = open(mk("RecordId", &|i| i / 2 + 1).as_bytes());
    assert_eq!(rownum::detect_table(&t), None);
}
