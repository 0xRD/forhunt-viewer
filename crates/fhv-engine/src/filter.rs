//! Filter engine: an expression tree of column predicates, evaluated to
//! roaring bitmaps of data rows.
//!
//! Each leaf predicate produces a bitmap over all rows, cached by its
//! canonical form, so editing one condition of a filter re-evaluates only
//! that condition. Groups combine with bitmap algebra. Strategy per leaf:
//! - dictionary column: the predicate runs once per distinct value and the
//!   matching values' bitmaps are OR-ed (instant);
//! - primary time column with a range operator: a linear pass over the
//!   parsed ticks;
//! - otherwise a parallel scan over the file. For literal text predicates
//!   (contains, equals, begins/ends with, in) a case-insensitive regex over
//!   whole blocks of rows finds candidate rows first, so rows that cannot
//!   match are never split into fields.
//!
//! Semantics (also implemented by the naive reference in tests):
//! text comparisons are case-insensitive unless `case_sensitive`, using the
//! regex crate's simple Unicode case folding (the same rules as the block
//! prefilter, so the prefilter can never miss a match); a missing
//! field (short row) is an empty string; `>`/`<`/between compare as times
//! when the column is a time column and the operand parses as one, else as
//! numbers when both sides parse, else case-insensitively as text (on a
//! time column, an operand that is not a time is an error: D-036); cells that
//! do not parse in a time/number comparison do not match. An inclusive upper
//! time bound (`<=`, the end of `between`) covers the whole period written:
//! `between 2024-03-01 and 2024-03-01` is that day (D-034).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use regex::bytes::{Regex, RegexBuilder};
use roaring::RoaringBitmap;
use serde::{Deserialize, Serialize};

use crate::columns::{Columns, Kind, Num, field_bytes, parse_num};
use crate::scan::nth_field;
use crate::table::Table;
use crate::time::{self, Fmt, Order};
use crate::{Error, Result};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "op", content = "value")]
pub enum Op {
    Equals(String),
    NotEquals(String),
    Contains(String),
    NotContains(String),
    BeginsWith(String),
    EndsWith(String),
    IsBlank,
    IsNotBlank,
    In(Vec<String>),
    NotIn(Vec<String>),
    Regex(String),
    Gt(String),
    Ge(String),
    Lt(String),
    Le(String),
    Between(String, String),
    /// Complement of another operator (the rows it does not match, blank
    /// and unparsable cells included). Only at the top of a predicate.
    Not(Box<Op>),
}

impl Op {
    /// The operator matching exactly the rows this one does not: its
    /// natural opposite when it has one, else `Not` (and `Not(x)` is `x`).
    pub fn negated(&self) -> Op {
        match self {
            Op::Equals(v) => Op::NotEquals(v.clone()),
            Op::NotEquals(v) => Op::Equals(v.clone()),
            Op::Contains(v) => Op::NotContains(v.clone()),
            Op::NotContains(v) => Op::Contains(v.clone()),
            Op::In(v) => Op::NotIn(v.clone()),
            Op::NotIn(v) => Op::In(v.clone()),
            Op::IsBlank => Op::IsNotBlank,
            Op::IsNotBlank => Op::IsBlank,
            Op::Not(x) => (**x).clone(),
            other => Op::Not(Box::new(other.clone())),
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Op::Equals(_) => "equals",
            Op::NotEquals(_) => "not equals",
            Op::Contains(_) => "contains",
            Op::NotContains(_) => "not contains",
            Op::BeginsWith(_) => "begins with",
            Op::EndsWith(_) => "ends with",
            Op::IsBlank => "is blank",
            Op::IsNotBlank => "is not blank",
            Op::In(_) => "in",
            Op::NotIn(_) => "not in",
            Op::Regex(_) => "regex",
            Op::Gt(_) => ">",
            Op::Ge(_) => ">=",
            Op::Lt(_) => "<",
            Op::Le(_) => "<=",
            Op::Between(..) => "between",
            Op::Not(_) => "not",
        }
    }
}

/// Which cells a predicate looks at.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Target {
    Column(usize),
    /// Any column (global search).
    Any,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Pred {
    pub target: Target,
    #[serde(flatten)]
    pub op: Op,
    #[serde(default)]
    pub case_sensitive: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Expr {
    And(Vec<Expr>),
    Or(Vec<Expr>),
    Not(Box<Expr>),
    Pred(Pred),
    /// A fixed set of data rows (tagged rows, bad rows...). Not serialized
    /// by value in sessions; the app rebuilds it.
    #[serde(skip)]
    Rows(Arc<RoaringBitmap>),
}

/// Typed comparison operand of `>`, `<`, between.
#[derive(Clone, Copy, Debug)]
enum Bound {
    Time(i64),
    Num(Num),
}

/// A predicate compiled for one column.
pub struct Compiled {
    op: Op,
    ci: bool,
    /// Operand(s) lowercased when case-insensitive.
    needles: Vec<Vec<u8>>,
    /// User regex, or the case-insensitive matcher of a text operator.
    regex: Option<Regex>,
    /// `in` / `not in`: the values, case-folded when case-insensitive (a
    /// hash lookup per cell: lists of thousands of IOCs stay fast, where an
    /// alternation regex was slow or over the size limit).
    set: Option<rustc_hash::FxHashSet<Vec<u8>>>,
    /// For range operators: typed bounds (lo, hi) when the operand parses.
    range: Option<(Option<Bound>, Option<Bound>, bool, bool)>,
    /// Text fallback bounds for ranges.
    text_lo: Option<Vec<u8>>,
    text_hi: Option<Vec<u8>>,
    order: Option<Fmt>,
}

fn lower(b: &[u8]) -> Vec<u8> {
    // ASCII folding keeps byte lengths; non-ASCII letters are folded via
    // char lowercasing when the input is valid UTF-8.
    match std::str::from_utf8(b) {
        Ok(s) if !s.is_ascii() => s.to_lowercase().into_bytes(),
        _ => b.to_ascii_lowercase(),
    }
}

fn find(hay: &[u8], needle: &[u8]) -> bool {
    needle.is_empty() || memchr::memmem::find(hay, needle).is_some()
}

impl Compiled {
    pub fn new(op: &Op, case_sensitive: bool, kind: Kind) -> Result<Compiled> {
        if matches!(op, Op::Not(_)) {
            return Err(Error::Other("internal: unresolved NOT operator".into()));
        }
        let ci = !case_sensitive;
        let norm = |s: &str| {
            if ci {
                lower(s.as_bytes())
            } else {
                s.as_bytes().to_vec()
            }
        };
        let needles = match op {
            Op::Equals(v)
            | Op::NotEquals(v)
            | Op::Contains(v)
            | Op::NotContains(v)
            | Op::BeginsWith(v)
            | Op::EndsWith(v) => {
                vec![v.as_bytes().to_vec()]
            }
            Op::In(vs) | Op::NotIn(vs) => vs.iter().map(|v| v.as_bytes().to_vec()).collect(),
            _ => Vec::new(),
        };
        let lit = |v: &str| regex::escape(v);
        let pattern = match op {
            Op::Regex(r) => Some(r.clone()),
            _ if !ci => None,
            Op::Equals(v) | Op::NotEquals(v) => Some(format!("^(?:{})$", lit(v))),
            Op::Contains(v) | Op::NotContains(v) => Some(lit(v)),
            Op::BeginsWith(v) => Some(format!("^(?:{})", lit(v))),
            Op::EndsWith(v) => Some(format!("(?:{})$", lit(v))),
            _ => None,
        };
        let set = match op {
            Op::In(vs) | Op::NotIn(vs) => Some(
                vs.iter()
                    .filter_map(|v| {
                        if ci {
                            fold_simple(v.as_bytes())
                        } else {
                            Some(v.as_bytes().to_vec())
                        }
                    })
                    .collect(),
            ),
            _ => None,
        };
        let regex = match pattern {
            Some(p) => Some(
                RegexBuilder::new(&p)
                    .case_insensitive(ci)
                    .size_limit(1 << 24)
                    .build()
                    .map_err(|e| Error::Other(format!("regex: {e}")))?,
            ),
            None => None,
        };
        let order = match kind {
            Kind::Time(o) => Some(o),
            _ => None,
        };
        // An inclusive upper time bound covers the whole period it names
        // (`<= 2024-03-01` is the whole day, `<= 10:00:00` the whole
        // second); other bounds start at its first tick (D-034).
        let bound = |s: &str, upper_incl: bool| -> Option<Bound> {
            if let Some(o) = order
                && let Some((t, span, _)) = time::parse_operand(s.as_bytes(), o)
            {
                return Some(Bound::Time(if upper_incl {
                    t.saturating_add(span - 1)
                } else {
                    t
                }));
            }
            parse_num(s.as_bytes()).map(Bound::Num)
        };
        // On a time column a range operand must be a date-time: a number
        // or text comparison of times silently returns nothing or nonsense
        // (`>= 2025`, a day-first date on a month-first column).
        if let Some(o) = order {
            let operands: Vec<&String> = match op {
                Op::Gt(v) | Op::Ge(v) | Op::Lt(v) | Op::Le(v) => vec![v],
                Op::Between(a, b) => vec![a, b],
                _ => Vec::new(),
            };
            if let Some(v) = operands
                .into_iter()
                .find(|v| time::parse_operand(v.as_bytes(), o).is_none())
            {
                let own = match o.order {
                    Order::Mdy => ", 3/11/2024 10:00 AM",
                    Order::Dmy => ", 11/3/2024 10:00",
                    Order::Ymd => "",
                };
                return Err(Error::Other(format!(
                    "{v:?} is not a date-time for this time column: use e.g. 2024-03-11, \
                     2024-03-11 10:00:00, 2024-03-11T10:00:00Z{own}"
                )));
            }
        }
        let (range, text_lo, text_hi) = match op {
            Op::Gt(v) => (
                Some((bound(v, false), None, false, true)),
                Some(norm(v)),
                None,
            ),
            Op::Ge(v) => (
                Some((bound(v, false), None, true, true)),
                Some(norm(v)),
                None,
            ),
            Op::Lt(v) => (
                Some((None, bound(v, false), true, false)),
                None,
                Some(norm(v)),
            ),
            Op::Le(v) => (
                Some((None, bound(v, true), true, true)),
                None,
                Some(norm(v)),
            ),
            Op::Between(a, b) => (
                Some((bound(a, false), bound(b, true), true, true)),
                Some(norm(a)),
                Some(norm(b)),
            ),
            _ => (None, None, None),
        };
        Ok(Compiled {
            op: op.clone(),
            ci,
            needles,
            regex,
            set,
            range,
            text_lo,
            text_hi,
            order,
        })
    }

    /// Evaluates the predicate on one decoded cell.
    pub fn matches(&self, cell: &[u8]) -> bool {
        let re = |c: &[u8]| self.regex.as_ref().is_some_and(|r| r.is_match(c));
        let ci = self.ci;
        match &self.op {
            Op::Equals(_) => {
                if ci {
                    re(cell)
                } else {
                    cell == self.needles[0]
                }
            }
            Op::NotEquals(_) => {
                !(if ci {
                    re(cell)
                } else {
                    cell == self.needles[0]
                })
            }
            Op::Contains(_) => {
                if ci {
                    re(cell)
                } else {
                    find(cell, &self.needles[0])
                }
            }
            Op::NotContains(_) => {
                !(if ci {
                    re(cell)
                } else {
                    find(cell, &self.needles[0])
                })
            }
            Op::BeginsWith(_) => {
                if ci {
                    re(cell)
                } else {
                    cell.starts_with(&self.needles[0])
                }
            }
            Op::EndsWith(_) => {
                if ci {
                    re(cell)
                } else {
                    cell.ends_with(&self.needles[0])
                }
            }
            Op::IsBlank => cell.iter().all(u8::is_ascii_whitespace),
            Op::IsNotBlank => !cell.iter().all(u8::is_ascii_whitespace),
            Op::In(_) => self.in_set(cell),
            Op::NotIn(_) => !self.in_set(cell),
            Op::Regex(_) => re(cell),
            Op::Gt(_) | Op::Ge(_) | Op::Lt(_) | Op::Le(_) | Op::Between(..) => self.in_range(cell),
            Op::Not(_) => false,
        }
    }

    fn in_set(&self, cell: &[u8]) -> bool {
        let Some(set) = &self.set else { return false };
        if set.is_empty() {
            return false;
        }
        if self.ci {
            fold_simple(cell).is_some_and(|f| set.contains(&f))
        } else {
            set.contains(cell)
        }
    }

    /// Range test. Typed when every given bound parses the same way (all
    /// times on a time column, or all numbers), else case-insensitive text.
    fn in_range(&self, cell: &[u8]) -> bool {
        let Some((lo, hi, lo_incl, hi_incl)) = self.range else {
            return false;
        };
        let cmp = |o: std::cmp::Ordering, incl: bool, want: std::cmp::Ordering| {
            o == want || (incl && o.is_eq())
        };
        use std::cmp::Ordering::{Greater, Less};
        match self.typed() {
            Some(Bound::Time(_)) => {
                let Some(t) = self.order.and_then(|o| time::parse(cell, o)) else {
                    return false;
                };
                let ok_lo =
                    !matches!(lo, Some(Bound::Time(l)) if !cmp(t.cmp(&l), lo_incl, Greater));
                let ok_hi = !matches!(hi, Some(Bound::Time(h)) if !cmp(t.cmp(&h), hi_incl, Less));
                ok_lo && ok_hi
            }
            Some(Bound::Num(_)) => {
                let Some(x) = parse_num(cell) else {
                    return false;
                };
                let ok_lo =
                    !matches!(lo, Some(Bound::Num(l)) if !cmp(x.total_cmp(l), lo_incl, Greater));
                let ok_hi =
                    !matches!(hi, Some(Bound::Num(h)) if !cmp(x.total_cmp(h), hi_incl, Less));
                ok_lo && ok_hi
            }
            None => {
                let c = if self.ci { lower(cell) } else { cell.to_vec() };
                let ok_lo = self
                    .text_lo
                    .as_ref()
                    .is_none_or(|l| cmp(c.as_slice().cmp(l), lo_incl, Greater));
                let ok_hi = self
                    .text_hi
                    .as_ref()
                    .is_none_or(|h| cmp(c.as_slice().cmp(h), hi_incl, Less));
                ok_lo && ok_hi
            }
        }
    }

    /// The common type of the range bounds, if all given bounds share one.
    fn typed(&self) -> Option<Bound> {
        let (lo, hi, ..) = self.range?;
        let given = [self.text_lo.is_some(), self.text_hi.is_some()];
        let parsed = [lo, hi];
        let mut kind: Option<Bound> = None;
        for (g, p) in given.iter().zip(parsed) {
            if !g {
                continue;
            }
            match (p, kind) {
                (None, _) => return None,
                (Some(b), None) => kind = Some(b),
                (Some(Bound::Time(_)), Some(Bound::Time(_)))
                | (Some(Bound::Num(_)), Some(Bound::Num(_))) => {}
                _ => return None,
            }
        }
        kind
    }

    /// Ticks bounds for the time index fast path.
    fn time_bounds(&self) -> Option<(i64, i64)> {
        let (lo, hi, lo_incl, hi_incl) = self.range?;
        if !matches!(self.typed(), Some(Bound::Time(_))) {
            return None;
        }
        let l = match lo {
            Some(Bound::Time(l)) => {
                if lo_incl {
                    l
                } else {
                    l.checked_add(1)?
                }
            }
            None => i64::MIN + 1,
            Some(Bound::Num(_)) => return None,
        };
        let h = match hi {
            Some(Bound::Time(h)) => {
                if hi_incl {
                    h
                } else {
                    h.checked_sub(1)?
                }
            }
            None => i64::MAX,
            Some(Bound::Num(_)) => return None,
        };
        (lo.is_some() || hi.is_some()).then_some((l, h))
    }

    /// A regex finding every row that can match a positive literal
    /// predicate (raw bytes; quoted cells hold the literal unless it has a
    /// quote). `None` = scan every row.
    fn prefilter(&self) -> Option<Regex> {
        let lits: Vec<Vec<u8>> = match &self.op {
            // A case-insensitive alternation of many values is slow to
            // build and run: scan every row (hash lookups) instead.
            Op::In(v) if self.ci && v.len() > IN_PREFILTER_MAX => return None,
            Op::Equals(_) | Op::Contains(_) | Op::BeginsWith(_) | Op::EndsWith(_) | Op::In(_) => {
                self.needles.clone()
            }
            Op::Regex(p) => return regex_prefilter(p, self.ci),
            _ => return None,
        };
        literal_prefilter(&lits, self.ci)
    }

    fn is_negative(&self) -> bool {
        matches!(
            self.op,
            Op::NotEquals(_) | Op::NotContains(_) | Op::NotIn(_) | Op::IsNotBlank
        )
    }

    /// The positive form of a negated operator (`not contains x` = all rows
    /// minus `contains x`).
    fn positive(&self) -> Option<Op> {
        Some(match &self.op {
            Op::NotEquals(v) => Op::Equals(v.clone()),
            Op::NotContains(v) => Op::Contains(v.clone()),
            Op::NotIn(v) => Op::In(v.clone()),
            Op::IsNotBlank => Op::IsBlank,
            _ => return None,
        })
    }
}

/// Largest case-insensitive `in` list searched with a literal prefilter.
const IN_PREFILTER_MAX: usize = 32;

/// Canonical form of `b` under Unicode simple case folding (each character
/// replaced by the smallest member of its folding orbit: `k`, `K` and the
/// Kelvin sign all become `K`), so two strings are equal case-insensitively
/// exactly when the regex crate's `(?i)` literal match says so (D-012).
/// None when `b` is not UTF-8 (it cannot equal a UTF-8 value then).
fn fold_simple(b: &[u8]) -> Option<Vec<u8>> {
    if b.is_ascii() {
        return Some(b.to_ascii_uppercase());
    }
    let s = std::str::from_utf8(b).ok()?;
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        out.push(if c.is_ascii() {
            c.to_ascii_uppercase()
        } else {
            fold_char(c)
        });
    }
    Some(out.into_bytes())
}

fn fold_char(c: char) -> char {
    use regex_syntax::hir::{ClassUnicode, ClassUnicodeRange};
    thread_local! {
        static CACHE: std::cell::RefCell<rustc_hash::FxHashMap<char, char>> =
            Default::default();
    }
    CACHE.with(|m| {
        *m.borrow_mut().entry(c).or_insert_with(|| {
            let mut cls = ClassUnicode::new([ClassUnicodeRange::new(c, c)]);
            cls.case_fold_simple();
            cls.ranges().first().map_or(c, |r| r.start())
        })
    })
}

/// Prefilter regex from literals: `None` when a literal could be spelled
/// differently in the raw bytes (a quote, written `""`) or a newline could
/// hide an overlapping match in the next row.
fn literal_prefilter(lits: &[Vec<u8>], ci: bool) -> Option<Regex> {
    if lits.is_empty()
        || lits
            .iter()
            .any(|l| l.is_empty() || l.iter().any(|b| matches!(b, b'"' | b'\n' | b'\r')))
    {
        return None;
    }
    let alts: Vec<String> = lits
        .iter()
        .map(|l| regex::escape(&String::from_utf8_lossy(l)))
        .collect();
    RegexBuilder::new(&alts.join("|"))
        .case_insensitive(ci)
        .unicode(true)
        .build()
        .ok()
}

/// Prefilter for a user regex: every match starts with one of the literal
/// prefixes `regex-syntax` extracts (case variants included), so a cell can
/// only match when its raw bytes contain one of them. None when the prefix
/// set is unbounded or can be empty (e.g. `.*x`, `a?`).
fn regex_prefilter(pattern: &str, ci: bool) -> Option<Regex> {
    use regex_syntax::hir::literal::{ExtractKind, Extractor};
    let hir = regex_syntax::ParserBuilder::new()
        .case_insensitive(ci)
        .build()
        .parse(pattern)
        .ok()?;
    let mut ex = Extractor::new();
    ex.kind(ExtractKind::Prefix);
    let seq = ex.extract(&hir);
    let lits: Vec<Vec<u8>> = seq
        .literals()?
        .iter()
        .map(|l| l.as_bytes().to_vec())
        .collect();
    // Case variants are already in the set: match them exactly.
    literal_prefilter(&lits, false)
}

const SCAN_BLOCK: u64 = 32_768;

/// Evaluates expressions over a table, caching leaf results.
pub struct Engine {
    table: Arc<Table>,
    cache: Mutex<HashMap<String, Arc<RoaringBitmap>>>,
    /// Column kinds set by the analyst (e.g. day/month order of a date
    /// column), overriding the profiled kind.
    kinds: Mutex<HashMap<usize, Kind>>,
    /// Blank and unparsable cell counts per (column, kind).
    unparsable: Mutex<HashMap<(usize, String), (u64, u64)>>,
}

impl Engine {
    pub fn new(table: Arc<Table>) -> Self {
        Engine {
            table,
            cache: Mutex::new(HashMap::new()),
            kinds: Mutex::new(HashMap::new()),
            unparsable: Mutex::new(HashMap::new()),
        }
    }

    pub fn table(&self) -> &Arc<Table> {
        &self.table
    }

    /// Overrides the kind of column `c` (None: back to the profiled kind).
    /// Cached results are dropped.
    pub fn set_kind(&self, c: usize, kind: Option<Kind>) {
        if let Ok(mut k) = self.kinds.lock() {
            match kind {
                Some(x) => k.insert(c, x),
                None => k.remove(&c),
            };
        }
        self.clear_cache();
    }

    /// Kind set by [`Engine::set_kind`], if any.
    pub fn kind_override(&self, c: usize) -> Option<Kind> {
        self.kinds.lock().ok().and_then(|k| k.get(&c).copied())
    }

    /// All overridden kinds.
    pub fn kind_overrides(&self) -> HashMap<usize, Kind> {
        self.kinds.lock().map(|k| k.clone()).unwrap_or_default()
    }

    /// Kind of column `c` as filters and sorts use it.
    pub fn kind(&self, c: usize) -> Kind {
        self.kind_override(c).unwrap_or_else(|| {
            self.table
                .column_index()
                .and_then(|ci| ci.cols.get(c).map(|x| x.kind))
                .unwrap_or(Kind::Text)
        })
    }

    /// Cells of column `c` that a time / number comparison cannot use, and
    /// that range filters therefore exclude: (blank, not blank but not a
    /// date-time / number of the column's kind). (0, 0) for text columns.
    pub fn unparsable(&self, c: usize) -> Result<(u64, u64)> {
        let kind = self.kind(c);
        let parses = |v: &[u8]| match kind {
            Kind::Time(o) => time::parse(v, o).is_some(),
            Kind::Number => parse_num(v).is_some(),
            Kind::Text => true,
        };
        if kind == Kind::Text || c >= self.table.columns.len() {
            return Ok((0, 0));
        }
        let key = (c, format!("{kind:?}"));
        if let Some(v) = self
            .unparsable
            .lock()
            .map_err(|_| Error::Poisoned)?
            .get(&key)
        {
            return Ok(*v);
        }
        let blank = |v: &[u8]| v.iter().all(u8::is_ascii_whitespace);
        let mut out = (0u64, 0u64);
        match self
            .table
            .column_index()
            .and_then(|ci| ci.cols[c].dict.clone())
        {
            Some(d) => {
                for (v, b) in d.values.iter().zip(&d.bitmaps) {
                    if blank(v.as_bytes()) {
                        out.0 += b.len();
                    } else if !parses(v.as_bytes()) {
                        out.1 += b.len();
                    }
                }
            }
            None => {
                let t = self.table.tables();
                let quote = self.table.dialect.quote;
                let n = self.table.row_count();
                self.table.par_blocks(
                    0..n,
                    SCAN_BLOCK,
                    |b| {
                        let (mut nb, mut nu) = (0u64, 0u64);
                        let mut tmp = Vec::new();
                        for k in 0..b.len() {
                            let row = b.row(k);
                            let cell = match nth_field(t, row, c) {
                                Some(s) => field_bytes(row, s, quote, &mut tmp),
                                None => &[][..],
                            };
                            if blank(cell) {
                                nb += 1;
                            } else if !parses(cell) {
                                nu += 1;
                            }
                        }
                        (nb, nu)
                    },
                    |_, (nb, nu)| {
                        out.0 += nb;
                        out.1 += nu;
                        Ok(())
                    },
                )?;
            }
        }
        self.unparsable
            .lock()
            .map_err(|_| Error::Poisoned)?
            .insert(key, out);
        Ok(out)
    }

    fn all_rows(&self) -> RoaringBitmap {
        let mut b = RoaringBitmap::new();
        b.insert_range(0..self.table.row_count() as u32);
        b
    }

    /// Compiles every predicate of `expr` (bad regexes are reported even
    /// when an earlier AND term already matches nothing).
    pub fn validate(&self, expr: &Expr) -> Result<()> {
        match expr {
            Expr::And(xs) | Expr::Or(xs) => xs.iter().try_for_each(|x| self.validate(x)),
            Expr::Not(x) => self.validate(x),
            Expr::Pred(Pred {
                op: Op::Not(x),
                target,
                case_sensitive,
            }) => self.validate(&Expr::Pred(Pred {
                target: target.clone(),
                op: (**x).clone(),
                case_sensitive: *case_sensitive,
            })),
            Expr::Pred(p) => {
                // With the column's kind: a time column checks its range
                // operands (D-036) even when an AND term before it already
                // matches nothing.
                let kind = match &p.target {
                    Target::Column(c) => self.kind(*c),
                    Target::Any => Kind::Text,
                };
                Compiled::new(&p.op, p.case_sensitive, kind).map(|_| ())
            }
            Expr::Rows(_) => Ok(()),
        }
    }

    /// Rows matching `expr`.
    pub fn eval(&self, expr: &Expr) -> Result<RoaringBitmap> {
        Ok(match expr {
            Expr::And(xs) => {
                let mut it = xs.iter();
                let Some(first) = it.next() else {
                    return Ok(self.all_rows());
                };
                let mut acc = self.eval(first)?;
                for x in it {
                    if acc.is_empty() {
                        break;
                    }
                    acc &= self.eval(x)?;
                }
                acc
            }
            Expr::Or(xs) => {
                let mut acc = RoaringBitmap::new();
                for x in xs {
                    acc |= self.eval(x)?;
                }
                acc
            }
            Expr::Not(x) => self.all_rows() - self.eval(x)?,
            Expr::Rows(r) => (**r).clone() & self.all_rows(),
            Expr::Pred(p) => (*self.leaf(p)?).clone(),
        })
    }

    /// Bitmap of one predicate (cached).
    pub fn leaf(&self, p: &Pred) -> Result<Arc<RoaringBitmap>> {
        if let Op::Not(x) = &p.op {
            let inner = self.leaf(&Pred {
                target: p.target.clone(),
                op: (**x).clone(),
                case_sensitive: p.case_sensitive,
            })?;
            return Ok(Arc::new(self.all_rows() - &*inner));
        }
        let mut key = serde_json::to_string(p).unwrap_or_default();
        if let Target::Column(c) = p.target
            && let Some(k) = self.kind_override(c)
        {
            key.push_str(&format!("{k:?}"));
        }
        if let Some(b) = self.cache.lock().map_err(|_| Error::Poisoned)?.get(&key) {
            return Ok(b.clone());
        }
        let b = Arc::new(self.compute(p)?);
        self.cache
            .lock()
            .map_err(|_| Error::Poisoned)?
            .insert(key, b.clone());
        Ok(b)
    }

    pub fn clear_cache(&self) {
        if let Ok(mut c) = self.cache.lock() {
            c.clear();
        }
    }

    fn compute(&self, p: &Pred) -> Result<RoaringBitmap> {
        let cols = self.table.column_index();
        match p.target {
            Target::Any => self.scan_any(p),
            Target::Column(c) => {
                if c >= self.table.columns.len() {
                    return Err(Error::Other(format!("no column {c}")));
                }
                let kind = self.kind(c);
                let cp = Compiled::new(&p.op, p.case_sensitive, kind)?;
                if let Some(ci) = &cols {
                    if let Some(b) = self.via_dict(ci, c, &cp) {
                        return Ok(b);
                    }
                    // The time index holds ticks of the profiled kind.
                    if self.kind_override(c).is_none()
                        && let Some(b) = self.via_time(ci, c, &cp)
                    {
                        return Ok(b);
                    }
                }
                if cp.is_negative()
                    && let Some(pos) = cp.positive()
                {
                    let pos = self.leaf(&Pred {
                        target: p.target.clone(),
                        op: pos,
                        case_sensitive: p.case_sensitive,
                    })?;
                    return Ok(self.all_rows() - &*pos);
                }
                self.scan_column(c, &cp)
            }
        }
    }

    fn via_dict(&self, ci: &Columns, c: usize, cp: &Compiled) -> Option<RoaringBitmap> {
        let d = ci.cols[c].dict.as_ref()?;
        let mut acc = RoaringBitmap::new();
        for (v, b) in d.values.iter().zip(&d.bitmaps) {
            if cp.matches(v.as_bytes()) {
                acc |= b;
            }
        }
        // Rows shorter than this column are in the "" value's bitmap.
        Some(acc)
    }

    fn via_time(&self, ci: &Columns, c: usize, cp: &Compiled) -> Option<RoaringBitmap> {
        let t = ci.cols[c].time.as_ref()?;
        let (lo, hi) = cp.time_bounds()?;
        let mut b = RoaringBitmap::new();
        for (r, &x) in t.ticks.iter().enumerate() {
            if x != i64::MIN && x >= lo && x <= hi {
                let _ = b.try_push(r as u32);
            }
        }
        Some(b)
    }

    fn scan_column(&self, c: usize, cp: &Compiled) -> Result<RoaringBitmap> {
        let t = self.table.tables();
        let quote = self.table.dialect.quote;
        let pre = cp.prefilter();
        let odd = self
            .table
            .column_index()
            .map(|ci| ci.odd_quotes.clone())
            .filter(|o| !o.is_empty());
        let mut out = RoaringBitmap::new();
        let n = self.table.row_count();
        self.table.par_blocks(
            0..n,
            SCAN_BLOCK,
            |b| {
                let mut rows = Vec::new();
                let mut tmp = Vec::new();
                let test = |k: usize| {
                    let row = b.row(k);
                    let cell = match nth_field(t, row, c) {
                        Some(s) => field_bytes(row, s, quote, &mut tmp),
                        None => &[][..],
                    };
                    if cp.matches(cell) {
                        rows.push((b.first + k as u64) as u32);
                    }
                };
                match &pre {
                    Some(re) => {
                        // Candidate rows: prefilter hits plus rows whose
                        // decoded cells are not raw substrings, in order.
                        let r0 = b.first as u32;
                        let mut cand: Vec<usize> = Vec::new();
                        for m in re.find_iter(b.bytes()) {
                            let k = b.row_at(m.start());
                            if k < b.len() && cand.last() != Some(&k) {
                                cand.push(k);
                            }
                        }
                        if let Some(odd) = &odd {
                            cand.extend(
                                odd.range(r0..r0 + b.len() as u32)
                                    .map(|r| (r - r0) as usize),
                            );
                            cand.sort_unstable();
                            cand.dedup();
                        }
                        cand.into_iter().for_each(test);
                    }
                    None => (0..b.len()).for_each(test),
                }
                rows
            },
            |_, rows| {
                let _ = out.append(rows);
                Ok(())
            },
        )?;
        Ok(out)
    }

    /// Any column matches (global search).
    /// Fields past the header (ragged rows) are searched too, as text.
    fn scan_any(&self, p: &Pred) -> Result<RoaringBitmap> {
        let cp = Compiled::new(&p.op, p.case_sensitive, Kind::Text)?;
        // Each column compares with its own type (time / number ranges).
        let per_col: Vec<Compiled> = (0..self.table.columns.len())
            .map(|c| {
                let kind = self.kind(c);
                // Any column: an operand that is not a time compares the
                // time columns as text rather than failing the search.
                Compiled::new(&p.op, p.case_sensitive, kind)
                    .or_else(|_| Compiled::new(&p.op, p.case_sensitive, Kind::Text))
            })
            .collect::<Result<_>>()?;
        if cp.is_negative()
            && let Some(pos) = cp.positive()
        {
            let pos = self.leaf(&Pred {
                target: Target::Any,
                op: pos,
                case_sensitive: p.case_sensitive,
            })?;
            return Ok(self.all_rows() - &*pos);
        }
        let t = self.table.tables();
        let quote = self.table.dialect.quote;
        let ncols = self.table.columns.len();
        let pre = cp.prefilter();
        let odd = self
            .table
            .column_index()
            .map(|ci| ci.odd_quotes.clone())
            .filter(|o| !o.is_empty());
        let mut out = RoaringBitmap::new();
        let n = self.table.row_count();
        self.table.par_blocks(
            0..n,
            SCAN_BLOCK,
            |b| {
                let mut rows = Vec::new();
                let mut spans = Vec::new();
                let mut tmp = Vec::new();
                let test = |k: usize| {
                    let row = b.row(k);
                    crate::scan::split_row(t, row, &mut spans);
                    // Missing fields of a short row are empty cells.
                    let hit = spans.iter().enumerate().any(|(c, s)| {
                        per_col
                            .get(c)
                            .unwrap_or(&cp)
                            .matches(field_bytes(row, *s, quote, &mut tmp))
                    }) || (spans.len()..ncols).any(|c| per_col[c].matches(b""));
                    if hit {
                        rows.push((b.first + k as u64) as u32);
                    }
                };
                match &pre {
                    Some(re) => {
                        // Candidate rows: prefilter hits plus rows whose
                        // decoded cells are not raw substrings, in order.
                        let r0 = b.first as u32;
                        let mut cand: Vec<usize> = Vec::new();
                        for m in re.find_iter(b.bytes()) {
                            let k = b.row_at(m.start());
                            if k < b.len() && cand.last() != Some(&k) {
                                cand.push(k);
                            }
                        }
                        if let Some(odd) = &odd {
                            cand.extend(
                                odd.range(r0..r0 + b.len() as u32)
                                    .map(|r| (r - r0) as usize),
                            );
                            cand.sort_unstable();
                            cand.dedup();
                        }
                        cand.into_iter().for_each(test);
                    }
                    None => (0..b.len()).for_each(test),
                }
                rows
            },
            |_, rows| {
                let _ = out.append(rows);
                Ok(())
            },
        )?;
        Ok(out)
    }
}

/// Value list of a column for the filter dropdown: (value, count) within
/// `within` (all rows when `None`). Dictionary columns only.
pub fn value_counts(
    cols: &Columns,
    c: usize,
    within: Option<&RoaringBitmap>,
) -> Option<Vec<(String, u64)>> {
    let d = cols.cols.get(c)?.dict.as_ref()?;
    Some(
        d.values
            .iter()
            .zip(&d.bitmaps)
            .map(|(v, b)| (v.clone(), within.map_or(b.len(), |w| b.intersection_len(w))))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regex_prefilter_engages_only_when_safe() {
        assert!(regex_prefilter("-enc\\s+[A-Za-z0-9+/=]{40,}", false).is_some());
        assert!(regex_prefilter("id-1[0-9]+", false).is_some());
        let ci = regex_prefilter("(?i)mimikatz", false).unwrap();
        assert!(ci.is_match(b"x MimiKatz.exe"));
        assert!(regex_prefilter("(?i)mimikatz", true).is_some());
        // Unbounded or possibly empty prefixes: no prefilter.
        assert!(regex_prefilter(".*x", false).is_none());
        assert!(regex_prefilter("a?", false).is_none());
        assert!(regex_prefilter("^$", false).is_none());
        // A quote in the literal may be spelled "" in the file.
        assert!(regex_prefilter("q\"t", false).is_none());
    }
}
