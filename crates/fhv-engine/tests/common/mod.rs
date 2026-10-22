//! Independent reference CSV reader for differential tests: plain
//! sequential loops written from the semantics documented in `scan.rs`
//! (no code shared with the engine).
//!
//! 1. Records end at `\n` outside quotes (a quote opens a quoted field only
//!    at the start of a field).
//! 2. Records that are empty or a lone `\r` are skipped (blank lines).
//! 3. Trailing `\r`/`\n` bytes of a record are dropped.
//! 4. Fields split at the delimiter outside quotes; quoted fields lose their
//!    quotes, `""` becomes `"`, text after a closing quote is kept.

#![allow(dead_code)]

pub fn records(data: &[u8], delim: u8) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut cur = Vec::new();
    let (mut field_start, mut in_q, mut after_q) = (true, false, false);
    for &b in data {
        if in_q {
            cur.push(b);
            if b == b'"' {
                in_q = false;
                after_q = true;
            }
            continue;
        }
        if b == b'\n' {
            out.push(std::mem::take(&mut cur));
            field_start = true;
            after_q = false;
            continue;
        }
        cur.push(b);
        if b == b'"' && (field_start || after_q) {
            // Opening quote, or the second half of `""` after a quote.
            in_q = true;
            after_q = false;
            field_start = false;
            continue;
        }
        after_q = false;
        field_start = b == delim;
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out.into_iter()
        .filter(|r| !(r.is_empty() || r == b"\r"))
        .map(|mut r| {
            while matches!(r.last(), Some(b'\r' | b'\n')) {
                r.pop();
            }
            r
        })
        .collect()
}

pub fn fields(rec: &[u8], delim: u8) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut f = Vec::new();
    let (mut start, mut in_q, mut after_q) = (true, false, false);
    for &b in rec {
        if in_q {
            if b == b'"' {
                in_q = false;
                after_q = true;
            } else {
                f.push(b);
            }
            continue;
        }
        if b == delim {
            out.push(std::mem::take(&mut f));
            start = true;
            after_q = false;
            continue;
        }
        if b == b'"' && start {
            in_q = true;
            start = false;
            continue;
        }
        if b == b'"' && after_q {
            f.push(b'"');
            in_q = true;
            after_q = false;
            continue;
        }
        f.push(b);
        start = false;
        after_q = false;
    }
    out.push(f);
    out
}

pub fn reference_rows(data: &[u8], delim: u8) -> Vec<Vec<Vec<u8>>> {
    records(data, delim)
        .iter()
        .map(|r| fields(r, delim))
        .collect()
}
