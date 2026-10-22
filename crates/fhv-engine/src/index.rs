//! Row index: the byte offset where each row starts, built by one parallel,
//! quote-aware pass, stored in ~4 bytes per row.

use std::sync::RwLock;
use std::sync::atomic::Ordering;

use rayon::prelude::*;
use roaring::RoaringBitmap;

use crate::progress::Progress;
use crate::scan::{self, ChunkScan, Tables};
use crate::source::Source;
use crate::{Error, Result};

const BLOCK: usize = 1024;
const WIDE: u64 = 1 << 63;

/// Chunk of the file scanned by one task.
pub const CHUNK: u64 = 8 << 20;

/// A record longer than this (an unterminated quote running on) is cut back
/// into its physical lines, which are flagged as bad rows.
pub const MAX_ROW: u64 = 16 << 20;

/// Row start offsets, append-only (readers may look at it while it grows).
///
/// Rows are grouped in blocks of 1024: one `u64` base per block plus one
/// `u32` delta per row. A block spanning more than 4 GiB (rows averaging
/// over 4 MiB) stores absolute offsets in `wide` instead; its base then has
/// the top bit set and holds the position in `wide`.
#[derive(Default, Clone)]
pub struct RowIndex {
    bases: Vec<u64>,
    deltas: Vec<u32>,
    wide: Vec<u64>,
    /// Starts of the rows of the last, incomplete block.
    pending: Vec<u64>,
    /// End of the last indexed row (start of the next, if any).
    end: u64,
    /// Structurally bad rows: lines cut out of an oversized record (see
    /// `MAX_ROW`) and a last row that ends inside an unterminated quote.
    pub bad: RoaringBitmap,
    pub complete: bool,
}

impl RowIndex {
    pub fn len(&self) -> u64 {
        (self.deltas.len() + self.pending.len()) as u64
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    #[inline]
    pub fn start(&self, i: u64) -> u64 {
        let i = i as usize;
        if i >= self.deltas.len() {
            return self.pending[i - self.deltas.len()];
        }
        let b = self.bases[i / BLOCK];
        if b & WIDE == 0 {
            b + self.deltas[i] as u64
        } else {
            self.wide[(b & !WIDE) as usize + i % BLOCK]
        }
    }

    /// Byte range of row `i`, including its line terminator (and any blank
    /// lines that followed it).
    #[inline]
    pub fn range(&self, i: u64) -> std::ops::Range<u64> {
        let s = self.start(i);
        let e = if i + 1 < self.len() {
            self.start(i + 1)
        } else {
            self.end
        };
        s..e
    }

    fn push(&mut self, start: u64) {
        self.pending.push(start);
        if self.pending.len() == BLOCK {
            self.flush_block();
        }
    }

    fn flush_block(&mut self) {
        let first = self.pending[0];
        let last = *self.pending.last().unwrap_or(&first);
        if last - first <= u32::MAX as u64 {
            self.bases.push(first);
            self.deltas
                .extend(self.pending.iter().map(|&s| (s - first) as u32));
        } else {
            self.bases.push(WIDE | self.wide.len() as u64);
            self.wide.extend_from_slice(&self.pending);
            self.deltas
                .extend(std::iter::repeat_n(0, self.pending.len()));
        }
        self.pending.clear();
    }

    /// Heap bytes used.
    pub fn heap_bytes(&self) -> usize {
        self.bases.capacity() * 8
            + self.deltas.capacity() * 4
            + self.wide.capacity() * 8
            + self.pending.capacity() * 8
            + self.bad.serialized_size()
    }

    /// Flat serialization for the index cache.
    pub fn to_parts(&self) -> (Vec<u64>, &[u32], &[u64], &[u64], u64) {
        (
            self.bases.clone(),
            &self.deltas,
            &self.wide,
            &self.pending,
            self.end,
        )
    }

    pub fn from_parts(
        bases: Vec<u64>,
        deltas: Vec<u32>,
        wide: Vec<u64>,
        pending: Vec<u64>,
        end: u64,
        bad: RoaringBitmap,
    ) -> Result<Self> {
        let ok = bases.len() * BLOCK == deltas.len()
            && pending.len() < BLOCK
            && bases
                .iter()
                .filter(|b| *b & WIDE != 0)
                .all(|b| ((b & !WIDE) as usize + BLOCK) <= wide.len());
        if !ok {
            return Err(Error::Cache("row index shape".into()));
        }
        Ok(RowIndex {
            bases,
            deltas,
            wide,
            pending,
            end,
            bad,
            complete: true,
        })
    }
}

/// Sequential state of the fix-up pass.
struct Fixup {
    entry: u8,
    /// Start of the row being read.
    cur: u64,
    batch: Vec<u64>,
    bad: Vec<u64>,
    rows: u64,
}

fn byte_at(src: &Source, pos: u64) -> Result<u8> {
    let mut b = Vec::new();
    Ok(src
        .read(pos..pos + 1, &mut b)?
        .first()
        .copied()
        .unwrap_or(0))
}

/// A line `[s, e)` (without its `\n`) holding nothing or a lone `\r`.
fn is_blank(src: &Source, s: u64, e: u64) -> Result<bool> {
    Ok(e == s || (e == s + 1 && byte_at(src, s)? == b'\r'))
}

impl Fixup {
    /// Row `[self.cur, end)` ended (`end` = newline position or EOF).
    fn row_end(&mut self, src: &Source, end: u64) -> Result<()> {
        let s = self.cur;
        if is_blank(src, s, end)? {
            return Ok(());
        }
        if end - s > MAX_ROW {
            self.split(src, s, end)
        } else {
            self.batch.push(s);
            self.rows += 1;
            Ok(())
        }
    }

    fn push_bad(&mut self, start: u64) {
        self.bad.push(self.rows);
        self.batch.push(start);
        self.rows += 1;
    }

    /// Cuts an oversized record at every newline; each line is a bad row.
    fn split(&mut self, src: &Source, s: u64, e: u64) -> Result<()> {
        let mut buf = Vec::new();
        let mut line = s;
        let mut off = s;
        while off < e {
            let block_end = (off + CHUNK).min(e);
            let nls: Vec<u64> = memchr::memchr_iter(b'\n', src.read(off..block_end, &mut buf)?)
                .map(|p| off + p as u64)
                .collect();
            for nl in nls {
                if !is_blank(src, line, nl)? {
                    self.push_bad(line);
                }
                line = nl + 1;
            }
            off = block_end;
        }
        if line < e && !is_blank(src, line, e)? {
            self.push_bad(line);
        }
        Ok(())
    }
}

/// Builds the row index of `src` into `out` (which readers may observe
/// growing). Rows are pushed window by window; `progress` reports bytes and
/// is checked for cancellation between windows.
pub fn build(src: &Source, t: &Tables, out: &RwLock<RowIndex>, progress: &Progress) -> Result<()> {
    build_with(src, t, out, progress, CHUNK)
}

/// [`build`] with a given chunk size (tests use tiny chunks to hit every
/// boundary case).
pub fn build_with(
    src: &Source,
    t: &Tables,
    out: &RwLock<RowIndex>,
    progress: &Progress,
    chunk: u64,
) -> Result<()> {
    let start = src.data_start;
    let len = src.len;
    let chunks: Vec<(u64, u64)> = (0..)
        .map(|k| start + k * chunk)
        .take_while(|&o| o < len)
        .map(|o| (o, (o + chunk).min(len)))
        .collect();
    let window = rayon::current_num_threads().max(1) * 2;
    let mut fx = Fixup {
        entry: scan::S,
        cur: start,
        batch: Vec::new(),
        bad: Vec::new(),
        rows: 0,
    };
    // The first window is a single chunk so the first rows show at once.
    let first = chunks.len().min(1);
    let windows = std::iter::once(&chunks[..first]).chain(chunks[first..].chunks(window));
    for w in windows.filter(|w| !w.is_empty()) {
        if progress.cancelled() {
            return Err(Error::Cancelled);
        }
        let scans: Vec<Result<ChunkScan>> = w
            .par_iter()
            .map_init(Vec::new, |buf, &(a, b)| {
                let bytes = src.read(a..b, buf)?;
                Ok(scan::scan_chunk(t, bytes))
            })
            .collect();
        for (&(base, _), sc) in w.iter().zip(scans) {
            let sc = sc?;
            let bit = 1u8 << fx.entry;
            for (&p, &m) in sc.pos.iter().zip(&sc.mask) {
                if m & bit != 0 {
                    let nl = base + p as u64;
                    fx.row_end(src, nl)?;
                    fx.cur = nl + 1;
                }
            }
            fx.entry = sc.exit_state(fx.entry);
        }
        let done = w.last().map_or(start, |c| c.1);
        let end = fx.cur;
        publish(out, &mut fx, end);
        progress.bytes.store(done - start, Ordering::Relaxed);
        progress.rows.store(fx.rows, Ordering::Relaxed);
    }
    // Last row without a final newline.
    if fx.cur < len {
        let before = fx.rows;
        fx.row_end(src, len)?;
        if fx.entry == scan::Q && fx.rows > before {
            let last = fx.rows - 1;
            if fx.bad.last() != Some(&last) {
                fx.bad.push(last);
            }
        }
    }
    publish(out, &mut fx, len);
    let mut ix = out.write().map_err(|_| Error::Poisoned)?;
    ix.end = len;
    ix.deltas.shrink_to_fit();
    ix.bases.shrink_to_fit();
    ix.wide.shrink_to_fit();
    ix.complete = true;
    progress.rows.store(fx.rows, Ordering::Relaxed);
    Ok(())
}

fn publish(out: &RwLock<RowIndex>, fx: &mut Fixup, end: u64) {
    if fx.batch.is_empty() && fx.bad.is_empty() {
        return;
    }
    let Ok(mut ix) = out.write() else { return };
    for &s in &fx.batch {
        ix.push(s);
    }
    for &b in &fx.bad {
        ix.bad.insert(b as u32);
    }
    ix.end = end;
    fx.batch.clear();
    fx.bad.clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_blocks_and_lookup() {
        let mut ix = RowIndex::default();
        let mut starts = Vec::new();
        let mut off = 0u64;
        for i in 0..5000u64 {
            starts.push(off);
            ix.push(off);
            // Block 2 spans > 4 GiB.
            off += if (2048..3072).contains(&i) {
                5 << 20
            } else {
                100
            };
        }
        ix.end = off;
        for (i, &s) in starts.iter().enumerate() {
            assert_eq!(ix.start(i as u64), s, "row {i}");
        }
        assert_eq!(ix.range(4999), starts[4999]..off);
        assert!(ix.bases[2] & WIDE != 0);
    }
}
