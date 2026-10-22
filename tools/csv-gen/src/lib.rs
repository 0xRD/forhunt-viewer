//! Seeded synthetic CSV generator (library part, also used by engine tests).

pub mod edge;
pub mod layouts;
pub mod rng;
pub mod time;

use std::io::Write;

use rayon::prelude::*;

pub use layouts::{BLOCK_ROWS, Layout};

pub struct Spec {
    pub layout: Layout,
    pub seed: u64,
    /// Stop after this many rows...
    pub rows: Option<u64>,
    /// ...or once this many bytes are written (whole blocks).
    pub bytes: Option<u64>,
    pub delim: u8,
    pub crlf: bool,
}

pub struct Summary {
    pub rows: u64,
    pub bytes: u64,
}

fn block(spec: &Spec, start: u64, n: u64) -> Vec<u8> {
    let mut out = layouts::Out::new(spec.delim);
    spec.layout.block(spec.seed, start, n, &mut out);
    if spec.crlf { crlf(out.buf) } else { out.buf }
}

/// Converts row-ending LFs to CRLF (embedded LFs inside quotes stay LF).
fn crlf(buf: Vec<u8>) -> Vec<u8> {
    let mut out = Vec::with_capacity(buf.len() + buf.len() / 32);
    let mut quoted = false;
    for b in buf {
        if b == b'"' {
            quoted = !quoted;
        }
        if b == b'\n' && !quoted {
            out.push(b'\r');
        }
        out.push(b);
    }
    out
}

/// Writes the header and rows to `w`. Output depends only on `spec`, not on
/// the thread count.
pub fn generate(spec: &Spec, w: &mut impl Write) -> std::io::Result<Summary> {
    let mut head = layouts::Out::new(spec.delim);
    head.header(spec.layout.header());
    let head = if spec.crlf { crlf(head.buf) } else { head.buf };
    w.write_all(&head)?;
    let mut bytes = head.len() as u64;
    let mut rows = 0u64;
    let max_rows = spec.rows.unwrap_or(u64::MAX);
    let max_bytes = spec.bytes.unwrap_or(u64::MAX);
    let batch = (rayon::current_num_threads() * 4) as u64;
    let mut next_block = 0u64;
    while rows < max_rows && bytes < max_bytes {
        let blocks: Vec<(u64, u64)> = (next_block..next_block + batch)
            .map(|b| {
                let start = b * BLOCK_ROWS;
                (start, BLOCK_ROWS.min(max_rows.saturating_sub(start)))
            })
            .filter(|&(_, n)| n > 0)
            .collect();
        if blocks.is_empty() {
            break;
        }
        next_block += batch;
        let bufs: Vec<Vec<u8>> = blocks
            .par_iter()
            .map(|&(start, n)| block(spec, start, n))
            .collect();
        for ((_, n), buf) in blocks.iter().zip(bufs) {
            if rows >= max_rows || bytes >= max_bytes {
                break;
            }
            w.write_all(&buf)?;
            bytes += buf.len() as u64;
            rows += n;
        }
    }
    w.flush()?;
    Ok(Summary { rows, bytes })
}
