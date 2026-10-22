//! CSV state machine shared by the row index, the dialect sniffer and the
//! field splitter.
//!
//! Semantics (lenient, like common CSV readers):
//! - a quote opens a quoted field only at the start of a field; elsewhere it
//!   is a literal character, so a stray quote cannot swallow the file;
//! - inside a quoted field, `""` is an escaped quote and newlines are data;
//! - text after a closing quote is appended literally (`"a"b` -> `ab`);
//! - a row ends at `\n` outside quotes; a `\r` before it is dropped.
//!
//! States: `S` field start, `U` unquoted field, `Q` inside quotes, `E` just
//! after a quote inside quotes (closing or first half of `""`).
//!
//! For parallel indexing the file is cut at arbitrary byte offsets, where the
//! entry state is unknown. Each chunk is run for all four entry states at
//! once: the four 2-bit states are packed in one byte and advanced with a
//! single table lookup per input byte. A sequential fix-up then picks, per
//! chunk, the run that matches the exit state of the previous chunk.

pub const S: u8 = 0;
pub const U: u8 = 1;
pub const Q: u8 = 2;
pub const E: u8 = 3;

const C_OTHER: u8 = 0;
const C_QUOTE: u8 = 1;
const C_DELIM: u8 = 2;
const C_NL: u8 = 3;

/// Single-state transition.
const fn step(state: u8, class: u8) -> u8 {
    match (state, class) {
        (S, C_OTHER) => U,
        (S, C_QUOTE) => Q,
        (S, _) => S,
        (U, C_DELIM) | (U, C_NL) => S,
        (U, _) => U,
        (Q, C_QUOTE) => E,
        (Q, _) => Q,
        (E, C_OTHER) => U,
        (E, C_QUOTE) => Q,
        (E, _) => S,
        _ => S,
    }
}

/// Packed state of the four runs that started in S, U, Q, E.
pub const PACKED_START: u8 = S | (U << 2) | (Q << 4) | (E << 6);

pub struct Tables {
    /// Byte -> class.
    class: [u8; 256],
    /// (packed state << 2 | class) -> packed state.
    packed: [u8; 1024],
    /// Packed state -> bitmask of runs (bit i = run started in state i)
    /// for which a newline ends a row (run not inside quotes).
    end_mask: [u8; 256],
    delim: u8,
    /// Quote byte, or the delimiter again when there is no quote char (so
    /// the byte search below stays branch-free).
    quote: u8,
}

impl Tables {
    pub fn new(delim: u8, quote: Option<u8>) -> Self {
        let mut class = [C_OTHER; 256];
        class[delim as usize] = C_DELIM;
        if let Some(q) = quote {
            class[q as usize] = C_QUOTE;
        }
        class[b'\n' as usize] = C_NL;
        let mut packed = [0u8; 1024];
        let mut end_mask = [0u8; 256];
        for v in 0..256usize {
            for c in 0..4u8 {
                let mut out = 0u8;
                for i in 0..4 {
                    let s = (v >> (2 * i)) as u8 & 3;
                    out |= step(s, c) << (2 * i);
                }
                packed[v << 2 | c as usize] = out;
            }
            let mut m = 0u8;
            for i in 0..4 {
                if (v >> (2 * i)) as u8 & 3 != Q {
                    m |= 1 << i;
                }
            }
            end_mask[v] = m;
        }
        Tables {
            class,
            packed,
            end_mask,
            delim,
            quote: quote.unwrap_or(delim),
        }
    }

    #[inline]
    pub fn class(&self, b: u8) -> u8 {
        self.class[b as usize]
    }
}

/// Newlines of a chunk that end a row for at least one entry state.
#[derive(Default)]
pub struct ChunkScan {
    /// Offsets (relative to the chunk start) of candidate row ends.
    pub pos: Vec<u32>,
    /// For each entry in `pos`, the runs (bit = entry state) it ends a row in.
    pub mask: Vec<u8>,
    /// Packed exit state of the four runs.
    pub exit: u8,
}

impl ChunkScan {
    /// Exit state of the run that entered in `entry`.
    pub fn exit_state(&self, entry: u8) -> u8 {
        (self.exit >> (2 * entry)) & 3
    }
}

/// Bit 7 of each byte of `w` is set where the byte equals `b` (exact, no
/// false positives).
#[inline(always)]
fn eq_mask(w: u64, b: u8) -> u64 {
    const LO7: u64 = 0x7F7F_7F7F_7F7F_7F7F;
    let x = w ^ (0x0101_0101_0101_0101 * b as u64);
    !(((x & LO7).wrapping_add(LO7)) | x | LO7)
}

/// Runs the packed machine over `chunk` (< 4 GiB).
///
/// Only delimiters, quotes and newlines change the packed state in a way
/// that matters: a run of other bytes maps it through the "other"
/// transition, which is idempotent (S->U, U->U, Q->Q, E->U). So the loop
/// finds special bytes eight at a time (SWAR) and applies "other" once per
/// gap, instead of one dependent table lookup per byte.
pub fn scan_chunk(t: &Tables, chunk: &[u8]) -> ChunkScan {
    debug_assert!(chunk.len() < u32::MAX as usize);
    let mut out = ChunkScan {
        pos: Vec::with_capacity(chunk.len() / 128),
        mask: Vec::with_capacity(chunk.len() / 128),
        exit: 0,
    };
    let mut v = PACKED_START;
    // Index just past the last byte fed to the machine.
    let mut fed = 0usize;
    let special = |i: usize, v: &mut u8, fed: &mut usize, out: &mut ChunkScan| {
        if i > *fed {
            *v = t.packed[(*v as usize) << 2 | C_OTHER as usize];
        }
        let c = t.class[chunk[i] as usize];
        if c == C_NL {
            let m = t.end_mask[*v as usize];
            if m != 0 {
                out.pos.push(i as u32);
                out.mask.push(m);
            }
        }
        *v = t.packed[(*v as usize) << 2 | c as usize];
        *fed = i + 1;
    };
    let mut words = chunk.chunks_exact(8);
    let mut base = 0usize;
    for w in &mut words {
        let w = u64::from_le_bytes(w.try_into().unwrap_or([0; 8]));
        let mut m = eq_mask(w, b'\n') | eq_mask(w, t.delim) | eq_mask(w, t.quote);
        while m != 0 {
            let j = (m.trailing_zeros() / 8) as usize;
            special(base + j, &mut v, &mut fed, &mut out);
            m &= m - 1;
        }
        base += 8;
    }
    for (j, &b) in words.remainder().iter().enumerate() {
        if b == b'\n' || b == t.delim || b == t.quote {
            special(base + j, &mut v, &mut fed, &mut out);
        }
    }
    if chunk.len() > fed {
        v = t.packed[(v as usize) << 2 | C_OTHER as usize];
    }
    out.exit = v;
    out
}

/// Byte-at-a-time version of [`scan_chunk`] (reference for tests).
pub fn scan_chunk_simple(t: &Tables, chunk: &[u8]) -> ChunkScan {
    let mut out = ChunkScan::default();
    let mut v = PACKED_START;
    for (i, &b) in chunk.iter().enumerate() {
        let c = t.class[b as usize];
        if c == C_NL {
            let m = t.end_mask[v as usize];
            if m != 0 {
                out.pos.push(i as u32);
                out.mask.push(m);
            }
        }
        v = t.packed[(v as usize) << 2 | c as usize];
    }
    out.exit = v;
    out
}

/// One field of a row: byte range in the row and how to decode it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub start: u32,
    pub end: u32,
    /// Needs unquoting (`"` wrappers and/or `""` escapes).
    pub quoted: bool,
}

/// Splits one row (without its line terminator) into fields.
/// Returns the number of fields (`out` holds them).
pub fn split_row(t: &Tables, row: &[u8], out: &mut Vec<Span>) -> usize {
    out.clear();
    let mut start = 0usize;
    let mut quoted = false;
    let mut state = S;
    for (i, &b) in row.iter().enumerate() {
        let c = t.class[b as usize];
        if c == C_DELIM && state != Q {
            out.push(Span {
                start: start as u32,
                end: i as u32,
                quoted,
            });
            start = i + 1;
            quoted = false;
            state = S;
            continue;
        }
        if c == C_QUOTE && state == S {
            quoted = true;
        }
        // Newlines inside quoted fields are data; outside quotes a row never
        // contains one (the index split there), so treat them as text.
        let c = if c == C_NL { C_OTHER } else { c };
        state = step(state, c);
    }
    out.push(Span {
        start: start as u32,
        end: row.len() as u32,
        quoted,
    });
    out.len()
}

/// Decodes a quoted field: drops the opening quote, the closing quote and
/// un-doubles `""`; text after the closing quote is kept literally.
pub fn unquote(raw: &[u8], quote: u8, out: &mut Vec<u8>) {
    out.clear();
    let mut i = 1; // skip opening quote
    let mut inside = true;
    while i < raw.len() {
        let b = raw[i];
        if inside && b == quote {
            if raw.get(i + 1) == Some(&quote) {
                out.push(quote);
                i += 2;
                continue;
            }
            inside = false;
            i += 1;
            continue;
        }
        out.push(b);
        i += 1;
    }
}

/// Strips the line terminator(s) at the end of a row slice.
#[inline]
pub fn trim_eol(row: &[u8]) -> &[u8] {
    let mut end = row.len();
    while end > 0 && (row[end - 1] == b'\n' || row[end - 1] == b'\r') {
        end -= 1;
    }
    &row[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields(row: &str) -> Vec<String> {
        let t = Tables::new(b',', Some(b'"'));
        let mut spans = Vec::new();
        split_row(&t, row.as_bytes(), &mut spans);
        let mut buf = Vec::new();
        spans
            .iter()
            .map(|s| {
                let raw = &row.as_bytes()[s.start as usize..s.end as usize];
                if s.quoted {
                    unquote(raw, b'"', &mut buf);
                    String::from_utf8(buf.clone()).unwrap()
                } else {
                    String::from_utf8(raw.to_vec()).unwrap()
                }
            })
            .collect()
    }

    #[test]
    fn split_basic() {
        assert_eq!(fields("a,b,c"), ["a", "b", "c"]);
        assert_eq!(fields(""), [""]);
        assert_eq!(fields(",,"), ["", "", ""]);
        assert_eq!(fields("\"a,b\",c"), ["a,b", "c"]);
        assert_eq!(fields("\"he said \"\"hi\"\"\",x"), ["he said \"hi\"", "x"]);
        assert_eq!(fields("a\"b,c"), ["a\"b", "c"]);
        assert_eq!(fields("\"x\"y,2"), ["xy", "2"]);
        assert_eq!(fields("\"multi\nline\",z"), ["multi\nline", "z"]);
        assert_eq!(fields("\"\",\"\"\"\""), ["", "\""]);
        assert_eq!(fields("\"unterminated,x"), ["unterminated,x"]);
    }

    /// Reference: sequential single-state run from S.
    fn row_ends_seq(t: &Tables, data: &[u8]) -> Vec<usize> {
        let mut s = S;
        let mut ends = Vec::new();
        for (i, &b) in data.iter().enumerate() {
            let c = t.class(b);
            if c == C_NL && s != Q {
                ends.push(i);
            }
            s = step(s, c);
        }
        ends
    }

    #[test]
    fn swar_scan_matches_simple() {
        let mut x = 0x1234_5678u64;
        for len in 0..300 {
            for (d, q) in [(b',', Some(b'"')), (b';', None)] {
                let t = Tables::new(d, q);
                let data: Vec<u8> = (0..len)
                    .map(|_| {
                        x ^= x << 13;
                        x ^= x >> 7;
                        x ^= x << 17;
                        b"ab,;\"\n\r\x00\xff"[(x % 9) as usize]
                    })
                    .collect();
                let a = scan_chunk(&t, &data);
                let b = scan_chunk_simple(&t, &data);
                assert_eq!(
                    (a.pos, a.mask, a.exit),
                    (b.pos, b.mask, b.exit),
                    "len {len}"
                );
            }
        }
    }

    #[test]
    fn packed_matches_sequential_for_any_split() {
        let t = Tables::new(b',', Some(b'"'));
        let data = b"a,\"b\nc\",d\n\"x\"\"y\",z\nq\"r,s\n\"\n\"\n,,\n\"open\nno,close\n";
        let want = row_ends_seq(&t, data);
        for cut in 0..data.len() {
            let (a, b) = data.split_at(cut);
            let sa = scan_chunk(&t, a);
            let sb = scan_chunk(&t, b);
            let mut got: Vec<usize> = sa
                .pos
                .iter()
                .zip(&sa.mask)
                .filter(|(_, m)| *m & 1 != 0)
                .map(|(p, _)| *p as usize)
                .collect();
            let entry = sa.exit_state(S);
            got.extend(
                sb.pos
                    .iter()
                    .zip(&sb.mask)
                    .filter(|(_, m)| *m & (1 << entry) != 0)
                    .map(|(p, _)| *p as usize + cut),
            );
            assert_eq!(got, want, "cut at {cut}");
        }
    }
}
