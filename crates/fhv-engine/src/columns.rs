//! Column profiling and column indexes.
//!
//! A sample of rows decides, per column:
//! - **dictionary**: few distinct values (≤ `SAMPLE_DISTINCT_MAX` in the
//!   sample). The full pass then builds one roaring bitmap of rows per value
//!   (capped at `DICT_MAX` values, abandoned past it). Any predicate on such a
//!   column is evaluated once per distinct value, and the value list
//!   (with counts) of the filter dropdown is free.
//! - **time**: ≥ 90 % of non-blank sampled cells parse as date text, or
//!   the column holds epoch numbers whose values all fall in a plausible
//!   range for one unit (see [`epoch_shape`]). The primary time column
//!   (first one, or one with a well-known name) is parsed for every row
//!   into ticks, plus a permutation sorted by (time, row) unless the file
//!   is already in time order.
//! - **number**: ≥ 99 % of non-blank sampled cells parse as numbers;
//!   decides numeric comparisons and sort.
//!
//! The same pass counts fields per row; rows whose count differs from the
//! header are flagged as bad (ragged).

use rayon::prelude::*;
use roaring::RoaringBitmap;
use rustc_hash::FxHashMap;

use crate::scan::{Span, Tables, unquote};
use crate::table::{Block, Table};
use crate::time::{self, Fmt, Hint, Order, Shape};
use crate::{Error, Result};

pub const SAMPLE_ROWS: u64 = 20_000;
pub const SAMPLE_DISTINCT_MAX: usize = 512;
pub const DICT_MAX: usize = 65_536;
const BLOCK_ROWS: u64 = 32_768;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Text,
    Number,
    Time(Fmt),
}

/// Per-value row bitmaps of a low-cardinality column.
#[derive(Clone, Debug, Default)]
pub struct Dict {
    /// Distinct decoded values, sorted by the column's kind (times, numbers,
    /// else case-insensitively; unparsable values first), then by
    /// [`text_cmp`]. This is the sort order of the column.
    pub values: Vec<String>,
    pub bitmaps: Vec<RoaringBitmap>,
    pub numeric: bool,
}

impl Dict {
    pub fn count(&self, i: usize) -> u64 {
        self.bitmaps[i].len()
    }
}

/// Parsed times of every row of a column.
#[derive(Clone, Debug, Default)]
pub struct TimeIndex {
    /// Ticks per data row; `i64::MIN` when blank or unparsable.
    pub ticks: Vec<i64>,
    /// Data rows sorted by (ticks, row); `None` when that is the identity
    /// (file already in time order). Unparsable rows come first.
    pub perm: Option<Vec<u32>>,
}

impl TimeIndex {
    pub fn sorted_rows(&self) -> Box<dyn DoubleEndedIterator<Item = u32> + '_> {
        match &self.perm {
            Some(p) => Box::new(p.iter().copied()),
            None => Box::new(0..self.ticks.len() as u32),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Column {
    pub kind: Kind,
    /// ≥ 99 % of the sampled non-blank cells are numbers (the kind of the
    /// column when it is not read as dates).
    pub numeric: bool,
    pub dict: Option<Dict>,
    pub time: Option<TimeIndex>,
}

#[derive(Clone, Debug, Default)]
pub struct Columns {
    pub cols: Vec<Column>,
    /// Rows whose field count differs from the header.
    pub ragged: RoaringBitmap,
    /// Rows with text after a closing quote (`"ab"c`): their decoded cells
    /// are not substrings of the raw bytes, so prefiltered scans test them
    /// explicitly.
    pub odd_quotes: RoaringBitmap,
    /// Index of the primary time column, if any.
    pub primary_time: Option<usize>,
}

/// Decodes field `span` of `row` (quoted fields unquoted into `tmp`).
#[inline]
pub fn field_bytes<'a>(
    row: &'a [u8],
    span: Span,
    quote: Option<u8>,
    tmp: &'a mut Vec<u8>,
) -> &'a [u8] {
    let raw = &row[span.start as usize..span.end as usize];
    match (span.quoted, quote) {
        (true, Some(q)) => {
            unquote(raw, q, tmp);
            tmp
        }
        _ => raw,
    }
}

pub fn parse_number(b: &[u8]) -> Option<f64> {
    parse_num(b).map(Num::as_f64)
}

/// A parsed number: integers up to ±2^64 (decimal or `0x` hex, as Windows
/// writes Keywords such as 0x8020000000000000) stay exact; others are f64.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Num {
    Int(i128),
    Float(f64),
}

const INT_MAX: i128 = u64::MAX as i128;

impl Num {
    pub fn as_f64(self) -> f64 {
        match self {
            Num::Int(i) => i as f64,
            Num::Float(f) => f,
        }
    }

    /// Exact total order (integers against floats included).
    pub fn total_cmp(self, other: Num) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        fn int_float(a: i128, b: f64) -> Ordering {
            let fl = b.floor();
            if fl > INT_MAX as f64 {
                return Ordering::Less;
            }
            if fl < -(INT_MAX as f64) {
                return Ordering::Greater;
            }
            match a.cmp(&(fl as i128)) {
                Ordering::Equal if b > fl => Ordering::Less,
                o => o,
            }
        }
        match (self, other) {
            (Num::Int(a), Num::Int(b)) => a.cmp(&b),
            (Num::Float(a), Num::Float(b)) => a.total_cmp(&b),
            (Num::Int(a), Num::Float(b)) => int_float(a, b),
            (Num::Float(a), Num::Int(b)) => int_float(b, a).reverse(),
        }
    }

    /// Order-preserving two-word key: the f64 order, then the exact
    /// integer offset from that f64 (integers beyond 2^53 that share an
    /// f64 differ by < 2^12).
    pub fn sort_key(self) -> (u64, u64) {
        let f = self.as_f64();
        let b = f.to_bits();
        let hi = if b >> 63 == 1 { !b } else { b | (1 << 63) };
        let lo = match self {
            Num::Int(i) => (i - f as i128).clamp(-(1 << 62), 1 << 62) as i64,
            Num::Float(_) => 0,
        };
        (hi, (lo as u64) ^ (1 << 63))
    }

    /// Key for blank or unparsable cells: before every number.
    pub const MIN_KEY: (u64, u64) = (0, 0);
}

pub fn parse_num(b: &[u8]) -> Option<Num> {
    let s = std::str::from_utf8(b).ok()?.trim();
    if s.is_empty() || s.len() > 40 {
        return None;
    }
    // Hex as forhunt / Windows write it (0x17).
    if let Some(h) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        return u64::from_str_radix(h, 16).ok().map(|v| Num::Int(v as i128));
    }
    let digits = s.strip_prefix(['+', '-']).unwrap_or(s);
    if !digits.is_empty()
        && digits.bytes().all(|c| c.is_ascii_digit())
        && let Ok(i) = s.parse::<i128>()
        && (-INT_MAX..=INT_MAX).contains(&i)
    {
        return Some(Num::Int(i));
    }
    let v: f64 = s.parse().ok()?;
    v.is_finite().then_some(Num::Float(v))
}

fn is_blank(b: &[u8]) -> bool {
    b.iter().all(|c| c.is_ascii_whitespace())
}

/// Names that make a time column the primary one.
const TIME_NAMES: &[&str] = &[
    "timecreated",
    "timestamp",
    "datetime",
    "time",
    "date",
    "created0x10",
    "updatetimestamp",
    "sourcemodified",
    "lastmodified0x10",
];

struct Profile {
    distinct: rustc_hash::FxHashSet<Vec<u8>>,
    distinct_overflow: bool,
    nonblank: u64,
    times: u64,
    numbers: u64,
    order: Option<Order>,
    order_conflict: bool,
    /// Sampled cells per kind of date text.
    hints: Vec<(Hint, u64)>,
    /// Plain decimal numbers (no sign, hex or exponent), zero excluded,
    /// and per epoch candidate how many fall in the narrow / wide range.
    plain: u64,
    epoch: [(u64, u64); time::EPOCH_CANDIDATES.len()],
}

/// Decides column kinds and index candidates from a sample of rows.
fn profile(table: &Table) -> Result<Vec<Profile>> {
    let n = table.row_count();
    let ncols = table.columns.len();
    let quote = table.dialect.quote;
    let t = table.tables();
    let mut profs: Vec<Profile> = (0..ncols)
        .map(|_| Profile {
            distinct: Default::default(),
            distinct_overflow: false,
            nonblank: 0,
            times: 0,
            numbers: 0,
            order: None,
            order_conflict: false,
            hints: Vec::new(),
            plain: 0,
            epoch: Default::default(),
        })
        .collect();
    // Head of the file plus rows spread over it.
    let mut ranges: Vec<std::ops::Range<u64>> = Vec::new();
    ranges.push(0..n.min(SAMPLE_ROWS / 2));
    if n > SAMPLE_ROWS {
        let step = n / 64;
        for k in 1..64 {
            let s = k * step;
            ranges.push(s..(s + SAMPLE_ROWS / 128).min(n));
        }
    }
    let mut tmp = Vec::new();
    for r in ranges {
        table.par_blocks(
            r,
            BLOCK_ROWS,
            |b| collect_cells(t, b, ncols),
            |_, cells| {
                for (c, v) in cells.into_iter().enumerate() {
                    let p = &mut profs[c];
                    for cell in v {
                        sample_cell(p, &cell, quote, &mut tmp);
                    }
                }
                Ok(())
            },
        )?;
    }
    Ok(profs)
}

/// Raw cells per column of a block (used only on the small sample).
fn collect_cells(t: &Tables, b: &Block<'_>, ncols: usize) -> Vec<Vec<(Vec<u8>, bool)>> {
    let mut out: Vec<Vec<(Vec<u8>, bool)>> = vec![Vec::new(); ncols];
    let mut spans = Vec::new();
    for k in 0..b.len() {
        let row = b.row(k);
        crate::scan::split_row(t, row, &mut spans);
        for (c, o) in out.iter_mut().enumerate() {
            let cell = spans
                .get(c)
                .map(|s| (row[s.start as usize..s.end as usize].to_vec(), s.quoted))
                .unwrap_or_default();
            o.push(cell);
        }
    }
    out
}

fn sample_cell(p: &mut Profile, cell: &(Vec<u8>, bool), quote: Option<u8>, tmp: &mut Vec<u8>) {
    let v: &[u8] = match (cell.1, quote) {
        (true, Some(q)) => {
            unquote(&cell.0, q, tmp);
            tmp
        }
        _ => &cell.0,
    };
    if !p.distinct_overflow {
        if !p.distinct.contains(v) {
            p.distinct.insert(v.to_vec());
        }
        if p.distinct.len() > SAMPLE_DISTINCT_MAX {
            p.distinct_overflow = true;
            p.distinct.clear();
        }
    }
    if is_blank(v) {
        return;
    }
    p.nonblank += 1;
    if parse_number(v).is_some() {
        p.numbers += 1;
    }
    let hint = time::text_hint(v, Order::Mdy).or_else(|| {
        // Only year-last numeric dates depend on the order.
        v.first()
            .is_some_and(u8::is_ascii_digit)
            .then(|| time::text_hint(v, Order::Dmy))
            .flatten()
    });
    if let Some(h) = hint {
        p.times += 1;
        match p.hints.iter_mut().find(|x| x.0 == h) {
            Some(x) => x.1 += 1,
            None => p.hints.push((h, 1)),
        }
        match (time::order_hint(v), p.order) {
            (Some(o), None) => p.order = Some(o),
            (Some(o), Some(prev)) if o != prev => p.order_conflict = true,
            _ => {}
        }
    } else if v.len() <= 30
        && v.iter().all(|c| c.is_ascii_digit() || *c == b'.')
        && v.iter().any(|c| *c != b'0' && *c != b'.')
        && let Some(ts) = time::epoch_ticks(v)
    {
        p.plain += 1;
        for (e, t) in p.epoch.iter_mut().zip(ts) {
            e.0 += NARROW.contains(&t) as u64;
            e.1 += WIDE.contains(&t) as u64;
        }
    }
}

/// Format of a column of date text: day/month order from the sampled
/// values, the dominant kind of text, and for year-less dates the year
/// (and month) of the file's last modification (D-050).
fn text_fmt(p: &Profile, year: i32, roll: u8) -> Fmt {
    let order = if p.order_conflict {
        Order::Ymd
    } else {
        p.order.unwrap_or(Order::Ymd)
    };
    let top = p.hints.iter().max_by_key(|h| h.1).copied();
    let hint = match top {
        Some((h, n)) if n * 10 >= p.times * 9 => h,
        _ => Hint::Mixed,
    };
    let yearless = p.hints.iter().any(|h| h.0 == Hint::YearLess);
    Fmt {
        order,
        hint,
        year: if yearless { year } else { 0 },
        roll: if yearless { roll } else { 12 },
        ..Fmt::default()
    }
}

/// Year and month of the file's modification time (now if unknown).
fn file_year(table: &Table) -> (i32, u8) {
    let t = std::fs::metadata(&table.path)
        .and_then(|m| m.modified())
        .unwrap_or_else(|_| std::time::SystemTime::now());
    let secs = t
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    let (y, m, _) = time::civil_from_days(secs.div_euclid(86_400));
    (y as i32, m as u8)
}

/// Plausible instants of epoch numbers: 1990..2040 for detection by value
/// range alone, 1971..2100 when the header names a time.
const NARROW: std::ops::Range<i64> =
    631_152_000 * time::TICKS_PER_SEC..2_208_988_800 * time::TICKS_PER_SEC;
const WIDE: std::ops::Range<i64> =
    31_536_000 * time::TICKS_PER_SEC..4_102_444_800 * time::TICKS_PER_SEC;

/// Header words that make a numeric column a time column.
fn time_name(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    [
        "time", "date", "stamp", "created", "modified", "accessed", "changed", "written",
        "updated", "epoch", "when", "visit", "expir", "birth",
    ]
    .iter()
    .any(|w| n.contains(w))
        || n == "ts"
        || n.ends_with("_ts")
        || n.starts_with("ts_")
        || n.ends_with("_at")
}

/// Epoch unit of a numeric column, if its sample makes it unambiguous
/// (D-050): every plain non-zero sampled value is in 1990..2040 for one
/// unit; or the header names a time and ≥ 99 % are in 1971..2100. Excel,
/// Cocoa and day counts overlap ordinary numbers and need their own word in
/// the header. Units are tried from the narrowest range of magnitudes up.
fn epoch_shape(p: &Profile, name: &str) -> Option<Shape> {
    if p.plain == 0 || p.numbers * 100 < p.nonblank * 99 || p.plain * 100 < p.numbers * 99 {
        return None;
    }
    let lname = name.to_ascii_lowercase();
    let timeish = time_name(name);
    let cands = || time::EPOCH_CANDIDATES.iter().zip(&p.epoch);
    let named = |words: &[&str]| words.iter().any(|w| lname.contains(w));
    let wide_ok = |wide: u64| wide * 100 >= p.plain * 99;
    cands()
        .find(|((_, words), (narrow, wide))| {
            if words.is_empty() {
                *narrow == p.plain
            } else {
                named(words) && wide_ok(*wide)
            }
        })
        .or_else(|| {
            cands().find(|((_, words), (_, wide))| timeish && words.is_empty() && wide_ok(*wide))
        })
        .map(|((s, _), _)| *s)
}

type ValueBitmaps = Vec<(Vec<u8>, RoaringBitmap)>;
/// Local values of a block and the local code of each row.
type LocalCodes = (Vec<Vec<u8>>, Vec<u16>);

/// Per-block results of the full pass.
struct BlockOut {
    ragged: Vec<u32>,
    odd: Vec<u32>,
    /// Per dictionary column: local values and their row bitmaps.
    dicts: Vec<Option<ValueBitmaps>>,
    ticks: Vec<i64>,
}

/// Builds the column indexes of a fully indexed table.
pub fn build(table: &Table) -> Result<Columns> {
    let n = table.row_count();
    if n > u32::MAX as u64 {
        return Err(Error::Other("more than 4 billion rows".into()));
    }
    let ncols = table.columns.len();
    let quote = table.dialect.quote;
    let t = table.tables();
    let profs = profile(table)?;
    let (year, roll) = file_year(table);
    let numeric: Vec<bool> = profs
        .iter()
        .map(|p| p.nonblank > 0 && p.numbers * 100 >= p.nonblank * 99)
        .collect();
    let kinds: Vec<Kind> = profs
        .iter()
        .enumerate()
        .map(|(c, p)| {
            if p.nonblank > 0 && p.times * 10 >= p.nonblank * 9 {
                Kind::Time(text_fmt(p, year, roll))
            } else if let Some(s) = epoch_shape(p, &table.columns[c]) {
                Kind::Time(Fmt::shape(s))
            } else if numeric[c] {
                Kind::Number
            } else {
                Kind::Text
            }
        })
        .collect();
    let dict_cols: Vec<bool> = profs
        .iter()
        .map(|p| !p.distinct_overflow && n > 0)
        .collect();
    let time_cols: Vec<usize> = (0..ncols)
        .filter(|&c| matches!(kinds[c], Kind::Time(_)))
        .collect();
    let primary_time = time_cols
        .iter()
        .copied()
        .find(|&c| TIME_NAMES.contains(&table.columns[c].to_ascii_lowercase().as_str()))
        .or_else(|| time_cols.first().copied());
    let time_order = primary_time.map(|c| match kinds[c] {
        Kind::Time(f) => f,
        _ => Fmt::default(),
    });

    // Global dictionaries.
    let mut gmaps: Vec<Option<FxHashMap<Vec<u8>, u32>>> = dict_cols
        .iter()
        .map(|&d| d.then(FxHashMap::default))
        .collect();
    let mut gvalues: Vec<Vec<Vec<u8>>> = vec![Vec::new(); ncols];
    let mut gbitmaps: Vec<Vec<RoaringBitmap>> = vec![Vec::new(); ncols];
    let mut ragged = RoaringBitmap::new();
    let mut odd_quotes = RoaringBitmap::new();
    let mut ticks: Vec<i64> = Vec::with_capacity(if primary_time.is_some() {
        n as usize
    } else {
        0
    });
    // Abandoned dictionary columns (too many values), shared with workers.
    let abandoned: Vec<std::sync::atomic::AtomicBool> =
        (0..ncols).map(|c| (!dict_cols[c]).into()).collect();
    table.progress.set_phase(crate::Phase::Columns, n);
    table.par_blocks(
        0..n,
        BLOCK_ROWS,
        |b| {
            let mut out = BlockOut {
                ragged: Vec::new(),
                odd: Vec::new(),
                dicts: Vec::new(),
                ticks: Vec::with_capacity(if primary_time.is_some() { b.len() } else { 0 }),
            };
            // Per dictionary column: local values and per-row local codes.
            let mut local: Vec<Option<LocalCodes>> = (0..ncols)
                .map(|c| {
                    (!abandoned[c].load(std::sync::atomic::Ordering::Relaxed))
                        .then(|| (Vec::new(), Vec::with_capacity(b.len())))
                })
                .collect();
            let mut borrowed: Vec<FxHashMap<&[u8], u16>> = vec![FxHashMap::default(); ncols];
            let mut owned: Vec<FxHashMap<Vec<u8>, u16>> = vec![FxHashMap::default(); ncols];
            let mut tmp = Vec::new();
            let mut spans: Vec<Span> = Vec::with_capacity(ncols);
            for k in 0..b.len() {
                let row = b.row(k);
                let (nf, odd) = crate::scan::split_row_odd(t, row, &mut spans);
                if nf != ncols {
                    out.ragged.push((b.first + k as u64) as u32);
                }
                if odd {
                    out.odd.push((b.first + k as u64) as u32);
                }
                for c in 0..ncols {
                    let Some((vals, codes)) = &mut local[c] else {
                        continue;
                    };
                    let next = vals.len();
                    let code = match spans.get(c) {
                        Some(s) if s.quoted => {
                            let cell = field_bytes(row, *s, quote, &mut tmp);
                            match owned[c].get(cell) {
                                Some(&code) => code,
                                None => {
                                    owned[c].insert(cell.to_vec(), next as u16);
                                    vals.push(cell.to_vec());
                                    next as u16
                                }
                            }
                        }
                        span => {
                            let cell: &[u8] =
                                span.map_or(&[][..], |s| &row[s.start as usize..s.end as usize]);
                            *borrowed[c].entry(cell).or_insert_with(|| {
                                vals.push(cell.to_vec());
                                next as u16
                            })
                        }
                    };
                    // Owned and borrowed maps can hold the same value twice
                    // (quoted and unquoted spellings); the merge dedupes.
                    if vals.len() > u16::MAX as usize {
                        local[c] = None;
                        abandoned[c].store(true, std::sync::atomic::Ordering::Relaxed);
                        continue;
                    }
                    codes.push(code);
                }
                if let (Some(tc), Some(order)) = (primary_time, time_order) {
                    let v = match spans.get(tc) {
                        Some(s) => time::parse(field_bytes(row, *s, quote, &mut tmp), order),
                        None => None,
                    };
                    out.ticks.push(v.unwrap_or(i64::MIN));
                }
            }
            // Row bitmaps per local value, built here in parallel (sorted
            // appends) rather than in the sequential merge.
            out.dicts = local
                .into_iter()
                .map(|d| {
                    d.map(|(vals, codes)| {
                        let mut rows: Vec<Vec<u32>> = vec![Vec::new(); vals.len()];
                        for (k, code) in codes.iter().enumerate() {
                            rows[*code as usize].push((b.first + k as u64) as u32);
                        }
                        vals.into_iter()
                            .zip(rows)
                            .map(|(v, r)| {
                                (v, RoaringBitmap::from_sorted_iter(r).unwrap_or_default())
                            })
                            .collect()
                    })
                })
                .collect();
            out
        },
        |_first, out| {
            for r in out.ragged {
                let _ = ragged.try_push(r);
            }
            for r in out.odd {
                let _ = odd_quotes.try_push(r);
            }
            ticks.extend_from_slice(&out.ticks);
            for (c, d) in out.dicts.into_iter().enumerate() {
                let Some(gmap) = &mut gmaps[c] else { continue };
                let Some(vals) = d else {
                    gmaps[c] = None;
                    gvalues[c].clear();
                    gbitmaps[c].clear();
                    continue;
                };
                for (v, bm) in vals {
                    let next = gvalues[c].len();
                    let id = *gmap.entry(v).or_insert_with_key(|k| {
                        gvalues[c].push(k.clone());
                        gbitmaps[c].push(RoaringBitmap::new());
                        next as u32
                    });
                    gbitmaps[c][id as usize] |= bm;
                }
                if gvalues[c].len() > DICT_MAX {
                    gmaps[c] = None;
                    gvalues[c].clear();
                    gbitmaps[c].clear();
                    abandoned[c].store(true, std::sync::atomic::Ordering::Relaxed);
                }
            }
            table
                .progress
                .bytes
                .fetch_add(BLOCK_ROWS, std::sync::atomic::Ordering::Relaxed);
            Ok(())
        },
    )?;

    let mut cols: Vec<Column> = kinds
        .iter()
        .zip(&numeric)
        .map(|(&kind, &numeric)| Column {
            kind,
            numeric,
            dict: None,
            time: None,
        })
        .collect();
    for c in 0..ncols {
        if gmaps[c].is_none() {
            continue;
        }
        let values = std::mem::take(&mut gvalues[c]);
        let bitmaps = std::mem::take(&mut gbitmaps[c]);
        cols[c].dict = Some(sorted_dict(values, bitmaps, kinds[c]));
    }
    if let Some(tc) = primary_time {
        cols[tc].time = Some(time_index(ticks));
    }
    Ok(Columns {
        cols,
        ragged,
        odd_quotes,
        primary_time,
    })
}

/// Total order used for values and text sort: case-insensitive (ASCII
/// folding), then raw bytes so different spellings never tie.
pub fn text_cmp(a: &[u8], b: &[u8]) -> std::cmp::Ordering {
    let la = a.iter().map(u8::to_ascii_lowercase);
    let lb = b.iter().map(u8::to_ascii_lowercase);
    la.cmp(lb).then_with(|| a.cmp(b))
}

/// Dictionary in the sort order of a column of `kind` (the order the
/// general sort uses for such a column: parsed times / numbers with
/// unparsable cells first, else text), ties by [`text_cmp`].
fn sorted_dict(values: Vec<Vec<u8>>, bitmaps: Vec<RoaringBitmap>, kind: Kind) -> Dict {
    let nums: Vec<Option<f64>> = values.iter().map(|v| parse_number(v)).collect();
    let numeric = nums
        .iter()
        .zip(&values)
        .all(|(n, v)| n.is_some() || is_blank(v));
    let mut order: Vec<usize> = (0..values.len()).collect();
    match kind {
        Kind::Time(o) => {
            let ticks: Vec<i64> = values
                .iter()
                .map(|v| time::parse(v, o).unwrap_or(i64::MIN))
                .collect();
            order.sort_by(|&a, &b| {
                ticks[a]
                    .cmp(&ticks[b])
                    .then_with(|| text_cmp(&values[a], &values[b]))
            });
        }
        Kind::Number => {
            let keys: Vec<(u64, u64)> = values
                .iter()
                .map(|v| parse_num(v).map_or(Num::MIN_KEY, Num::sort_key))
                .collect();
            order.sort_by(|&a, &b| {
                keys[a]
                    .cmp(&keys[b])
                    .then_with(|| text_cmp(&values[a], &values[b]))
            })
        }
        Kind::Text => order.sort_by(|&a, &b| text_cmp(&values[a], &values[b])),
    }
    let mut bitmaps: Vec<Option<RoaringBitmap>> = bitmaps.into_iter().map(Some).collect();
    Dict {
        values: order
            .iter()
            .map(|&i| String::from_utf8_lossy(&values[i]).into_owned())
            .collect(),
        bitmaps: order
            .iter()
            .map(|&i| bitmaps[i].take().unwrap_or_default())
            .collect(),
        numeric,
    }
}

fn time_index(ticks: Vec<i64>) -> TimeIndex {
    let sorted = ticks.windows(2).all(|w| w[0] <= w[1]);
    let perm = (!sorted).then(|| {
        let mut p: Vec<u32> = (0..ticks.len() as u32).collect();
        p.par_sort_unstable_by_key(|&r| (ticks[r as usize], r));
        p
    });
    TimeIndex { ticks, perm }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers() {
        assert_eq!(parse_number(b" 42 "), Some(42.0));
        assert_eq!(parse_number(b"0x17"), Some(23.0));
        assert_eq!(parse_number(b"-1.5e3"), Some(-1500.0));
        assert_eq!(parse_number(b"nan"), None);
        assert_eq!(parse_number(b""), None);
        assert_eq!(parse_number(b"12abc"), None);
        let big = |s: &str| parse_num(s.as_bytes()).unwrap();
        assert_eq!(big("0x8000000000000001"), Num::Int(0x8000_0000_0000_0001));
        assert_eq!(big("-12"), Num::Int(-12));
        assert_eq!(big("1.5"), Num::Float(1.5));
        use std::cmp::Ordering::*;
        assert_eq!(
            big("0x8000000000000001").total_cmp(big("9223372036854775808")),
            Greater
        );
        assert_eq!(
            big("9223372036854775808").total_cmp(big("9.223372036854775808e18")),
            Equal
        );
        assert_eq!(big("3").total_cmp(big("2.5")), Greater);
        assert_eq!(big("2").total_cmp(big("2.5")), Less);
        assert_eq!(big("-3").total_cmp(big("-2.5")), Less);
        let ks: Vec<(u64, u64)> = [
            "-1e300",
            "-5",
            "-0.5",
            "0",
            "0x8000000000000000",
            "0x8000000000000001",
            "1e300",
        ]
        .iter()
        .map(|s| big(s).sort_key())
        .collect();
        assert!(ks.windows(2).all(|w| w[0] < w[1]), "{ks:?}");
    }

    #[test]
    fn dict_sorting() {
        let d = sorted_dict(
            vec![b"10".to_vec(), b"9".to_vec(), b"".to_vec()],
            vec![
                RoaringBitmap::new(),
                RoaringBitmap::new(),
                RoaringBitmap::new(),
            ],
            Kind::Number,
        );
        assert!(d.numeric);
        assert_eq!(d.values, ["", "9", "10"]);
        let d = sorted_dict(
            vec![b"b".to_vec(), b"B".to_vec(), b"a".to_vec()],
            vec![
                RoaringBitmap::new(),
                RoaringBitmap::new(),
                RoaringBitmap::new(),
            ],
            Kind::Text,
        );
        assert_eq!(d.values, ["a", "B", "b"]);
    }
}
