//! A CSV file opened for viewing: encoding, dialect, header, row index and
//! row access.

use std::borrow::Cow;
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock, RwLockReadGuard};

use crate::columns::{self, Columns};
use crate::dialect::{self, Dialect, Encoding};
use crate::index::{self, RowIndex};
use crate::progress::{Phase, Progress};
use crate::scan::{Span, Tables, split_row, trim_eol, unquote};
use crate::source::{Access, Source};
use crate::{Error, Result};

/// Head of the file read to detect encoding and dialect.
const SNIFF_BYTES: u64 = 1 << 20;

#[derive(Clone, Debug)]
pub struct OpenOptions {
    /// Where derived files go (transcoded copies, index cache). Never the
    /// evidence directory.
    pub cache_dir: PathBuf,
    pub access: Access,
    /// Overrides the detected dialect.
    pub dialect: Option<Dialect>,
    /// Overrides the detected encoding (a BOM is still skipped).
    pub encoding: Option<Encoding>,
    /// Load and save the index cache.
    pub use_cache: bool,
}

impl OpenOptions {
    pub fn new(cache_dir: impl Into<PathBuf>) -> Self {
        OpenOptions {
            cache_dir: cache_dir.into(),
            access: Access::Pread,
            dialect: None,
            encoding: None,
            use_cache: true,
        }
    }
}

pub struct Table {
    pub path: PathBuf,
    pub file_len: u64,
    pub encoding: Encoding,
    pub dialect: Dialect,
    pub columns: Vec<String>,
    opts: OpenOptions,
    tables: Tables,
    source: OnceLock<Source>,
    index: RwLock<RowIndex>,
    pub progress: Progress,
    /// Content key of the input + options (index cache, transcoded copy).
    key: String,
    /// The index came from the cache.
    pub from_cache: std::sync::atomic::AtomicBool,
    /// Column indexes, once built.
    col_index: RwLock<Option<Arc<Columns>>>,
}

/// Decoded cells of a range of rows, for display.
#[derive(Default)]
pub struct Rows {
    pub first: u64,
    pub ncols: usize,
    cells: Vec<String>,
    /// Number of fields each row really has (ragged rows differ from ncols).
    pub nfields: Vec<u32>,
    /// Data row of each position.
    ids: Vec<u32>,
    raw: Vec<u8>,
    spans: Vec<Span>,
    tmp: Vec<u8>,
}

impl Rows {
    pub fn len(&self) -> usize {
        self.nfields.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nfields.is_empty()
    }

    /// Data row shown at position `k` of this batch.
    pub fn id(&self, k: usize) -> Option<u32> {
        self.ids.get(k).copied()
    }

    /// Cell at display position `row` (`first` + k).
    pub fn cell(&self, row: u64, col: usize) -> &str {
        let k = (row.wrapping_sub(self.first)) as usize;
        self.cells
            .get(k * self.ncols + col)
            .map(String::as_str)
            .unwrap_or("")
    }

    pub fn cell_mut(&mut self, row: u64, col: usize) -> Option<&mut String> {
        let k = (row.wrapping_sub(self.first)) as usize;
        self.cells.get_mut(k * self.ncols + col)
    }

    pub fn contains(&self, row: u64) -> bool {
        row >= self.first && row < self.first + self.len() as u64
    }
}

/// Contiguous data rows read in one go (see [`Table::par_blocks`]).
pub struct Block<'a> {
    /// First data row of the block.
    pub first: u64,
    base: u64,
    /// Row start offsets plus the end of the last row.
    offs: &'a [u64],
    bytes: &'a [u8],
}

impl<'a> Block<'a> {
    pub fn len(&self) -> usize {
        self.offs.len() - 1
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Bytes of the k-th row of the block, without line terminator.
    #[inline]
    pub fn row(&self, k: usize) -> &'a [u8] {
        let s = (self.offs[k] - self.base) as usize;
        let e = (self.offs[k + 1] - self.base) as usize;
        trim_eol(&self.bytes[s..e])
    }

    /// All bytes of the block (rows with terminators).
    pub fn bytes(&self) -> &'a [u8] {
        self.bytes
    }

    /// Row (k) containing byte position `pos` of [`Block::bytes`].
    pub fn row_at(&self, pos: usize) -> usize {
        let abs = self.base + pos as u64;
        self.offs.partition_point(|&o| o <= abs) - 1
    }
}

/// Lossy UTF-8 decoding of a field, unquoting when needed.
pub fn decode_field<'a>(
    raw: &'a [u8],
    span: &Span,
    quote: Option<u8>,
    tmp: &'a mut Vec<u8>,
) -> Cow<'a, str> {
    let bytes = &raw[span.start as usize..span.end as usize];
    let bytes: &[u8] = match (span.quoted, quote) {
        (true, Some(q)) => {
            unquote(bytes, q, tmp);
            tmp
        }
        _ => bytes,
    };
    String::from_utf8_lossy(bytes)
}

fn unique_names(raw: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(raw.len());
    for (i, name) in raw.into_iter().enumerate() {
        let base = if name.trim().is_empty() {
            format!("Column {}", i + 1)
        } else {
            name
        };
        let mut n = base.clone();
        let mut k = 2;
        while out.contains(&n) {
            n = format!("{base} ({k})");
            k += 1;
        }
        out.push(n);
    }
    out
}

/// Cache key of a file: canonical path, size, mtime and a hash of sampled
/// content (first, middle and last 64 KiB).
pub fn file_key(path: &Path) -> Result<String> {
    let meta = fs::metadata(path)?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos());
    let canon = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let mut h = blake3::Hasher::new();
    h.update(canon.to_string_lossy().as_bytes());
    h.update(&meta.len().to_le_bytes());
    h.update(&mtime.to_le_bytes());
    let len = meta.len();
    let src = Source::open(path, Access::Pread, 0)?;
    let mut b = Vec::new();
    for off in [0, len / 2, len.saturating_sub(64 << 10)] {
        h.update(src.read(off..off + (64 << 10), &mut b)?);
    }
    Ok(h.finalize().to_hex()[..32].to_string())
}

impl Table {
    /// Opens `path` read-only and detects encoding, dialect and header.
    /// Fast (reads the first MiB); call [`Table::build`] next, from a worker.
    pub fn open(path: &Path, opts: OpenOptions) -> Result<Table> {
        let meta = fs::metadata(path)?;
        if meta.is_dir() {
            return Err(Error::Other(format!("{} is a directory", path.display())));
        }
        let file_len = meta.len();
        let probe = Source::open(path, Access::Pread, 0)?;
        let mut buf = Vec::new();
        let head = probe.read(0..SNIFF_BYTES, &mut buf)?.to_vec();
        let (detected, bom) = dialect::detect_encoding(&head);
        let encoding = opts.encoding.unwrap_or(detected);
        let mut sample = Vec::new();
        if encoding.needs_transcoding() {
            dialect::transcode(encoding, &head[bom..], &mut sample);
        } else {
            sample.extend_from_slice(&head[bom..]);
        }
        let whole = file_len <= SNIFF_BYTES;
        let dialect = opts
            .dialect
            .unwrap_or_else(|| dialect::sniff(&sample, whole));
        let tables = Tables::new(dialect.delim, dialect.quote);

        // First row: header names, or the column count for generic names.
        let first_row = {
            let scan = crate::scan::scan_chunk(&tables, &sample);
            let mut start = 0usize;
            let mut row: &[u8] = &[];
            let ends = scan.pos.iter().zip(&scan.mask).filter(|(_, m)| *m & 1 != 0);
            for (&p, _) in ends.map(|e| (e.0, e.1)) {
                let r = trim_eol(&sample[start..p as usize]);
                start = p as usize + 1;
                if !r.is_empty() {
                    row = r;
                    break;
                }
            }
            if row.is_empty() {
                row = trim_eol(&sample[start.min(sample.len())..]);
            }
            row.to_vec()
        };
        let mut spans = Vec::new();
        let n = split_row(&tables, &first_row, &mut spans);
        let mut tmp = Vec::new();
        let names: Vec<String> = if dialect.header && !first_row.is_empty() {
            spans
                .iter()
                .map(|s| {
                    decode_field(&first_row, s, dialect.quote, &mut tmp)
                        .trim()
                        .to_string()
                })
                .collect()
        } else {
            (1..=n).map(|i| format!("Column {i}")).collect()
        };

        let key = {
            let mut h = blake3::Hasher::new();
            h.update(file_key(path)?.as_bytes());
            h.update(format!("{:?}{:?}{:?}", opts.dialect, opts.encoding, encoding).as_bytes());
            h.finalize().to_hex()[..32].to_string()
        };
        let table = Table {
            key,
            from_cache: Default::default(),
            col_index: RwLock::new(None),
            path: path.to_path_buf(),
            file_len,
            encoding,
            dialect,
            columns: unique_names(names),
            opts,
            tables,
            source: OnceLock::new(),
            index: RwLock::new(RowIndex::default()),
            progress: Progress::default(),
        };
        if !encoding.needs_transcoding() {
            let src = Source::open(path, table.opts.access, bom as u64)?;
            let _ = table.source.set(src);
        }
        Ok(table)
    }

    /// Transcodes (UTF-16, Windows-1252) and indexes the file. Rows become
    /// visible while this runs. Blocking: run it on a worker thread.
    pub fn build(&self) -> Result<()> {
        self.build_chunked(index::CHUNK)
    }

    #[doc(hidden)]
    pub fn build_chunked(&self, chunk: u64) -> Result<()> {
        let r = self.build_inner(chunk);
        self.progress.set_phase(
            if r.is_ok() {
                Phase::Done
            } else {
                Phase::Failed
            },
            0,
        );
        r
    }

    fn build_inner(&self, chunk: u64) -> Result<()> {
        if self.opts.use_cache && self.load_cache().is_ok() {
            self.from_cache
                .store(true, std::sync::atomic::Ordering::Relaxed);
            return Ok(());
        }
        if self.source.get().is_none() {
            let out = self.transcode()?;
            let src = Source::open(&out, self.opts.access, 0)?;
            let _ = self.source.set(src);
        }
        let src = self.source()?;
        self.progress
            .set_phase(Phase::Indexing, src.len - src.data_start);
        index::build_with(src, &self.tables, &self.index, &self.progress, chunk)?;
        let cols = columns::build(self)?;
        *self.col_index.write().map_err(|_| Error::Poisoned)? = Some(Arc::new(cols));
        if self.opts.use_cache {
            // A cache that cannot be written only costs a rebuild next time.
            let _ = self.save_cache();
        }
        Ok(())
    }

    fn cache_path(&self) -> PathBuf {
        crate::cache::path_for(&self.opts.cache_dir, &self.key)
    }

    fn transcoded_path(&self) -> PathBuf {
        self.opts
            .cache_dir
            .join("transcoded")
            .join(format!("{}.utf8.csv", self.key))
    }

    fn save_cache(&self) -> Result<()> {
        let ix = self.index();
        let mut w = crate::cache::Writer::new();
        w.begin(b"META\0\0\0\0");
        w.bytes(self.key.as_bytes());
        w.u64(self.file_len);
        w.u64(ix.len());
        w.u64(self.columns.len() as u64);
        w.begin(b"ROWS\0\0\0\0");
        let (bases, deltas, wide, pending, end) = ix.to_parts();
        w.u64s(&bases);
        w.u32s(deltas);
        w.u64s(wide);
        w.u64s(pending);
        w.u64(end);
        w.bitmap(&ix.bad);
        drop(ix);
        if let Some(cols) = self.column_index() {
            w.begin(b"COLS\0\0\0\0");
            save_columns(&mut w, &cols);
        }
        w.finish(&self.cache_path())
    }

    fn load_cache(&self) -> Result<()> {
        let r = crate::cache::Reader::open(&self.cache_path())?;
        let mut m = r.section(b"META\0\0\0\0")?;
        let key_ok = m.bytes()? == self.key.as_bytes();
        let len_ok = m.u64()? == self.file_len;
        let _rows = m.u64()?;
        let cols_ok = m.u64()? == self.columns.len() as u64;
        if !(key_ok && len_ok && cols_ok) {
            return Err(Error::Cache("key mismatch".into()));
        }
        if self.source.get().is_none() {
            let t = self.transcoded_path();
            if !t.exists() {
                return Err(Error::Cache("transcoded copy missing".into()));
            }
            let _ = self.source.set(Source::open(&t, self.opts.access, 0)?);
        }
        let mut c = r.section(b"ROWS\0\0\0\0")?;
        let ix = RowIndex::from_parts(
            c.u64s()?,
            c.u32s()?,
            c.u64s()?,
            c.u64s()?,
            c.u64()?,
            c.bitmap()?,
        )?;
        if !ix.is_empty() && ix.range(ix.len() - 1).end > self.source()?.len {
            return Err(Error::Cache("offsets past end of file".into()));
        }
        let cols = load_columns(
            &mut r.section(b"COLS\0\0\0\0")?,
            self.columns.len(),
            ix.len(),
        )?;
        self.progress
            .rows
            .store(ix.len(), std::sync::atomic::Ordering::Relaxed);
        *self.index.write().map_err(|_| Error::Poisoned)? = ix;
        *self.col_index.write().map_err(|_| Error::Poisoned)? = Some(Arc::new(cols));
        Ok(())
    }

    fn transcode(&self) -> Result<PathBuf> {
        let out = self.transcoded_path();
        if let Some(dir) = out.parent() {
            fs::create_dir_all(dir)?;
        }
        if out.exists() {
            return Ok(out);
        }
        self.progress.set_phase(Phase::Transcoding, self.file_len);
        let tmp_path = out.with_extension("partial");
        let mut w = BufWriter::with_capacity(4 << 20, File::create(&tmp_path)?);
        let src = Source::open(&self.path, Access::Pread, 0)?;
        let (_, bom) = dialect::detect_encoding(src.read(0..4, &mut Vec::new())?);
        let mut off = bom as u64;
        let mut buf = Vec::new();
        let mut carry: Vec<u8> = Vec::new();
        let mut utf8 = Vec::new();
        while off < self.file_len {
            if self.progress.cancelled() {
                drop(w);
                let _ = fs::remove_file(&tmp_path);
                return Err(Error::Cancelled);
            }
            let block = src.read(off..off + (4 << 20), &mut buf)?;
            off += block.len() as u64;
            carry.extend_from_slice(block);
            utf8.clear();
            let used = dialect::transcode(self.encoding, &carry, &mut utf8);
            carry.drain(..used);
            w.write_all(&utf8)?;
            self.progress
                .bytes
                .store(off, std::sync::atomic::Ordering::Relaxed);
        }
        if !carry.is_empty() {
            // Odd trailing byte or lone high surrogate.
            w.write_all("\u{FFFD}".as_bytes())?;
        }
        w.flush()?;
        drop(w);
        fs::rename(&tmp_path, &out)?;
        Ok(out)
    }

    fn source(&self) -> Result<&Source> {
        self.source
            .get()
            .ok_or_else(|| Error::Other("file not ready".into()))
    }

    /// Column indexes (None until `build` has finished them).
    pub fn column_index(&self) -> Option<Arc<Columns>> {
        self.col_index.read().ok().and_then(|g| g.clone())
    }

    pub fn index(&self) -> RwLockReadGuard<'_, RowIndex> {
        self.index.read().unwrap_or_else(|e| e.into_inner())
    }

    /// Transient read-only memory map of the data being viewed (the
    /// original file or its transcoded copy), for random access during a
    /// sort. Dropped by the caller as soon as it is done.
    pub fn map_data(&self) -> Result<memmap2::Mmap> {
        let src = self.source()?;
        let f = File::open(src.path())?;
        // SAFETY: read-only mapping of a file nothing writes to (see
        // `Source::open`).
        Ok(unsafe { memmap2::Mmap::map(&f)? })
    }

    pub fn header_rows(&self) -> u64 {
        self.dialect.header as u64
    }

    /// Data rows indexed so far (all of them once `build` finished).
    pub fn row_count(&self) -> u64 {
        self.index().len().saturating_sub(self.header_rows())
    }

    pub fn is_complete(&self) -> bool {
        self.index().complete
    }

    /// Structurally bad rows (see `RowIndex::bad`), as data row numbers.
    pub fn structural_bad_rows(&self) -> roaring::RoaringBitmap {
        let h = self.header_rows() as u32;
        self.index()
            .bad
            .iter()
            .filter(|&r| r >= h)
            .map(|r| r - h)
            .collect()
    }

    /// Raw bytes of data row `row` (no line terminator).
    pub fn raw_row<'a>(&'a self, row: u64, buf: &'a mut Vec<u8>) -> Result<&'a [u8]> {
        let range = self.index().range(row + self.header_rows());
        Ok(trim_eol(self.source()?.read(range, buf)?))
    }

    /// All fields of one data row, decoded (including extra fields of a
    /// ragged row).
    pub fn row_fields(&self, row: u64) -> Result<Vec<String>> {
        let mut buf = Vec::new();
        let raw = self.raw_row(row, &mut buf)?;
        let mut spans = Vec::new();
        split_row(&self.tables, raw, &mut spans);
        let mut tmp = Vec::new();
        Ok(spans
            .iter()
            .map(|s| decode_field(raw, s, self.dialect.quote, &mut tmp).into_owned())
            .collect())
    }

    /// Decodes data rows `range` into `out` (clamped to the rows available).
    pub fn read_rows(&self, range: Range<u64>, out: &mut Rows) -> Result<()> {
        let ncols = self.columns.len();
        out.first = range.start;
        out.ncols = ncols;
        out.nfields.clear();
        out.ids.clear();
        let ix = self.index();
        let h = self.header_rows();
        let avail = ix.len().saturating_sub(h);
        let end = range.end.min(avail);
        if range.start >= end {
            out.cells.clear();
            return Ok(());
        }
        let n = (end - range.start) as usize;
        if out.cells.len() < n * ncols {
            out.cells.resize(n * ncols, String::new());
        }
        out.cells.truncate(n * ncols);
        // One read for the whole span of rows (they are contiguous).
        let first = ix.range(range.start + h).start;
        let last = ix.range(end - 1 + h).end;
        let src = self.source()?;
        let Rows {
            raw,
            spans,
            tmp,
            cells,
            nfields,
            ids,
            ..
        } = out;
        let bytes = src.read(first..last, raw)?;
        for k in 0..n {
            let r = ix.range(range.start + h + k as u64);
            let row = trim_eol(&bytes[(r.start - first) as usize..(r.end - first) as usize]);
            let nf = split_row(&self.tables, row, spans);
            nfields.push(nf as u32);
            ids.push((range.start + k as u64) as u32);
            for c in 0..ncols {
                let cell = &mut cells[k * ncols + c];
                cell.clear();
                if let Some(s) = spans.get(c) {
                    cell.push_str(&decode_field(row, s, self.dialect.quote, tmp));
                }
            }
        }
        Ok(())
    }

    /// Decodes the given data rows (any order) into `out`, at display
    /// positions `first..first + rows.len()`.
    pub fn read_row_list(&self, first: u64, rows: &[u32], out: &mut Rows) -> Result<()> {
        let ncols = self.columns.len();
        out.first = first;
        out.ncols = ncols;
        out.nfields.clear();
        out.ids.clear();
        let n = rows.len();
        if out.cells.len() < n * ncols {
            out.cells.resize(n * ncols, String::new());
        }
        out.cells.truncate(n * ncols);
        let ix = self.index();
        let h = self.header_rows();
        let avail = ix.len().saturating_sub(h);
        let src = self.source()?;
        let Rows {
            raw,
            spans,
            tmp,
            cells,
            nfields,
            ids,
            ..
        } = out;
        for (k, &r) in rows.iter().enumerate() {
            ids.push(r);
            if r as u64 >= avail {
                nfields.push(0);
                for c in 0..ncols {
                    cells[k * ncols + c].clear();
                }
                continue;
            }
            let bytes = trim_eol(src.read(ix.range(r as u64 + h), raw)?);
            let nf = split_row(&self.tables, bytes, spans);
            nfields.push(nf as u32);
            for c in 0..ncols {
                let cell = &mut cells[k * ncols + c];
                cell.clear();
                if let Some(s) = spans.get(c) {
                    cell.push_str(&decode_field(bytes, s, self.dialect.quote, tmp));
                }
            }
        }
        Ok(())
    }

    /// Runs `map` over blocks of data rows in parallel and feeds the results
    /// to `consume` in row order. Memory is bounded by a window of blocks;
    /// cancellation (`progress.cancel()`) is checked between windows.
    /// Requires a complete row index.
    pub fn par_blocks<T: Send>(
        &self,
        rows: Range<u64>,
        block_rows: u64,
        map: impl Fn(&Block<'_>) -> T + Sync,
        consume: impl FnMut(u64, T) -> Result<()>,
    ) -> Result<()> {
        let blocks: Vec<Range<u64>> = (rows.start..rows.end)
            .step_by(block_rows.max(1) as usize)
            .map(|s| s..(s + block_rows).min(rows.end))
            .collect();
        self.par_block_list(&blocks, map, consume)
    }

    /// [`Table::par_blocks`] over the blocks of `block_rows` rows that hold
    /// at least one row of `set` (other blocks are not read).
    pub fn par_blocks_of(&self, set: &roaring::RoaringBitmap, block_rows: u64) -> Vec<Range<u64>> {
        let n = self.row_count();
        (0..n)
            .step_by(block_rows.max(1) as usize)
            .map(|s| s..(s + block_rows).min(n))
            .filter(|r| set.range_cardinality(r.start as u32..r.end as u32) > 0)
            .collect()
    }

    /// Runs `map` over the given row ranges (in order, non-empty).
    pub fn par_block_list<T: Send>(
        &self,
        blocks: &[Range<u64>],
        map: impl Fn(&Block<'_>) -> T + Sync,
        mut consume: impl FnMut(u64, T) -> Result<()>,
    ) -> Result<()> {
        use rayon::prelude::*;
        let src = self.source()?;
        let h = self.header_rows();
        let window = rayon::current_num_threads().max(1) * 4;
        for w in blocks.chunks(window) {
            if self.progress.cancelled() {
                return Err(Error::Cancelled);
            }
            // Row offsets of each block, taken under one read lock.
            let offsets: Vec<Vec<u64>> = {
                let ix = self.index();
                w.iter()
                    .map(|r| {
                        let mut v: Vec<u64> = (r.start..r.end).map(|i| ix.start(i + h)).collect();
                        v.push(ix.range(r.end - 1 + h).end);
                        v
                    })
                    .collect()
            };
            let out: Vec<Result<T>> = w
                .par_iter()
                .zip(offsets.par_iter())
                .map_init(Vec::new, |buf, (r, offs)| {
                    let base = offs[0];
                    let bytes = src.read(base..offs[offs.len() - 1], buf)?;
                    Ok(map(&Block {
                        first: r.start,
                        base,
                        offs,
                        bytes,
                    }))
                })
                .collect();
            for (r, t) in w.iter().zip(out) {
                consume(r.start, t?)?;
            }
        }
        Ok(())
    }

    pub fn tables(&self) -> &Tables {
        &self.tables
    }

    pub fn access(&self) -> Option<Access> {
        self.source.get().map(Source::access)
    }
}

fn save_columns(w: &mut crate::cache::Writer, cols: &Columns) {
    use crate::time::Order;
    w.u64(cols.cols.len() as u64);
    w.u64(cols.primary_time.map_or(u64::MAX, |c| c as u64));
    w.bitmap(&cols.ragged);
    for c in &cols.cols {
        let (k, o) = match c.kind {
            columns::Kind::Text => (0, 0),
            columns::Kind::Number => (1, 0),
            columns::Kind::Time(o) => (
                2,
                match o {
                    Order::Ymd => 0,
                    Order::Mdy => 1,
                    Order::Dmy => 2,
                },
            ),
        };
        w.u8(k);
        w.u8(o);
        match &c.dict {
            Some(d) => {
                w.u8(1);
                w.u8(d.numeric as u8);
                w.u64(d.values.len() as u64);
                for (v, b) in d.values.iter().zip(&d.bitmaps) {
                    w.bytes(v.as_bytes());
                    w.bitmap(b);
                }
            }
            None => w.u8(0),
        }
        match &c.time {
            Some(t) => {
                w.u8(1);
                w.i64s(&t.ticks);
                match &t.perm {
                    Some(p) => {
                        w.u8(1);
                        w.u32s(p);
                    }
                    None => w.u8(0),
                }
            }
            None => w.u8(0),
        }
    }
}

fn load_columns(
    c: &mut crate::cache::Cursor<'_>,
    ncols: usize,
    index_rows: u64,
) -> Result<Columns> {
    use crate::time::Order;
    let bad = || Error::Cache("columns section".into());
    if c.u64()? as usize != ncols {
        return Err(bad());
    }
    let pt = c.u64()?;
    let primary_time = (pt != u64::MAX).then_some(pt as usize);
    let ragged = c.bitmap()?;
    let mut cols = Vec::with_capacity(ncols);
    for _ in 0..ncols {
        let k = c.u8()?;
        let o = c.u8()?;
        let order = match o {
            0 => Order::Ymd,
            1 => Order::Mdy,
            2 => Order::Dmy,
            _ => return Err(bad()),
        };
        let kind = match k {
            0 => columns::Kind::Text,
            1 => columns::Kind::Number,
            2 => columns::Kind::Time(order),
            _ => return Err(bad()),
        };
        let dict = if c.u8()? == 1 {
            let numeric = c.u8()? == 1;
            let n = c.u64()? as usize;
            if n > columns::DICT_MAX + 1 {
                return Err(bad());
            }
            let mut values = Vec::with_capacity(n);
            let mut bitmaps = Vec::with_capacity(n);
            for _ in 0..n {
                values.push(String::from_utf8_lossy(c.bytes()?).into_owned());
                bitmaps.push(c.bitmap()?);
            }
            Some(columns::Dict {
                values,
                bitmaps,
                numeric,
            })
        } else {
            None
        };
        let time = if c.u8()? == 1 {
            let ticks = c.i64s()?;
            let perm = if c.u8()? == 1 { Some(c.u32s()?) } else { None };
            if ticks.len() as u64 > index_rows
                || perm.as_ref().is_some_and(|p| p.len() != ticks.len())
            {
                return Err(bad());
            }
            Some(columns::TimeIndex { ticks, perm })
        } else {
            None
        };
        cols.push(columns::Column { kind, dict, time });
    }
    Ok(Columns {
        cols,
        ragged,
        primary_time,
    })
}
