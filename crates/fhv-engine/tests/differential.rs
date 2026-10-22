//! The parallel row index + field splitter must agree with the sequential
//! reference reader on arbitrary input, for any chunk size.

mod common;

use std::io::Write;

use fhv_engine::{Dialect, OpenOptions, Table};
use proptest::prelude::*;

fn engine_rows(data: &[u8], chunk: u64) -> Vec<Vec<Vec<u8>>> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.csv");
    std::fs::File::create(&path)
        .unwrap()
        .write_all(data)
        .unwrap();
    let mut opts = OpenOptions::new(dir.path().join("cache"));
    opts.encoding = Some(fhv_engine::Encoding::Utf8);
    opts.dialect = Some(Dialect {
        header: false,
        ..Dialect::default()
    });
    let t = Table::open(&path, opts).unwrap();
    t.build_chunked(chunk).unwrap();
    (0..t.row_count())
        .map(|r| {
            let mut buf = Vec::new();
            let raw = t.raw_row(r, &mut buf).unwrap().to_vec();
            let mut spans = Vec::new();
            fhv_engine::scan::split_row(t.tables(), &raw, &mut spans);
            spans
                .iter()
                .map(|s| {
                    let b = &raw[s.start as usize..s.end as usize];
                    if s.quoted {
                        let mut out = Vec::new();
                        fhv_engine::scan::unquote(b, b'"', &mut out);
                        out
                    } else {
                        b.to_vec()
                    }
                })
                .collect()
        })
        .collect()
}

fn csvish() -> impl Strategy<Value = Vec<u8>> {
    // Small alphabet dense in structure characters.
    proptest::collection::vec(
        prop_oneof![
            4 => Just(b'a'),
            2 => Just(b','),
            2 => Just(b'"'),
            2 => Just(b'\n'),
            1 => Just(b'\r'),
            1 => Just(b' '),
            1 => Just(0u8),
            1 => Just(0xE9u8),
        ],
        0..400,
    )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(400))]
    #[test]
    fn index_matches_reference(data in csvish(), chunk in 1u64..64) {
        let want = common::reference_rows(&data, b',');
        prop_assert_eq!(engine_rows(&data, chunk), want);
    }
}

#[test]
fn generated_layouts_match_reference() {
    for layout in [
        csv_gen::Layout::Forhunt,
        csv_gen::Layout::Mft,
        csv_gen::Layout::Hayabusa,
        csv_gen::Layout::Plaso,
        csv_gen::Layout::Generic,
    ] {
        for crlf in [false, true] {
            let spec = csv_gen::Spec {
                layout,
                seed: 3,
                rows: Some(3000),
                bytes: None,
                delim: b',',
                crlf,
            };
            let mut data = Vec::new();
            csv_gen::generate(&spec, &mut data).unwrap();
            let want = common::reference_rows(&data, b',');
            assert_eq!(want.len(), 3001, "{layout:?}");
            for chunk in [4096, 65_536, 1 << 20] {
                assert_eq!(
                    engine_rows(&data, chunk),
                    want,
                    "{layout:?} crlf={crlf} chunk={chunk}"
                );
            }
        }
    }
}

/// An unterminated quote near the start must not turn the rest of a big file
/// into one row: records over 16 MiB are cut back into lines, flagged bad.
#[test]
fn runaway_quote_is_cut_into_bad_lines() {
    let mut data = b"a,b\n1,2\n3,\"oops\n".to_vec();
    let mut i = 0u64;
    while data.len() < (17 << 20) {
        data.extend_from_slice(format!("{i},row\n").as_bytes());
        i += 1;
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("runaway.csv");
    std::fs::write(&path, &data).unwrap();
    let t = Table::open(&path, OpenOptions::new(dir.path().join("cache"))).unwrap();
    t.build().unwrap();
    // 1 good row, then "3,\"oops" and every following line as its own row.
    assert_eq!(t.row_count(), 2 + i);
    let bad = t.structural_bad_rows();
    assert_eq!(bad.len() as u64, 1 + i);
    assert!(!bad.contains(0) && bad.contains(1));
    assert_eq!(t.row_fields(2).unwrap(), ["0", "row"]);
    assert_eq!(
        t.row_fields(1 + i).unwrap(),
        [(i - 1).to_string(), "row".into()]
    );
}
