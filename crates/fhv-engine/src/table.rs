//! A CSV file opened for viewing: encoding, dialect, header, row index and
//! row access.

use std::borrow::Cow;
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock, RwLockReadGuard};

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
}

impl OpenOptions {
    pub fn new(cache_dir: impl Into<PathBuf>) -> Self {
        OpenOptions {
            cache_dir: cache_dir.into(),
            access: Access::Mmap,
            dialect: None,
            encoding: None,
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
}

/// Decoded cells of a range of rows, for display.
#[derive(Default)]
pub struct Rows {
    pub first: u64,
    pub ncols: usize,
    cells: Vec<String>,
    /// Number of fields each row really has (ragged rows differ from ncols).
    pub nfields: Vec<u32>,
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

    /// Cell of display row `row` (absolute data row number).
    pub fn cell(&self, row: u64, col: usize) -> &str {
        let k = (row.wrapping_sub(self.first)) as usize;
        self.cells
            .get(k * self.ncols + col)
            .map(String::as_str)
            .unwrap_or("")
    }

    pub fn contains(&self, row: u64) -> bool {
        row >= self.first && row < self.first + self.len() as u64
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

        let table = Table {
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
        if self.source.get().is_none() {
            let out = self.transcode()?;
            let src = Source::open(&out, self.opts.access, 0)?;
            let _ = self.source.set(src);
        }
        let src = self.source()?;
        self.progress
            .set_phase(Phase::Indexing, src.len - src.data_start);
        index::build_with(src, &self.tables, &self.index, &self.progress, chunk)
    }

    fn transcode(&self) -> Result<PathBuf> {
        let dir = self.opts.cache_dir.join("transcoded");
        fs::create_dir_all(&dir)?;
        let out = dir.join(format!("{}.utf8.csv", file_key(&self.path)?));
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

    pub fn index(&self) -> RwLockReadGuard<'_, RowIndex> {
        self.index.read().unwrap_or_else(|e| e.into_inner())
    }

    fn header_rows(&self) -> u64 {
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
            ..
        } = out;
        let bytes = src.read(first..last, raw)?;
        for k in 0..n {
            let r = ix.range(range.start + h + k as u64);
            let row = trim_eol(&bytes[(r.start - first) as usize..(r.end - first) as usize]);
            let nf = split_row(&self.tables, row, spans);
            nfields.push(nf as u32);
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

    pub fn tables(&self) -> &Tables {
        &self.tables
    }

    pub fn access(&self) -> Option<Access> {
        self.source.get().map(Source::access)
    }
}
