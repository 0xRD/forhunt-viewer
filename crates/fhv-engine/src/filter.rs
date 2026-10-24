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
//! numbers when both sides parse, else case-insensitively as text; cells that
//! do not parse in a time/number comparison do not match. An inclusive upper
//! time bound (`<=`, the end of `between`) covers the whole period written:
//! `between 2024-03-01 and 2024-03-01` is that day (D-034).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use regex::bytes::{Regex, RegexBuilder};
use roaring::RoaringBitmap;
use serde::{Deserialize, Serialize};

use crate::columns::{Columns, Kind, field_bytes, parse_number};
use crate::scan::nth_field;
use crate::table::Table;
use crate::time::{self, Order};
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
}

impl Op {
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
    Num(f64),
}

/// A predicate compiled for one column.
pub struct Compiled {
    op: Op,
    ci: bool,
    /// Operand(s) lowercased when case-insensitive.
    needles: Vec<Vec<u8>>,
    /// User regex, or the case-insensitive matcher of a text operator.
    regex: Option<Regex>,
    /// For range operators: typed bounds (lo, hi) when the operand parses.
    range: Option<(Option<Bound>, Option<Bound>, bool, bool)>,
    /// Text fallback bounds for ranges.
    text_lo: Option<Vec<u8>>,
    text_hi: Option<Vec<u8>>,
    order: Option<Order>,
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
            Op::In(vs) | Op::NotIn(vs) if !vs.is_empty() => Some(format!(
                "^(?:{})$",
                vs.iter().map(|v| lit(v)).collect::<Vec<_>>().join("|")
            )),
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
                && let Some((t, span)) = time::parse_span(s.as_bytes(), o)
            {
                return Some(Bound::Time(if upper_incl {
                    t.saturating_add(span - 1)
                } else {
                    t
                }));
            }
            parse_number(s.as_bytes()).map(Bound::Num)
        };
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
            Op::In(v) => {
                !v.is_empty()
                    && if ci {
                        re(cell)
                    } else {
                        self.needles.iter().any(|n| cell == n.as_slice())
                    }
            }
            Op::NotIn(v) => {
                v.is_empty()
                    || !(if ci {
                        re(cell)
                    } else {
                        self.needles.iter().any(|n| cell == n.as_slice())
                    })
            }
            Op::Regex(_) => re(cell),
            Op::Gt(_) | Op::Ge(_) | Op::Lt(_) | Op::Le(_) | Op::Between(..) => self.in_range(cell),
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
                let Some(x) = parse_number(cell) else {
                    return false;
                };
                let ok_lo =
                    !matches!(lo, Some(Bound::Num(l)) if !cmp(x.total_cmp(&l), lo_incl, Greater));
                let ok_hi =
                    !matches!(hi, Some(Bound::Num(h)) if !cmp(x.total_cmp(&h), hi_incl, Less));
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
}

impl Engine {
    pub fn new(table: Arc<Table>) -> Self {
        Engine {
            table,
            cache: Mutex::new(HashMap::new()),
        }
    }

    pub fn table(&self) -> &Arc<Table> {
        &self.table
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
            Expr::Pred(p) => Compiled::new(&p.op, p.case_sensitive, Kind::Text).map(|_| ()),
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
        let key = serde_json::to_string(p).unwrap_or_default();
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
                let kind = cols.as_ref().map_or(Kind::Text, |ci| ci.cols[c].kind);
                let cp = Compiled::new(&p.op, p.case_sensitive, kind)?;
                if let Some(ci) = &cols {
                    if let Some(b) = self.via_dict(ci, c, &cp) {
                        return Ok(b);
                    }
                    if let Some(b) = self.via_time(ci, c, &cp) {
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
        let cols = self.table.column_index();
        let per_col: Vec<Compiled> = (0..self.table.columns.len())
            .map(|c| {
                let kind = cols.as_ref().map_or(Kind::Text, |ci| ci.cols[c].kind);
                Compiled::new(&p.op, p.case_sensitive, kind)
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
