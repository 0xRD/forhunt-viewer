//! Multi-column sort of a set of rows.
//!
//! Every key part becomes an order-preserving `u64`:
//! - indexed time column: ticks (sign bit flipped);
//! - dictionary column: rank of the value in the sorted dictionary;
//! - time column without index: ticks parsed during one scan;
//! - number column: IEEE total-order bits of the value (blank / unparsable
//!   as -inf, so they come first like blanks elsewhere);
//! - text: the first 8 bytes, ASCII-lowercased, big-endian.
//!
//! Descending parts store `!key`. Rows are sorted by (keys, row) in
//! parallel, so ties always keep file order and the result is deterministic.
//! Text prefixes can tie for different values; runs of equal keys that
//! contain a text value not fully represented by its prefix (longer than 8
//! bytes, uppercase, NUL or non-ASCII) are re-sorted on the full cells
//! (case-insensitive, then byte order: [`columns::text_cmp`]).
//!
//! Single-key sorts on an indexed time column or a dictionary column skip
//! all of this and walk the precomputed order.

use std::cmp::Ordering;

use rayon::prelude::*;
use roaring::RoaringBitmap;
use serde::{Deserialize, Serialize};

use crate::columns::{self, Columns, Kind, field_bytes, parse_number};
use crate::scan::nth_field;
use crate::table::Table;
use crate::time::{self, Order};
use crate::{Error, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SortKey {
    pub col: usize,
    pub desc: bool,
}

const BLOCK: u64 = 32_768;

#[derive(Clone, Copy, PartialEq)]
enum PartKind {
    Ticks,
    Rank,
    ParseTime(Order),
    Number,
    Text,
}

fn i64_key(v: i64) -> u64 {
    (v as u64) ^ (1 << 63)
}

fn f64_key(v: f64) -> u64 {
    let b = v.to_bits();
    if b >> 63 == 1 { !b } else { b | (1 << 63) }
}

/// (prefix key, fully represented by the prefix)
fn text_key(cell: &[u8]) -> (u64, bool) {
    let mut p = [0u8; 8];
    for (d, s) in p.iter_mut().zip(cell) {
        *d = s.to_ascii_lowercase();
    }
    let exact = cell.len() <= 8
        && cell
            .iter()
            .all(|&b| b != 0 && b.is_ascii() && !b.is_ascii_uppercase());
    (u64::from_be_bytes(p), exact)
}

/// Rows of `rows` in sorted order.
pub fn sort(table: &Table, rows: &RoaringBitmap, keys: &[SortKey]) -> Result<Vec<u32>> {
    if keys.is_empty() || rows.len() <= 1 {
        return Ok(rows.iter().collect());
    }
    let ncols = table.columns.len();
    if keys.iter().any(|k| k.col >= ncols) {
        return Err(Error::Other("sort column out of range".into()));
    }
    let cols = table.column_index();
    if keys.len() == 1
        && let Some(ci) = &cols
        && let Some(v) = fast_single(ci, rows, keys[0])
    {
        return Ok(v);
    }
    general(table, cols.as_deref(), rows, keys)
}

fn fast_single(ci: &Columns, rows: &RoaringBitmap, key: SortKey) -> Option<Vec<u32>> {
    let col = &ci.cols[key.col];
    if let Some(t) = &col.time {
        let asc: Vec<u32> = t.sorted_rows().filter(|r| rows.contains(*r)).collect();
        if !key.desc {
            return Some(asc);
        }
        // Descending times, ties still in row order: reverse the groups.
        let mut out = Vec::with_capacity(asc.len());
        let mut end = asc.len();
        while end > 0 {
            let tk = t.ticks[asc[end - 1] as usize];
            let mut start = end - 1;
            while start > 0 && t.ticks[asc[start - 1] as usize] == tk {
                start -= 1;
            }
            out.extend_from_slice(&asc[start..end]);
            end = start;
        }
        return Some(out);
    }
    if let Some(d) = &col.dict {
        let mut out = Vec::with_capacity(rows.len() as usize);
        let order: Box<dyn Iterator<Item = &RoaringBitmap>> = if key.desc {
            Box::new(d.bitmaps.iter().rev())
        } else {
            Box::new(d.bitmaps.iter())
        };
        for b in order {
            out.extend((b & rows).iter());
        }
        return Some(out);
    }
    None
}

fn general(
    table: &Table,
    cols: Option<&Columns>,
    rows: &RoaringBitmap,
    keys: &[SortKey],
) -> Result<Vec<u32>> {
    let n_all = table.row_count() as usize;
    let k = keys.len();
    let kinds: Vec<PartKind> = keys
        .iter()
        .map(|key| {
            let c = cols.map(|ci| &ci.cols[key.col]);
            match c {
                Some(c) if c.time.is_some() => PartKind::Ticks,
                Some(c) if c.dict.is_some() => PartKind::Rank,
                Some(c) => match c.kind {
                    Kind::Time(o) => PartKind::ParseTime(o),
                    Kind::Number => PartKind::Number,
                    Kind::Text => PartKind::Text,
                },
                None => PartKind::Text,
            }
        })
        .collect();
    // Dictionary ranks per row (only for rank parts).
    let ranks: Vec<Option<Vec<u32>>> = keys
        .iter()
        .zip(&kinds)
        .map(|(key, pk)| {
            (*pk == PartKind::Rank).then(|| {
                let d = cols.and_then(|ci| ci.cols[key.col].dict.as_ref());
                let mut r = vec![0u32; n_all];
                if let Some(d) = d {
                    for (i, b) in d.bitmaps.iter().enumerate() {
                        for row in b & rows {
                            r[row as usize] = i as u32;
                        }
                    }
                }
                r
            })
        })
        .collect();

    // Order of rows in `rows` (rank of each member) to place keys.
    let members: Vec<u32> = rows.iter().collect();
    let m = members.len();
    let mut flat = vec![0u64; m * k];
    // Rows whose text part is not exact, per part.
    let mut inexact = vec![RoaringBitmap::new(); k];

    // Parts from indexes.
    for (p, (key, pk)) in keys.iter().zip(&kinds).enumerate() {
        match pk {
            PartKind::Ticks => {
                let t = cols
                    .and_then(|ci| ci.cols[key.col].time.as_ref())
                    .ok_or(Error::Poisoned)?;
                for (i, &r) in members.iter().enumerate() {
                    flat[i * k + p] = i64_key(t.ticks[r as usize]);
                }
            }
            PartKind::Rank => {
                let rk = ranks[p].as_ref().ok_or(Error::Poisoned)?;
                for (i, &r) in members.iter().enumerate() {
                    flat[i * k + p] = rk[r as usize] as u64;
                }
            }
            _ => {}
        }
    }
    // Parts that need the file: one scan of the blocks holding members.
    let scanned: Vec<usize> = (0..k)
        .filter(|&p| {
            matches!(
                kinds[p],
                PartKind::ParseTime(_) | PartKind::Number | PartKind::Text
            )
        })
        .collect();
    if !scanned.is_empty() {
        let t = table.tables();
        let quote = table.dialect.quote;
        let blocks = table.par_blocks_of(rows, BLOCK);
        let mut pos = 0usize; // index into members
        table.par_block_list(
            &blocks,
            |b| {
                // (row, per scanned part: (key, exact))
                let mut out: Vec<(u32, Vec<(u64, bool)>)> = Vec::new();
                let mut tmp = Vec::new();
                let r0 = b.first as u32;
                let r1 = r0 + b.len() as u32;
                for r in rows.range(r0..r1) {
                    let row = b.row((r - r0) as usize);
                    let parts = scanned
                        .iter()
                        .map(|&p| {
                            let cell = match nth_field(t, row, keys[p].col) {
                                Some(s) => field_bytes(row, s, quote, &mut tmp),
                                None => &[][..],
                            };
                            match kinds[p] {
                                PartKind::ParseTime(o) => {
                                    (i64_key(time::parse(cell, o).unwrap_or(i64::MIN)), true)
                                }
                                PartKind::Number => (
                                    f64_key(parse_number(cell).unwrap_or(f64::NEG_INFINITY)),
                                    true,
                                ),
                                _ => text_key(cell),
                            }
                        })
                        .collect();
                    out.push((r, parts));
                }
                out
            },
            |_, out| {
                for (r, parts) in out {
                    // Members are visited in row order, like `members`.
                    debug_assert_eq!(members.get(pos), Some(&r));
                    for (j, &p) in scanned.iter().enumerate() {
                        flat[pos * k + p] = parts[j].0;
                        if !parts[j].1 {
                            inexact[p].insert(r);
                        }
                    }
                    pos += 1;
                }
                Ok(())
            },
        )?;
        if pos != m {
            return Err(Error::Other("sort: row set changed during scan".into()));
        }
    }
    // Descending parts.
    for (p, key) in keys.iter().enumerate() {
        if key.desc {
            for i in 0..m {
                flat[i * k + p] = !flat[i * k + p];
            }
        }
    }
    let mut idx: Vec<u32> = (0..m as u32).collect();
    let cmp_idx = |a: &u32, b: &u32| {
        let (a, b) = (*a as usize, *b as usize);
        flat[a * k..a * k + k]
            .cmp(&flat[b * k..b * k + k])
            .then(a.cmp(&b))
    };
    idx.par_sort_unstable_by(cmp_idx);

    // Resolve runs where a text prefix may hide the real order. A run is a
    // maximal group equal on the parts up to and including the first text
    // part; inside it every part is compared in full (text parts on the
    // whole cell), then by row.
    let text_parts: Vec<usize> = (0..k).filter(|&p| kinds[p] == PartKind::Text).collect();
    if let Some(&q) = text_parts.first() {
        let head = |i: u32| &flat[i as usize * k..i as usize * k + q + 1];
        let mut runs: Vec<(usize, usize)> = Vec::new();
        let mut s = 0;
        while s < m {
            let mut e = s + 1;
            while e < m && head(idx[e]) == head(idx[s]) {
                e += 1;
            }
            if e - s > 1
                && idx[s..e].iter().any(|&i| {
                    text_parts
                        .iter()
                        .any(|&p| inexact[p].contains(members[i as usize]))
                })
            {
                runs.push((s, e));
            }
            s = e;
        }
        let fixed: Vec<Result<Vec<u32>>> = runs
            .par_iter()
            .map(|&(s, e)| {
                let mut buf = Vec::new();
                let mut spans = Vec::new();
                let mut tmp = Vec::new();
                let mut full: Vec<(Vec<Vec<u8>>, u32)> = Vec::with_capacity(e - s);
                for &i in &idx[s..e] {
                    let raw = table.raw_row(members[i as usize] as u64, &mut buf)?;
                    crate::scan::split_row(table.tables(), raw, &mut spans);
                    let vals = text_parts
                        .iter()
                        .map(|&p| match spans.get(keys[p].col) {
                            Some(sp) => {
                                field_bytes(raw, *sp, table.dialect.quote, &mut tmp).to_vec()
                            }
                            None => Vec::new(),
                        })
                        .collect();
                    full.push((vals, i));
                }
                full.sort_by(|a, b| {
                    let (ia, ib) = (a.1 as usize, b.1 as usize);
                    for p in 0..k {
                        let o = match text_parts.iter().position(|&t| t == p) {
                            Some(j) => {
                                let o = columns::text_cmp(&a.0[j], &b.0[j]);
                                if keys[p].desc { o.reverse() } else { o }
                            }
                            None => flat[ia * k + p].cmp(&flat[ib * k + p]),
                        };
                        if o != Ordering::Equal {
                            return o;
                        }
                    }
                    ia.cmp(&ib)
                });
                Ok(full.into_iter().map(|f| f.1).collect())
            })
            .collect();
        for ((s, e), f) in runs.into_iter().zip(fixed) {
            idx[s..e].copy_from_slice(&f?);
        }
    }
    Ok(idx.into_iter().map(|i| members[i as usize]).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_preserve_order() {
        let xs = [-5.5f64, -0.0, 0.0, 1e-9, 3.0, f64::INFINITY];
        for w in xs.windows(2) {
            assert!(f64_key(w[0]) <= f64_key(w[1]), "{w:?}");
        }
        assert!(f64_key(f64::NEG_INFINITY) < f64_key(-1e300));
        assert!(i64_key(i64::MIN) < i64_key(-1));
        assert!(i64_key(-1) < i64_key(0));
        assert!(text_key(b"abc").0 < text_key(b"abd").0);
        assert!(text_key(b"").0 < text_key(b"a").0);
        assert_eq!(text_key(b"ABC").0, text_key(b"abc").0);
        assert!(!text_key(b"ABC").1 && text_key(b"abc").1);
        assert!(!text_key(b"abcdefghi").1);
    }
}
