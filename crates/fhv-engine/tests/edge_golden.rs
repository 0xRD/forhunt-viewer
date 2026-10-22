//! Opens every file of csv-gen's edge-case suite and compares what the
//! engine makes of it with a reviewed golden file in tests/golden/.
//! `UPDATE_GOLDEN=1 cargo test -p fhv-engine --test edge_golden` rewrites
//! them (review the diff!).

use std::fmt::Write as _;

use fhv_engine::{OpenOptions, Table};

/// Cells longer than this are summarised (length + hash) in the golden file.
const MAX_SHOWN: usize = 80;

fn summarise(t: &Table) -> String {
    let mut s = String::new();
    let d = &t.dialect;
    writeln!(
        s,
        "encoding: {}\ndelimiter: {:?}\nheader: {}\ncrlf: {}\ncolumns ({}): {:?}\nrows: {}\nstructural_bad: {:?}",
        t.encoding.name(),
        d.delim as char,
        d.header,
        d.crlf,
        t.columns.len(),
        if t.columns.len() > 12 { t.columns[..12].to_vec() } else { t.columns.clone() },
        t.row_count(),
        t.structural_bad_rows().iter().collect::<Vec<_>>(),
    )
    .unwrap();
    let n = t.row_count();
    let shown: Vec<u64> = if n <= 12 {
        (0..n).collect()
    } else {
        vec![0, 1, 2, n / 2, n - 2, n - 1]
    };
    for r in shown {
        let f = t.row_fields(r).unwrap();
        let cells: Vec<String> = f
            .iter()
            .take(12)
            .map(|c| {
                if c.chars().count() > MAX_SHOWN {
                    format!(
                        "<{} chars, blake3 {}>",
                        c.chars().count(),
                        &blake3::hash(c.as_bytes()).to_hex()[..16]
                    )
                } else {
                    format!("{c:?}")
                }
            })
            .collect();
        writeln!(s, "row {r} ({} fields): {}", f.len(), cells.join(", ")).unwrap();
    }
    s
}

#[test]
fn edge_cases_match_golden() {
    let dir = tempfile::tempdir().unwrap();
    let golden = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden");
    let update = std::env::var_os("UPDATE_GOLDEN").is_some();
    let mut failures = Vec::new();
    for case in csv_gen::edge::cases() {
        let path = dir.path().join(case.name);
        std::fs::write(&path, &case.bytes).unwrap();
        let t = Table::open(&path, OpenOptions::new(dir.path().join("cache"))).unwrap();
        t.build().unwrap();
        // Small chunks too: results must not depend on chunking.
        let mut no_cache = OpenOptions::new(dir.path().join("cache"));
        no_cache.use_cache = false;
        let t2 = Table::open(&path, no_cache).unwrap();
        t2.build_chunked(7).unwrap();
        let got = format!("# {}: {}\n{}", case.name, case.what, summarise(&t));
        assert_eq!(
            summarise(&t),
            summarise(&t2),
            "{}: chunking changed the result",
            case.name
        );
        let gpath = golden.join(format!("{}.txt", case.name));
        if update {
            std::fs::write(&gpath, &got).unwrap();
            continue;
        }
        match std::fs::read_to_string(&gpath) {
            Ok(want) if want.replace("\r\n", "\n") == got => {}
            Ok(want) => failures.push(format!(
                "{}:\n--- golden\n{want}\n--- got\n{got}",
                case.name
            )),
            Err(_) => failures.push(format!("{}: no golden file", case.name)),
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
