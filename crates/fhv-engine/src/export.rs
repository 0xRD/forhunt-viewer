//! Export of a view (filtered / tagged rows, display order) to CSV or JSONL.
//!
//! Output is UTF-8 with LF line ends whatever the input encoding; CSV uses
//! commas and RFC 4180 quoting; JSONL writes one object per row with the
//! columns in the given order. Only the chosen columns are written.

use std::io::Write;

use rayon::prelude::*;
use roaring::RoaringBitmap;
use serde::{Deserialize, Serialize};

use crate::scan::{Span, split_row, trim_eol};
use crate::table::{Table, decode_field};
use crate::{Error, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Format {
    Csv,
    Jsonl,
}

fn csv_cell(out: &mut Vec<u8>, s: &str) {
    if s.bytes().any(|b| matches!(b, b',' | b'"' | b'\n' | b'\r')) {
        out.push(b'"');
        for b in s.bytes() {
            if b == b'"' {
                out.push(b'"');
            }
            out.push(b);
        }
        out.push(b'"');
    } else {
        out.extend_from_slice(s.as_bytes());
    }
}

fn json_str(out: &mut Vec<u8>, s: &str) {
    // serde_json escaping (control characters, quotes, backslashes).
    let _ = serde_json::to_writer(&mut *out, s);
}

struct Encoder<'a> {
    table: &'a Table,
    cols: &'a [usize],
    names: Vec<String>,
    fmt: Format,
}

impl Encoder<'_> {
    fn header(&self, out: &mut Vec<u8>) {
        if self.fmt == Format::Csv {
            for (i, n) in self.names.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                csv_cell(out, n);
            }
            out.push(b'\n');
        }
    }

    fn row(&self, raw: &[u8], spans: &mut Vec<Span>, tmp: &mut Vec<u8>, out: &mut Vec<u8>) {
        split_row(self.table.tables(), raw, spans);
        let quote = self.table.dialect.quote;
        if self.fmt == Format::Jsonl {
            out.push(b'{');
        }
        for (i, &c) in self.cols.iter().enumerate() {
            let v = spans
                .get(c)
                .map(|s| decode_field(raw, s, quote, tmp))
                .unwrap_or_default();
            match self.fmt {
                Format::Csv => {
                    if i > 0 {
                        out.push(b',');
                    }
                    csv_cell(out, &v);
                }
                Format::Jsonl => {
                    if i > 0 {
                        out.push(b',');
                    }
                    json_str(out, &self.names[i]);
                    out.push(b':');
                    json_str(out, &v);
                }
            }
        }
        if self.fmt == Format::Jsonl {
            out.push(b'}');
        }
        out.push(b'\n');
    }
}

/// Writes `rows` (data rows, in this order) to `w`. Returns the number of
/// rows written. Cancellable through `table.progress`.
pub fn export(
    table: &Table,
    rows: &[u32],
    cols: &[usize],
    fmt: Format,
    w: &mut dyn Write,
) -> Result<u64> {
    let enc = Encoder {
        table,
        cols,
        names: cols
            .iter()
            .map(|&c| table.columns.get(c).cloned().unwrap_or_default())
            .collect(),
        fmt,
    };
    let mut head = Vec::new();
    enc.header(&mut head);
    w.write_all(&head)?;
    let ascending = rows.windows(2).all(|p| p[0] < p[1]);
    let mut written = 0u64;
    if ascending {
        // Sequential block reads over the file.
        let set: RoaringBitmap = rows.iter().copied().collect();
        let blocks = table.par_blocks_of(&set, 32_768);
        table.par_block_list(
            &blocks,
            |b| {
                let mut out = Vec::new();
                let (mut spans, mut tmp) = (Vec::new(), Vec::new());
                let r0 = b.first as u32;
                let mut n = 0u64;
                for r in set.range(r0..r0 + b.len() as u32) {
                    enc.row(b.row((r - r0) as usize), &mut spans, &mut tmp, &mut out);
                    n += 1;
                }
                (out, n)
            },
            |_, (out, n)| {
                w.write_all(&out)?;
                written += n;
                table
                    .progress
                    .rows
                    .store(written, std::sync::atomic::Ordering::Relaxed);
                Ok(())
            },
        )?;
    } else {
        // Display order: rows read individually, batches encoded in parallel.
        for batch in rows.chunks(16_384) {
            if table.progress.cancelled() {
                return Err(Error::Cancelled);
            }
            let parts: Vec<Result<Vec<u8>>> = batch
                .par_chunks(512)
                .map(|ch| {
                    let (mut buf, mut spans, mut tmp, mut out) =
                        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
                    for &r in ch {
                        let raw = table.raw_row(r as u64, &mut buf)?;
                        enc.row(trim_eol(raw), &mut spans, &mut tmp, &mut out);
                    }
                    Ok(out)
                })
                .collect();
            for p in parts {
                w.write_all(&p?)?;
            }
            written += batch.len() as u64;
            table
                .progress
                .rows
                .store(written, std::sync::atomic::Ordering::Relaxed);
        }
    }
    w.flush()?;
    Ok(written)
}
