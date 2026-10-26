//! Query text evaluates to the same rows as the equivalent filter tree
//! built by the condition editor (oracle), and printed trees evaluate like
//! the trees they came from.

use std::sync::Arc;

use fhv_engine::filter::{Engine, Expr, Op, Pred, Target};
use fhv_engine::query::{normalize, parse, print};
use fhv_engine::{OpenOptions, Table};
use proptest::prelude::*;

const USERS: [&str; 5] = ["jdoe", "SYSTEM", "system", "asmith", ""];
const INFOS: [&str; 5] = ["logon failed", "Logon OK", "a \"quoted\" word", "x,y", ""];

struct Fx {
    _dir: tempfile::TempDir,
    engine: Engine,
    cols: Vec<String>,
}

fn fixture() -> Fx {
    let mut s = String::from("Channel,Event ID,Info,User,TimeCreated,Size\n");
    for i in 0..3000u32 {
        let info = INFOS[(i % 5) as usize];
        s.push_str(&format!(
            "{},{},\"{}\",{},2024-03-{:02} {:02}:{:02}:00,{}\n",
            ["Security", "System", "security"][(i % 3) as usize],
            [4624, 4625, 4688, 1102][(i % 4) as usize],
            info.replace('"', "\"\""),
            USERS[(i * 7 % 5) as usize],
            1 + i % 3,
            i % 24,
            i % 60,
            i * 37 % 10_000
        ));
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("q.csv");
    std::fs::write(&path, s).unwrap();
    let mut opts = OpenOptions::new(dir.path().join("cache"));
    opts.use_cache = false;
    let t = Table::open(&path, opts).unwrap();
    t.build().unwrap();
    let cols = t.columns.clone();
    Fx {
        _dir: dir,
        engine: Engine::new(Arc::new(t)),
        cols,
    }
}

fn pred(c: usize, op: Op, cs: bool) -> Expr {
    Expr::Pred(Pred {
        target: Target::Column(c),
        op,
        case_sensitive: cs,
    })
}

fn rows(fx: &Fx, e: &Expr) -> Vec<u32> {
    fx.engine.eval(e).unwrap().iter().collect()
}

#[test]
fn queries_equal_condition_editor_filters() {
    let fx = fixture();
    let s = |v: &str| v.to_string();
    // (query, tree the per-column filters / condition editor build)
    let cases: Vec<(&str, Expr)> = vec![
        (
            r#"Channel = "Security" AND ("Event ID" in (4624, 4625) OR Info contains "failed") AND NOT User startswith "SYSTEM""#,
            Expr::And(vec![
                pred(0, Op::Equals(s("Security")), false),
                Expr::Or(vec![
                    pred(1, Op::In(vec![s("4624"), s("4625")]), false),
                    pred(2, Op::Contains(s("failed")), false),
                ]),
                Expr::Not(Box::new(pred(3, Op::BeginsWith(s("SYSTEM")), false))),
            ]),
        ),
        (
            "Channel = Security cs",
            pred(0, Op::Equals(s("Security")), true),
        ),
        (
            "User in (system) cs",
            pred(3, Op::In(vec![s("system")]), true),
        ),
        (
            "User not in (jdoe, system)",
            pred(3, Op::NotIn(vec![s("jdoe"), s("system")]), false),
        ),
        ("User is blank", pred(3, Op::IsBlank, false)),
        ("Info is not blank", pred(2, Op::IsNotBlank, false)),
        (
            r#"Info = 'a "quoted" word'"#,
            pred(2, Op::Equals(s("a \"quoted\" word")), false),
        ),
        ("Info endswith ok", pred(2, Op::EndsWith(s("ok")), false)),
        (
            "Info regex '^lo.*d$'",
            pred(2, Op::Regex(s("^lo.*d$")), false),
        ),
        (
            "Info not contains logon",
            pred(2, Op::NotContains(s("logon")), false),
        ),
        (
            "Size > 5000 AND Size <= 7000",
            Expr::And(vec![
                pred(5, Op::Gt(s("5000")), false),
                pred(5, Op::Le(s("7000")), false),
            ]),
        ),
        (
            "TimeCreated between 2024-03-02 and \"2024-03-02 11:30\"",
            pred(
                4,
                Op::Between(s("2024-03-02"), s("2024-03-02 11:30")),
                false,
            ),
        ),
        (
            "TimeCreated >= 2024-03-03 OR TimeCreated < \"2024-03-01 01:00\"",
            Expr::Or(vec![
                pred(4, Op::Ge(s("2024-03-03")), false),
                pred(4, Op::Lt(s("2024-03-01 01:00")), false),
            ]),
        ),
        (
            "Info not endswith ok AND Size not between 100 and 9000",
            Expr::And(vec![
                pred(2, Op::Not(Box::new(Op::EndsWith(s("ok")))), false),
                pred(
                    5,
                    Op::Not(Box::new(Op::Between(s("100"), s("9000")))),
                    false,
                ),
            ]),
        ),
        (
            "* contains SYSTEM",
            Expr::Pred(Pred {
                target: Target::Any,
                op: Op::Contains(s("SYSTEM")),
                case_sensitive: false,
            }),
        ),
    ];
    for (q, tree) in cases {
        let got = parse(q, &fx.cols, false)
            .unwrap_or_else(|e| panic!("{q}: {e}"))
            .unwrap();
        assert_eq!(got, tree, "{q}");
        let r = rows(&fx, &got);
        assert!(!r.is_empty() && r.len() < 3000, "{q}: {} rows", r.len());
        assert_eq!(r, rows(&fx, &tree), "{q}");
    }
}

fn arb_op() -> impl Strategy<Value = (usize, Op)> {
    let user = prop::sample::select(USERS.to_vec()).prop_map(String::from);
    let info =
        prop::sample::select(vec!["logon", "OK", "\"quoted\"", "x,", "f"]).prop_map(String::from);
    let num = (0u32..10_000).prop_map(|n| n.to_string());
    let day = (1u32..4).prop_map(|d| format!("2024-03-0{d}"));
    prop_oneof![
        user.clone().prop_map(|v| (3, Op::Equals(v))),
        user.clone().prop_map(|v| (3, Op::NotEquals(v))),
        prop::collection::vec(user, 0..3).prop_map(|v| (3, Op::In(v))),
        info.clone().prop_map(|v| (2, Op::Contains(v))),
        info.clone().prop_map(|v| (2, Op::NotContains(v))),
        info.clone().prop_map(|v| (2, Op::BeginsWith(v))),
        info.prop_map(|v| (2, Op::EndsWith(v))),
        Just((3, Op::IsBlank)),
        num.clone().prop_map(|v| (5, Op::Gt(v))),
        num.prop_map(|v| (5, Op::Le(v))),
        (day.clone(), day).prop_map(|(a, b)| (4, Op::Between(a, b))),
    ]
    // Inverted chips (Op::Not), as the chip click builds them.
    .prop_flat_map(|(c, op)| {
        prop_oneof![
            Just((c, op.clone())),
            Just((c, op.negated())),
            Just((c, Op::Not(Box::new(op)))),
        ]
    })
}

fn arb_expr() -> impl Strategy<Value = Expr> {
    let leaf = (arb_op(), any::<bool>()).prop_map(|((c, op), cs)| pred(c, op, cs));
    leaf.prop_recursive(3, 12, 3, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 1..4).prop_map(Expr::And),
            prop::collection::vec(inner.clone(), 1..4).prop_map(Expr::Or),
            inner.prop_map(|x| Expr::Not(Box::new(x))),
        ]
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn printed_trees_select_the_same_rows(tree in arb_expr(), dcs in any::<bool>()) {
        let fx = FX.with(|f| f.clone());
        let text = print(&tree, &fx.cols, dcs).unwrap();
        let back = parse(&text, &fx.cols, dcs).unwrap().unwrap();
        prop_assert_eq!(&back, &normalize(tree.clone()));
        prop_assert_eq!(rows(&fx, &back), rows(&fx, &tree), "{}", text);
    }
}

thread_local! {
    static FX: Arc<Fx> = Arc::new(fixture());
}
