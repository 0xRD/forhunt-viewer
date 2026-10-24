//! Multi-column sort of a set of rows.
//!
//! Every key part becomes an order-preserving `u64`:
//! - indexed time column: ticks (sign bit flipped);
//! - dictionary column: rank of the value in the sorted dictionary (the
//!   column kind's order; equal times / numbers share a rank);
//! - time column without index: ticks parsed during one scan;
//! - number column: two words, the IEEE total-order bits of the value and
//!   the exact integer offset from it (hex Keywords above 2^53 stay
//!   distinct); blank / unparsable first, like blanks elsewhere;
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

use crate::columns::{self, Columns, Kind, Num, field_bytes, parse_num};
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

/// Key slots of a part: text windows, two words for exact numbers
/// (see [`Num::sort_key`]), else one.
fn width(pk: PartKind) -> usize {
    match pk {
        PartKind::Text => TW,
        PartKind::Number => 2,
        _ => 1,
    }
}

fn i64_key(v: i64) -> u64 {
    (v as u64) ^ (1 << 63)
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
        let all = rows.len() == t.ticks.len() as u64;
        let asc: Vec<u32> = match (&t.perm, all) {
            // File already in time order: the row order is the time order.
            (None, _) => rows.iter().collect(),
            (Some(p), true) => p.clone(),
            (Some(p), false) => p
                .par_iter()
                .copied()
                .filter(|r| rows.contains(*r))
                .collect(),
        };
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
        // One group of equal sort keys at a time (rows in file order).
        let rank = tie_ranks(d, col.kind);
        let mut groups: Vec<std::ops::Range<usize>> = Vec::new();
        let mut s = 0;
        while s < rank.len() {
            let e = s + rank[s..].iter().take_while(|&&r| r == rank[s]).count();
            groups.push(s..e);
            s = e;
        }
        if key.desc {
            groups.reverse();
        }
        for g in groups {
            let mut b = RoaringBitmap::new();
            for i in g {
                b |= &d.bitmaps[i] & rows;
            }
            out.extend(b.iter());
        }
        return Some(out);
    }
    None
}

/// Sort rank of each dictionary value: values whose sort keys are equal
/// (`0x10` and `16`, two spellings of one time) share a rank, so they tie
/// and keep file order like on the other sort paths. The dictionary is
/// already in the column's sort order.
fn tie_ranks(d: &columns::Dict, kind: Kind) -> Vec<u32> {
    let key = |v: &str| -> Option<(u64, u64)> {
        match kind {
            Kind::Time(o) => Some((i64_key(time::parse(v.as_bytes(), o).unwrap_or(i64::MIN)), 0)),
            Kind::Number => Some(parse_num(v.as_bytes()).map_or(Num::MIN_KEY, Num::sort_key)),
            Kind::Text => None,
        }
    };
    let mut out = Vec::with_capacity(d.values.len());
    let mut prev: Option<(u64, u64)> = None;
    for (i, v) in d.values.iter().enumerate() {
        let k = key(v);
        let r = match (k, prev, out.last()) {
            (Some(k), Some(p), Some(&last)) if k == p => last,
            _ => i as u32,
        };
        out.push(r);
        prev = k;
    }
    out
}

/// Runs up to this size are sorted by direct full comparison.
const SMALL_RUN: usize = 16;

/// Inline bytes of a text key (3 x u64).
const TW: usize = 3;
const TBYTES: usize = TW * 8;

/// First `TBYTES` bytes of `cell[from..]`, ASCII-lowercased, as big-endian
/// words (zero padded).
fn text_window(cell: &[u8], from: usize, out: &mut [u64]) {
    let mut buf = [0u8; TBYTES];
    if from < cell.len() {
        for (d, s) in buf.iter_mut().zip(&cell[from..]) {
            *d = s.to_ascii_lowercase();
        }
    }
    for (w, c) in out.iter_mut().zip(buf.chunks_exact(8)) {
        *w = u64::from_be_bytes(c.try_into().unwrap_or([0; 8]));
    }
}

/// The cell fully determines its order from the window (no truncation,
/// no case or NUL ambiguity).
fn window_exact(cell: &[u8]) -> bool {
    cell.len() <= TBYTES
        && cell
            .iter()
            .all(|&b| b != 0 && b.is_ascii() && !b.is_ascii_uppercase())
}

fn general(
    table: &Table,
    cols: Option<&Columns>,
    rows: &RoaringBitmap,
    keys: &[SortKey],
) -> Result<Vec<u32>> {
    let n_all = table.row_count() as usize;
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
    // Slots of each part in the flat key array.
    let mut off = Vec::with_capacity(keys.len());
    let mut k = 0usize;
    for pk in &kinds {
        off.push(k);
        k += width(*pk);
    }
    let trace = std::env::var_os("FHV_TRACE").is_some();
    let t0 = std::time::Instant::now();
    let lap = |what: &str| {
        if trace {
            eprintln!("sort: {what} at {:.3}s", t0.elapsed().as_secs_f64());
        }
    };
    let members: Vec<u32> = rows.iter().collect();
    let m = members.len();
    let mut flat = vec![0u64; m * k];
    // Members (indexes into `members`) whose text part is not exact.
    let mut inexact = vec![RoaringBitmap::new(); keys.len()];

    for (p, (key, pk)) in keys.iter().zip(&kinds).enumerate() {
        match pk {
            PartKind::Ticks => {
                let t = cols
                    .and_then(|ci| ci.cols[key.col].time.as_ref())
                    .ok_or(Error::Poisoned)?;
                flat.par_chunks_mut(k)
                    .zip(members.par_iter())
                    .for_each(|(f, &r)| {
                        f[off[p]] = i64_key(t.ticks[r as usize]);
                    });
            }
            PartKind::Rank => {
                let d = cols
                    .and_then(|ci| ci.cols[key.col].dict.as_ref())
                    .ok_or(Error::Poisoned)?;
                let ranks = tie_ranks(d, cols.map_or(Kind::Text, |ci| ci.cols[key.col].kind));
                let mut rk = vec![0u32; n_all];
                for (i, b) in d.bitmaps.iter().enumerate() {
                    for row in b & rows {
                        rk[row as usize] = ranks[i];
                    }
                }
                flat.par_chunks_mut(k)
                    .zip(members.par_iter())
                    .for_each(|(f, &r)| {
                        f[off[p]] = rk[r as usize] as u64;
                    });
            }
            _ => {}
        }
    }
    // Parts that need the file: one scan of the blocks holding members.
    let scanned: Vec<usize> = (0..keys.len())
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
        let mut pos = 0usize;
        table.par_block_list(
            &blocks,
            |b| {
                // Per member of the block: k key slots and inexact flags.
                let mut slots: Vec<u64> = Vec::new();
                let mut flags: Vec<u8> = Vec::new();
                let mut tmp = Vec::new();
                let r0 = b.first as u32;
                let r1 = r0 + b.len() as u32;
                for r in rows.range(r0..r1) {
                    let row = b.row((r - r0) as usize);
                    let base = slots.len();
                    slots.resize(base + k, 0);
                    let sl = &mut slots[base..];
                    let mut fl = 0u8;
                    for (j, &p) in scanned.iter().enumerate() {
                        let cell = match nth_field(t, row, keys[p].col) {
                            Some(s) => field_bytes(row, s, quote, &mut tmp),
                            None => &[][..],
                        };
                        match kinds[p] {
                            PartKind::ParseTime(o) => {
                                sl[off[p]] = i64_key(time::parse(cell, o).unwrap_or(i64::MIN))
                            }
                            PartKind::Number => {
                                let (hi, lo) = parse_num(cell).map_or(Num::MIN_KEY, Num::sort_key);
                                sl[off[p]] = hi;
                                sl[off[p] + 1] = lo;
                            }
                            _ => {
                                text_window(cell, 0, &mut sl[off[p]..off[p] + TW]);
                                if !window_exact(cell) {
                                    fl |= 1 << j;
                                }
                            }
                        }
                    }
                    flags.push(fl);
                }
                (slots, flags)
            },
            |_, (slots, flags)| {
                for (i, fl) in flags.into_iter().enumerate() {
                    let dst = &mut flat[(pos + i) * k..(pos + i + 1) * k];
                    for &p in &scanned {
                        let w = width(kinds[p]);
                        dst[off[p]..off[p] + w]
                            .copy_from_slice(&slots[i * k + off[p]..i * k + off[p] + w]);
                    }
                    for (j, &p) in scanned.iter().enumerate() {
                        if fl & (1 << j) != 0 {
                            inexact[p].insert((pos + i) as u32);
                        }
                    }
                }
                pos += slots.len() / k;
                Ok(())
            },
        )?;
        if pos != m {
            return Err(Error::Other("sort: row set changed during scan".into()));
        }
    }
    lap("keys extracted");
    // Descending parts.
    for (p, key) in keys.iter().enumerate() {
        if key.desc {
            let w = width(kinds[p]);
            flat.par_chunks_mut(k).for_each(|f| {
                for x in &mut f[off[p]..off[p] + w] {
                    *x = !*x;
                }
            });
        }
    }
    let mut idx = msd_sort(&flat, k, m);
    lap("sorted");
    // Runs where an inline text window may hide the real order: maximal
    // groups equal on the slots up to and including the first text part,
    // with an inexact text value in them.
    let text_parts: Vec<usize> = (0..keys.len())
        .filter(|&p| kinds[p] == PartKind::Text)
        .collect();
    if let Some(&q) = text_parts.first() {
        let head = off[q] + TW;
        let any_inexact = |i: u32| text_parts.iter().any(|&p| inexact[p].contains(i));
        let mut runs: Vec<(usize, usize)> = Vec::new();
        let mut s = 0;
        while s < m {
            let hs = idx[s] as usize * k;
            let mut e = s + 1;
            while e < m
                && flat[idx[e] as usize * k..idx[e] as usize * k + head] == flat[hs..hs + head]
            {
                e += 1;
            }
            if e - s > 1 && idx[s..e].iter().any(|&i| any_inexact(i)) {
                runs.push((s, e));
            }
            s = e;
        }
        lap(&format!(
            "{} runs, {} rows",
            runs.len(),
            runs.iter().map(|r| r.1 - r.0).sum::<usize>()
        ));
        if !runs.is_empty() {
            let map = table.map_data()?;
            let ctx = Resolve {
                table,
                map: &map,
                keys,
                kinds: &kinds,
                off: &off,
                k,
                flat: &flat,
                members: &members,
                text_parts: &text_parts,
            };
            let fixed: Vec<Result<Vec<u32>>> = runs
                .par_iter()
                .map(|&(s, e)| ctx.run(idx[s..e].to_vec()))
                .collect();
            for ((s, e), f) in runs.into_iter().zip(fixed) {
                idx[s..e].copy_from_slice(&f?);
            }
        }
    }
    lap("resolved");
    Ok(idx.into_iter().map(|i| members[i as usize]).collect())
}

/// Sorts member indexes by their `k` key slots (then index): sorts
/// (slot, index) pairs one slot at a time, recursing into runs of equal
/// slots, which keeps the hot loop on contiguous 12-byte pairs.
fn msd_sort(flat: &[u64], k: usize, m: usize) -> Vec<u32> {
    let mut pairs: Vec<(u64, u32)> = (0..m as u32).map(|i| (flat[i as usize * k], i)).collect();
    pairs.par_sort_unstable();
    let mut idx: Vec<u32> = pairs.into_iter().map(|p| p.1).collect();
    if k == 1 {
        return idx;
    }
    // Runs of equal first slot, refined on the next slots.
    let mut runs: Vec<(usize, usize, usize)> = Vec::new(); // (start, end, slot)
    let mut s = 0;
    while s < m {
        let v = flat[idx[s] as usize * k];
        let mut e = s + 1;
        while e < m && flat[idx[e] as usize * k] == v {
            e += 1;
        }
        if e - s > 1 {
            runs.push((s, e, 1));
        }
        s = e;
    }
    let sorted: Vec<(usize, Vec<u32>)> = runs
        .into_par_iter()
        .map(|(s, e, slot)| {
            let mut part = idx[s..e].to_vec();
            refine(flat, k, &mut part, slot);
            (s, part)
        })
        .collect();
    for (s, part) in sorted {
        idx[s..s + part.len()].copy_from_slice(&part);
    }
    idx
}

/// Sorts `part` (equal on slots before `slot`) by slots `slot..k`, then index.
fn refine(flat: &[u64], k: usize, part: &mut [u32], slot: usize) {
    if slot >= k || part.len() < 2 {
        part.sort_unstable();
        return;
    }
    let mut pairs: Vec<(u64, u32)> = part
        .iter()
        .map(|&i| (flat[i as usize * k + slot], i))
        .collect();
    if pairs.len() > 1 << 16 {
        pairs.par_sort_unstable();
    } else {
        pairs.sort_unstable();
    }
    let mut s = 0;
    while s < pairs.len() {
        let mut e = s + 1;
        while e < pairs.len() && pairs[e].0 == pairs[s].0 {
            e += 1;
        }
        if e - s > 1 && slot + 1 < k {
            let mut sub: Vec<u32> = pairs[s..e].iter().map(|p| p.1).collect();
            refine(flat, k, &mut sub, slot + 1);
            for (p, v) in pairs[s..e].iter_mut().zip(sub) {
                p.1 = v;
            }
        }
        s = e;
    }
    for (x, p) in part.iter_mut().zip(pairs) {
        *x = p.1;
    }
}

/// Orders runs of members that tie on inline keys, reading cells from a
/// transient memory map.
struct Resolve<'a> {
    table: &'a Table,
    map: &'a [u8],
    keys: &'a [SortKey],
    kinds: &'a [PartKind],
    off: &'a [usize],
    k: usize,
    flat: &'a [u64],
    members: &'a [u32],
    text_parts: &'a [usize],
}

impl Resolve<'_> {
    /// Decoded cell of column `col` for member `i`, borrowed from the map
    /// unless it holds escaped quotes.
    fn cell(&self, ix: &crate::index::RowIndex, i: u32, col: usize) -> std::borrow::Cow<'_, [u8]> {
        use std::borrow::Cow;
        let h = self.table.header_rows();
        let r = ix.range(self.members[i as usize] as u64 + h);
        let map: &[u8] = self.map;
        let raw = crate::scan::trim_eol(&map[r.start as usize..(r.end as usize).min(map.len())]);
        let Some(s) = nth_field(self.table.tables(), raw, col) else {
            return Cow::Borrowed(&[]);
        };
        let bytes = &raw[s.start as usize..s.end as usize];
        match self.table.dialect.quote {
            Some(q) if s.quoted => {
                // `"text"` with no inner quote: just strip the quotes.
                let inner = &bytes[1..];
                match memchr::memchr(q, inner) {
                    Some(p) if p + 1 == inner.len() => Cow::Borrowed(&inner[..p]),
                    _ => {
                        let mut tmp = Vec::new();
                        crate::scan::unquote(bytes, q, &mut tmp);
                        Cow::Owned(tmp)
                    }
                }
            }
            _ => Cow::Borrowed(bytes),
        }
    }

    fn run(&self, run: Vec<u32>) -> Result<Vec<u32>> {
        let q = self.text_parts[0];
        let col = self.keys[q].col;
        let desc = self.keys[q].desc;
        // Cells of the first text part, extracted once.
        // One read lock for the whole run (a lock per cell contends).
        let ix = self.table.index();
        let cells: Vec<std::borrow::Cow<'_, [u8]>> =
            run.par_iter().map(|&i| self.cell(&ix, i, col)).collect();
        // `pos` orders positions in `run`; `groups` are ranges of `pos`
        // still to compare in full.
        let mut pos: Vec<usize> = (0..run.len()).collect();
        let mut groups: Vec<std::ops::Range<usize>> = Vec::new();
        if run.len() > SMALL_RUN {
            // Split by successive windows of the first text part until
            // groups are small or equal case-insensitively.
            let mut work = vec![(0usize, run.len(), TBYTES)];
            while let Some((s, e, level)) = work.pop() {
                let lenclass = |c: &[u8]| c.len().min(level + TBYTES + 1);
                let mut keyed: Vec<([u64; TW], usize, usize)> = pos[s..e]
                    .iter()
                    .map(|&j| {
                        let mut w = [0u64; TW];
                        text_window(&cells[j], level, &mut w);
                        (w, lenclass(cells[j].as_ref()), j)
                    })
                    .collect();
                let cmp = |a: &([u64; TW], usize, usize), b: &([u64; TW], usize, usize)| {
                    let o = (a.0, a.1).cmp(&(b.0, b.1));
                    (if desc { o.reverse() } else { o }).then(run[a.2].cmp(&run[b.2]))
                };
                if keyed.len() > 1 << 14 {
                    keyed.par_sort_unstable_by(cmp);
                } else {
                    keyed.sort_unstable_by(cmp);
                }
                for (x, kk) in pos[s..e].iter_mut().zip(&keyed) {
                    *x = kk.2;
                }
                let mut a = 0;
                while a < keyed.len() {
                    let mut b = a + 1;
                    while b < keyed.len() && (keyed[b].0, keyed[b].1) == (keyed[a].0, keyed[a].1) {
                        b += 1;
                    }
                    if b - a > 1 {
                        if keyed[a].1 > level + TBYTES && b - a > SMALL_RUN {
                            work.push((s + a, s + b, level + TBYTES));
                        } else {
                            groups.push(s + a..s + b);
                        }
                    }
                    a = b;
                }
            }
        } else {
            groups.push(0..run.len());
        }
        // Small or equal groups: full comparison of every part.
        let others: Vec<usize> = self.text_parts[1..].to_vec();
        for g in groups {
            let extra: Vec<Vec<std::borrow::Cow<'_, [u8]>>> = pos[g.clone()]
                .iter()
                .map(|&j| {
                    others
                        .iter()
                        .map(|&p| self.cell(&ix, run[j], self.keys[p].col))
                        .collect()
                })
                .collect();
            let mut order: Vec<usize> = (0..g.len()).collect();
            order.sort_by(|&x, &y| {
                let (jx, jy) = (pos[g.start + x], pos[g.start + y]);
                let (ia, ib) = (run[jx] as usize, run[jy] as usize);
                for p in 0..self.keys.len() {
                    let o = if p == q {
                        let o = columns::text_cmp(&cells[jx], &cells[jy]);
                        if desc { o.reverse() } else { o }
                    } else if let Some(j) = others.iter().position(|&t| t == p) {
                        let o = columns::text_cmp(&extra[x][j], &extra[y][j]);
                        if self.keys[p].desc { o.reverse() } else { o }
                    } else {
                        let w = width(self.kinds[p]);
                        let (fa, fb) = (ia * self.k + self.off[p], ib * self.k + self.off[p]);
                        self.flat[fa..fa + w].cmp(&self.flat[fb..fb + w])
                    };
                    if o != Ordering::Equal {
                        return o;
                    }
                }
                ia.cmp(&ib)
            });
            let sorted: Vec<usize> = order.iter().map(|&x| pos[g.start + x]).collect();
            pos[g].copy_from_slice(&sorted);
        }
        Ok(pos.into_iter().map(|j| run[j]).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_preserve_order() {
        let xs = [-5.5f64, -0.0, 0.0, 1e-9, 3.0, 1e300];
        for w in xs.windows(2) {
            let k = |x: f64| Num::Float(x).sort_key();
            assert!(k(w[0]) <= k(w[1]), "{w:?}");
        }
        assert!(Num::MIN_KEY < Num::Float(-1e300).sort_key());
        assert!(i64_key(i64::MIN) < i64_key(-1));
        assert!(i64_key(-1) < i64_key(0));
        let w = |c: &[u8], from| {
            let mut o = [0u64; TW];
            text_window(c, from, &mut o);
            o
        };
        assert!(w(b"abc", 0) < w(b"abd", 0));
        assert!(w(b"", 0) < w(b"a", 0));
        assert_eq!(w(b"ABC", 0), w(b"abc", 0));
        assert_eq!(w(b"0123456789abcdefghijklmnopq", 24), w(b"opq", 0));
        assert!(!window_exact(b"ABC") && window_exact(b"abc"));
        assert!(!window_exact(&[b'a'; 25]));
    }
}
