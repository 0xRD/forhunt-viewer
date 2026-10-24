//! Encoding and CSV dialect detection from the head of a file.

use serde::{Deserialize, Serialize};

use crate::scan::{Span, Tables, split_row, trim_eol};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Encoding {
    /// UTF-8 (invalid sequences are shown as U+FFFD, bytes are kept).
    Utf8,
    /// UTF-8 with a byte-order mark (skipped).
    Utf8Bom,
    /// UTF-16 little endian (with or without BOM); transcoded to UTF-8.
    Utf16Le,
    /// UTF-16 big endian; transcoded to UTF-8.
    Utf16Be,
    /// Windows-1252 / Latin-1 text (not valid UTF-8); transcoded to UTF-8.
    Windows1252,
}

impl Encoding {
    pub fn needs_transcoding(self) -> bool {
        matches!(
            self,
            Encoding::Utf16Le | Encoding::Utf16Be | Encoding::Windows1252
        )
    }

    pub fn name(self) -> &'static str {
        match self {
            Encoding::Utf8 => "UTF-8",
            Encoding::Utf8Bom => "UTF-8 (BOM)",
            Encoding::Utf16Le => "UTF-16LE",
            Encoding::Utf16Be => "UTF-16BE",
            Encoding::Windows1252 => "Windows-1252",
        }
    }
}

/// Detects the encoding from the first bytes. Returns it and the length of
/// the byte-order mark to skip.
pub fn detect_encoding(head: &[u8]) -> (Encoding, usize) {
    if head.starts_with(b"\xEF\xBB\xBF") {
        return (Encoding::Utf8Bom, 3);
    }
    if head.starts_with(b"\xFF\xFE") {
        return (Encoding::Utf16Le, 2);
    }
    if head.starts_with(b"\xFE\xFF") {
        return (Encoding::Utf16Be, 2);
    }
    // UTF-16 without BOM: ASCII text has a NUL in every other byte.
    let probe = &head[..head.len().min(4096) & !1];
    if probe.len() >= 8 {
        let even = probe.iter().step_by(2).filter(|&&b| b == 0).count();
        let odd = probe.iter().skip(1).step_by(2).filter(|&&b| b == 0).count();
        let half = probe.len() / 2;
        if odd * 10 > half * 7 && even * 10 < half {
            return (Encoding::Utf16Le, 0);
        }
        if even * 10 > half * 7 && odd * 10 < half {
            return (Encoding::Utf16Be, 0);
        }
    }
    // Not UTF-8 and no valid multi-byte sequence at all: a legacy 8-bit
    // code page (Windows-1252 is a superset of Latin-1's printable range).
    let sample = &head[..head.len().min(1 << 20)];
    let mut valid_multibyte = false;
    let mut invalid = false;
    let mut rest = sample;
    loop {
        match std::str::from_utf8(rest) {
            Ok(s) => {
                valid_multibyte |= s.bytes().any(|b| b >= 0x80);
                break;
            }
            Err(e) => {
                let ok = &rest[..e.valid_up_to()];
                valid_multibyte |= ok.iter().any(|&b| b >= 0x80);
                match e.error_len() {
                    Some(n) => {
                        invalid = true;
                        rest = &rest[e.valid_up_to() + n..];
                    }
                    // Truncated sequence at the end of the sample.
                    None => break,
                }
            }
        }
    }
    if invalid && !valid_multibyte {
        return (Encoding::Windows1252, 0);
    }
    (Encoding::Utf8, 0)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Dialect {
    pub delim: u8,
    pub quote: Option<u8>,
    /// The first row holds column names.
    pub header: bool,
    /// Rows end with CRLF (informational; both are always accepted).
    pub crlf: bool,
}

impl Default for Dialect {
    fn default() -> Self {
        Dialect {
            delim: b',',
            quote: Some(b'"'),
            header: true,
            crlf: false,
        }
    }
}

const CANDIDATES: &[u8] = b",\t;|";
const SAMPLE_ROWS: usize = 200;

/// Splits the sample into rows (quote-aware) and returns field counts and
/// the rows themselves. `whole` = the sample is the entire file.
fn sample_rows<'a>(t: &Tables, sample: &'a [u8], whole: bool) -> Vec<&'a [u8]> {
    let mut rows = Vec::new();
    let scan = crate::scan::scan_chunk(t, sample);
    let mut start = 0usize;
    for (&p, &m) in scan.pos.iter().zip(&scan.mask) {
        if m & 1 == 0 {
            continue;
        }
        let row = trim_eol(&sample[start..p as usize]);
        if !row.is_empty() {
            rows.push(row);
        }
        start = p as usize + 1;
        if rows.len() >= SAMPLE_ROWS {
            return rows;
        }
    }
    if whole {
        let row = trim_eol(&sample[start..]);
        if !row.is_empty() {
            rows.push(row);
        }
    }
    rows
}

/// Detects delimiter, header and line endings from a UTF-8 sample of the
/// head of the file.
pub fn sniff(sample: &[u8], whole: bool) -> Dialect {
    let quote = Some(b'"');
    let mut best: Option<(u32, usize, u8)> = None; // (consistency per mille, fields, delim)
    let mut spans: Vec<Span> = Vec::new();
    for &d in CANDIDATES {
        let t = Tables::new(d, quote);
        let rows = sample_rows(&t, sample, whole);
        if rows.is_empty() {
            continue;
        }
        let counts: Vec<usize> = rows.iter().map(|r| split_row(&t, r, &mut spans)).collect();
        let mut sorted = counts.clone();
        sorted.sort_unstable();
        // Mode of the field counts.
        let (mut mode, mut mode_n, mut i) = (0, 0, 0);
        while i < sorted.len() {
            let j = sorted[i..].iter().take_while(|&&c| c == sorted[i]).count();
            if j > mode_n || (j == mode_n && sorted[i] > mode) {
                mode = sorted[i];
                mode_n = j;
            }
            i += j;
        }
        if mode < 2 {
            continue;
        }
        let consistency = (mode_n * 1000 / counts.len()) as u32;
        let cand = (consistency, mode, d);
        if best.is_none_or(|b| (cand.0, cand.1) > (b.0, b.1)) {
            best = Some(cand);
        }
    }
    let delim = best.map(|b| b.2).unwrap_or(b',');
    let t = Tables::new(delim, quote);
    let rows = sample_rows(&t, sample, whole);
    let header = match rows.first() {
        Some(r) => {
            split_row(&t, r, &mut spans);
            !spans.iter().any(|s| {
                let cell = &r[s.start as usize..s.end as usize];
                looks_like_data(cell)
            })
        }
        None => true,
    };
    let crlf = memchr::memchr(b'\n', sample).is_some_and(|p| p > 0 && sample[p - 1] == b'\r');
    Dialect {
        delim,
        quote,
        header,
        crlf,
    }
}

/// Whether a detected header was a guess: the first row was taken as
/// column names only because none of its cells is a number or a date, and
/// no later sampled row has one either, so nothing tells names from data.
pub fn header_guessed(sample: &[u8], whole: bool, d: &Dialect) -> bool {
    if !d.header {
        return false;
    }
    let t = Tables::new(d.delim, d.quote);
    let mut spans: Vec<Span> = Vec::new();
    !sample_rows(&t, sample, whole).iter().skip(1).any(|r| {
        split_row(&t, r, &mut spans);
        spans
            .iter()
            .any(|s| looks_like_data(&r[s.start as usize..s.end as usize]))
    })
}

/// A cell that is a number or a date-time is data, not a column name.
fn looks_like_data(cell: &[u8]) -> bool {
    let s = match std::str::from_utf8(cell) {
        Ok(s) => s.trim().trim_matches('"'),
        Err(_) => return false,
    };
    if s.is_empty() {
        return false;
    }
    if s.parse::<f64>().is_ok() {
        return true;
    }
    crate::time::parse_any(s.as_bytes()).is_some()
}

/// Windows-1252 bytes 0x80..=0x9F (others map to the same code point).
const CP1252_HIGH: [char; 32] = [
    '€', '\u{81}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{8D}', 'Ž', '\u{8F}',
    '\u{90}', '‘', '’', '“', '”', '•', '–', '—', '˜', '™', 'š', '›', 'œ', '\u{9D}', 'ž', 'Ÿ',
];

/// Transcodes a block of `enc` text to UTF-8, appending to `out`, and
/// returns how many input bytes were consumed. For UTF-16, an odd trailing
/// byte or a high surrogate cut at the end of the block is left unconsumed
/// so the caller can prepend it to the next block.
pub fn transcode(enc: Encoding, input: &[u8], out: &mut Vec<u8>) -> usize {
    match enc {
        Encoding::Windows1252 => {
            for &b in input {
                let c = match b {
                    0x80..=0x9F => CP1252_HIGH[(b - 0x80) as usize],
                    _ => b as char,
                };
                let mut buf = [0u8; 4];
                out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            }
            input.len()
        }
        Encoding::Utf16Le | Encoding::Utf16Be => {
            let n = input.len() & !1;
            let mut units: Vec<u16> = input[..n]
                .chunks_exact(2)
                .map(|c| {
                    if enc == Encoding::Utf16Le {
                        u16::from_le_bytes([c[0], c[1]])
                    } else {
                        u16::from_be_bytes([c[0], c[1]])
                    }
                })
                .collect();
            // Keep a trailing high surrogate for the next block.
            let mut used = n;
            if let Some(&last) = units.last()
                && (0xD800..0xDC00).contains(&last)
            {
                units.pop();
                used -= 2;
            }
            let mut buf = [0u8; 4];
            for c in char::decode_utf16(units) {
                let c = c.unwrap_or(char::REPLACEMENT_CHARACTER);
                out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            }
            used
        }
        Encoding::Utf8 | Encoding::Utf8Bom => {
            out.extend_from_slice(input);
            input.len()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_guess() {
        let d = sniff(b"user,host\nalice,pc1\nbob,pc2\n", true);
        assert!(d.header && header_guessed(b"user,host\nalice,pc1\nbob,pc2\n", true, &d));
        let s = b"user,count\nalice,3\nbob,4\n";
        let d = sniff(s, true);
        assert!(d.header && !header_guessed(s, true, &d));
        let s = b"alice,3\nbob,4\n";
        assert!(!header_guessed(s, true, &sniff(s, true)));
    }

    #[test]
    fn encodings() {
        assert_eq!(detect_encoding(b"\xEF\xBB\xBFa,b"), (Encoding::Utf8Bom, 3));
        assert_eq!(detect_encoding(b"\xFF\xFEa\0,\0"), (Encoding::Utf16Le, 2));
        assert_eq!(detect_encoding(b"\xFE\xFF\0a\0,"), (Encoding::Utf16Be, 2));
        assert_eq!(
            detect_encoding(b"a\0,\0b\0\n\0c\0,\0d\0\n\0"),
            (Encoding::Utf16Le, 0)
        );
        assert_eq!(detect_encoding("a,é\n".as_bytes()), (Encoding::Utf8, 0));
        assert_eq!(
            detect_encoding(b"a,\xE9t\xE9\n"),
            (Encoding::Windows1252, 0)
        );
        // Mixed valid and invalid: UTF-8, shown lossily.
        assert_eq!(detect_encoding("é,\u{0}\n".as_bytes()), (Encoding::Utf8, 0));
        assert_eq!(detect_encoding(b"a,\x00b\n"), (Encoding::Utf8, 0));
    }

    #[test]
    fn transcoding() {
        let mut out = Vec::new();
        transcode(Encoding::Windows1252, b"caf\xE9 \x80", &mut out);
        assert_eq!(String::from_utf8(out).unwrap(), "café €");
        let mut out = Vec::new();
        let le: Vec<u8> = "a😀b"
            .encode_utf16()
            .flat_map(|u| u.to_le_bytes())
            .collect();
        // Cut inside the surrogate pair.
        let used = transcode(Encoding::Utf16Le, &le[..4], &mut out);
        assert_eq!(used, 2);
        transcode(Encoding::Utf16Le, &le[used..], &mut out);
        assert_eq!(String::from_utf8(out).unwrap(), "a😀b");
    }

    #[test]
    fn delimiters_and_header() {
        let d = sniff(
            b"id;amount;label\n1;3,14;a\n2;2,72;\"b;c\"\n3;1,41;d\n",
            false,
        );
        assert_eq!(d.delim, b';');
        assert!(d.header);
        assert_eq!(sniff(b"a\tb\n1\t2\n", true).delim, b'\t');
        assert_eq!(sniff(b"a|b|c\n1|2|3\n", true).delim, b'|');
        assert_eq!(sniff(b"value\nalpha\nbeta\n", true).delim, b',');
        let d = sniff(b"1,2024-03-11 10:00:00,x\n2,2024-03-11 10:00:01,y\n", true);
        assert!(!d.header);
        assert!(sniff(b"a,b\r\n1,2\r\n", true).crlf);
    }
}
