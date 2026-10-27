//! Occurrence counts of the values of one column, for highlighting
//! duplicate (2+) and unique (1) values (D-066).
//!
//! Dictionary columns are counted from their per-value bitmaps (instant,
//! within a row set by bitmap intersection). Other columns are counted by a
//! parallel scan of the rows in scope, cancellable between blocks.
//!
//! Bounded memory: values are keyed by a 64-bit hash (SipHash with a random
//! key per count), not stored, so a count costs ~16-24 bytes per distinct
//! value, and at most [`MAX_DISTINCT`] values are tracked. Past the cap,
//! values first seen afterwards are not counted (`count` returns `None`,
//! `capped` is set); values already tracked keep exact counts. Blank cells
//! (empty or whitespace only, missing fields included) and cells longer
//! than [`MAX_CELL`] bytes are ignored. Two distinct values share a hash
//! with probability ~n²/2^65 (1e-7 at the cap).

use std::hash::{BuildHasher, RandomState};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use roaring::RoaringBitmap;
use rustc_hash::FxHashMap;

use crate::columns::field_bytes;
use crate::scan::nth_field;
use crate::table::Table;
use crate::{Error, Result};

/// Distinct values tracked at most (~48 MiB of hash map).
pub const MAX_DISTINCT: usize = 1 << 21;
/// Longer cells are ignored: the grid holds only the first 4 KiB of a cell,
/// so a cut cell could not be looked up.
pub const MAX_CELL: usize = 4000;

const BLOCK_ROWS: u64 = 32_768;

pub struct Counts {
    state: RandomState,
    case_sensitive: bool,
    map: FxHashMap<u64, u32>,
    /// More than [`MAX_DISTINCT`] distinct values: some are not counted.
    pub capped: bool,
}

impl Counts {
    fn new(case_sensitive: bool) -> Counts {
        Counts {
            state: RandomState::new(),
            case_sensitive,
            map: FxHashMap::default(),
            capped: false,
        }
    }

    /// Hash key of a cell; None when ignored (blank or too long).
    pub fn key(&self, cell: &str) -> Option<u64> {
        if cell.len() > MAX_CELL || cell.bytes().all(|b| b.is_ascii_whitespace()) {
            return None;
        }
        Some(if self.case_sensitive {
            self.state.hash_one(cell.as_bytes())
        } else {
            match crate::filter::fold_simple(cell.as_bytes()) {
                Some(f) => self.state.hash_one(&f),
                None => self.state.hash_one(cell.as_bytes()),
            }
        })
    }

    fn add(&mut self, key: u64, n: u32) {
        let full = self.map.len() >= MAX_DISTINCT;
        match self.map.get_mut(&key) {
            Some(c) => *c = c.saturating_add(n),
            None if full => self.capped = true,
            None => {
                self.map.insert(key, n);
            }
        }
    }

    /// Occurrences of the value of `cell` in scope (None: ignored or not
    /// counted, see the module doc). O(1) besides hashing the cell.
    pub fn count(&self, cell: &str) -> Option<u32> {
        self.map.get(&self.key(cell)?).copied()
    }

    /// Distinct values tracked.
    pub fn distinct(&self) -> usize {
        self.map.len()
    }

    /// Distinct values occurring once, and 2+ times.
    pub fn unique_and_duplicate(&self) -> (u64, u64) {
        let u = self.map.values().filter(|&&c| c == 1).count() as u64;
        (u, self.map.len() as u64 - u)
    }
}

/// Counts the values of column `col` over `within` (all rows when None).
/// `done` receives the rows processed so far; `cancel` stops the scan
/// (`Error::Cancelled`), as does the table's own cancellation.
pub fn count(
    table: &Table,
    col: usize,
    within: Option<&RoaringBitmap>,
    case_sensitive: bool,
    cancel: &AtomicBool,
    done: &AtomicU64,
) -> Result<Counts> {
    let mut out = Counts::new(case_sensitive);
    if col >= table.columns.len() {
        return Err(Error::Other(format!("no column {col}")));
    }
    if let Some(d) = table
        .column_index()
        .and_then(|ci| ci.cols[col].dict.clone())
    {
        for (v, b) in d.values.iter().zip(&d.bitmaps) {
            let n = within.map_or(b.len(), |w| b.intersection_len(w));
            if n > 0
                && let Some(k) = out.key(v)
            {
                out.add(k, n.min(u32::MAX as u64) as u32);
            }
        }
        return Ok(out);
    }
    let n = table.row_count();
    let blocks = match within {
        Some(w) => table.par_blocks_of(w, BLOCK_ROWS),
        None if n == 0 => Vec::new(),
        None => (0..n.div_ceil(BLOCK_ROWS))
            .map(|i| i * BLOCK_ROWS..((i + 1) * BLOCK_ROWS).min(n))
            .collect(),
    };
    let t = table.tables();
    let quote = table.dialect.quote;
    let keyer = Counts {
        state: out.state.clone(),
        case_sensitive,
        map: FxHashMap::default(),
        capped: false,
    };
    table.par_block_list(
        &blocks,
        |b| {
            let mut local: FxHashMap<u64, u32> = FxHashMap::default();
            let mut tmp = Vec::new();
            let mut rows = 0u64;
            for k in 0..b.len() {
                let r = b.first + k as u64;
                if within.is_some_and(|w| !w.contains(r as u32)) {
                    continue;
                }
                rows += 1;
                let row = b.row(k);
                let cell = match nth_field(t, row, col) {
                    Some(s) => field_bytes(row, s, quote, &mut tmp),
                    None => &[][..],
                };
                if let Some(key) = keyer.key(&String::from_utf8_lossy(cell)) {
                    let c = local.entry(key).or_default();
                    *c = c.saturating_add(1);
                }
            }
            (rows, local)
        },
        |_, (rows, local)| {
            if cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            done.fetch_add(rows, Ordering::Relaxed);
            for (k, c) in local {
                out.add(k, c);
            }
            Ok(())
        },
    )?;
    Ok(out)
}
