//! `Op::negated` (the chip "invert" of the viewer, D-040): the rows after
//! inversion are exactly the complement of the rows before, whatever the
//! operator, the column kind or the target.

use std::sync::Arc;

use fhv_engine::filter::{Engine, Expr, Op, Pred, Target};
use fhv_engine::{OpenOptions, Table};

#[test]
fn negated_is_the_complement_for_every_operator() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.csv");
    let mut csv = String::from("Time,N,Text\n");
    for i in 0..300u32 {
        let text = ["alpha", "Beta", "", "gamma delta", "ALPHA2", "n/a"][(i % 6) as usize];
        let n = if i % 17 == 0 {
            "x".into()
        } else {
            (i % 50).to_string()
        };
        csv.push_str(&format!(
            "2024-03-{:02} 10:00:{:02},{n},{text}\n",
            1 + i % 28,
            i % 60
        ));
    }
    std::fs::write(&path, csv).unwrap();
    let mut opts = OpenOptions::new(dir.path().join("cache"));
    opts.use_cache = false;
    let t = Table::open(&path, opts).unwrap();
    t.build().unwrap();
    let e = Engine::new(Arc::new(t));
    let s = |x: &str| x.to_string();
    let ops = |c: usize| -> Vec<Op> {
        let mut v = vec![
            Op::Equals(s("alpha")),
            Op::NotEquals(s("alpha")),
            Op::Contains(s("a")),
            Op::NotContains(s("a")),
            Op::BeginsWith(s("al")),
            Op::EndsWith(s("a")),
            Op::IsBlank,
            Op::IsNotBlank,
            Op::In(vec![s("alpha"), s("n/a")]),
            Op::NotIn(vec![s("alpha")]),
            Op::Regex(s("^[ab]")),
        ];
        v.extend(match c {
            0 => vec![
                Op::Gt(s("2024-03-10")),
                Op::Le(s("2024-03-05")),
                Op::Between(s("2024-03-05"), s("2024-03-09")),
            ],
            1 => vec![
                Op::Gt(s("10")),
                Op::Lt(s("5")),
                Op::Between(s("5"), s("20")),
            ],
            _ => vec![Op::Ge(s("b")), Op::Between(s("a"), s("c"))],
        });
        v
    };
    let all: Vec<u32> = (0..300).collect();
    let run = |target: Target, op: Op| -> Vec<u32> {
        e.eval(&Expr::Pred(Pred {
            target,
            op,
            case_sensitive: false,
        }))
        .unwrap()
        .iter()
        .collect()
    };
    for c in 0..3 {
        for op in ops(c) {
            let before = run(Target::Column(c), op.clone());
            let after = run(Target::Column(c), op.negated());
            let want: Vec<u32> = all
                .iter()
                .copied()
                .filter(|r| !before.contains(r))
                .collect();
            assert_eq!(after, want, "col {c} {op:?}");
            assert_eq!(op.negated().negated(), op);
        }
    }
    // Global search target.
    let op = Op::Contains(s("alpha"));
    let before = run(Target::Any, op.clone());
    let after = run(Target::Any, Op::Not(Box::new(op)));
    assert_eq!(before.len() + after.len(), 300);
    assert!(after.iter().all(|r| !before.contains(r)));
}

#[test]
fn natural_opposites_and_wrapping() {
    assert_eq!(Op::Equals("a".into()).negated(), Op::NotEquals("a".into()));
    assert_eq!(Op::IsBlank.negated(), Op::IsNotBlank);
    assert_eq!(Op::In(vec![]).negated(), Op::NotIn(vec![]));
    assert_eq!(
        Op::Regex("x".into()).negated(),
        Op::Not(Box::new(Op::Regex("x".into())))
    );
    let p = Pred {
        target: Target::Column(1),
        op: Op::Between("1".into(), "2".into()).negated(),
        case_sensitive: false,
    };
    let j = serde_json::to_string(&p).unwrap();
    assert_eq!(serde_json::from_str::<Pred>(&j).unwrap(), p);
}
