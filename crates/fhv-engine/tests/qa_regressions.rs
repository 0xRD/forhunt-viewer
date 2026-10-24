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
