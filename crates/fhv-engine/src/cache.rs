//! Index cache: the row index (and later column indexes) of a file, saved in
//! the cache directory so reopening the same file skips the full pass.
//!
//! One file per input, named after `table::file_key` (canonical path, size,
//! mtime, sampled content hash) plus the parsing options. Any change to the
//! input gives a new key. Format: magic, version, tagged sections, then a
//! BLAKE3 hash of everything before it; a file that fails any check is
//! ignored and rebuilt.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::{Error, Result};

const MAGIC: &[u8; 8] = b"FHVIDX\0\x01";
/// Bump when any section layout changes.
pub const VERSION: u32 = 2;

#[derive(Default)]
pub struct Writer {
    buf: Vec<u8>,
    sections: u32,
    open: Option<usize>,
}

impl Writer {
    pub fn new() -> Self {
        let mut w = Writer::default();
        w.buf.extend_from_slice(MAGIC);
        w.buf.extend_from_slice(&VERSION.to_le_bytes());
        w.buf.extend_from_slice(&0u32.to_le_bytes()); // section count, patched
        w
    }

    pub fn begin(&mut self, tag: &[u8; 8]) {
        self.end_section();
        self.buf.extend_from_slice(tag);
        self.open = Some(self.buf.len());
        self.buf.extend_from_slice(&0u64.to_le_bytes());
        self.sections += 1;
    }

    fn end_section(&mut self) {
        if let Some(at) = self.open.take() {
            let len = (self.buf.len() - at - 8) as u64;
            self.buf[at..at + 8].copy_from_slice(&len.to_le_bytes());
        }
    }

    pub fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }
    pub fn u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    pub fn u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    pub fn i64(&mut self, v: i64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    pub fn bytes(&mut self, b: &[u8]) {
        self.u64(b.len() as u64);
        self.buf.extend_from_slice(b);
    }
    pub fn u32s(&mut self, v: &[u32]) {
        self.u64(v.len() as u64);
        self.buf.reserve(v.len() * 4);
        for x in v {
            self.buf.extend_from_slice(&x.to_le_bytes());
        }
    }
    pub fn u64s(&mut self, v: &[u64]) {
        self.u64(v.len() as u64);
        self.buf.reserve(v.len() * 8);
        for x in v {
            self.buf.extend_from_slice(&x.to_le_bytes());
        }
    }
    pub fn i64s(&mut self, v: &[i64]) {
        self.u64(v.len() as u64);
        self.buf.reserve(v.len() * 8);
        for x in v {
            self.buf.extend_from_slice(&x.to_le_bytes());
        }
    }
    pub fn bitmap(&mut self, b: &roaring::RoaringBitmap) {
        let mut v = Vec::with_capacity(b.serialized_size());
        let _ = b.serialize_into(&mut v);
        self.bytes(&v);
    }

    /// Writes atomically (temp file + rename).
    pub fn finish(mut self, path: &Path) -> Result<()> {
        self.end_section();
        self.buf[12..16].copy_from_slice(&self.sections.to_le_bytes());
        let hash = blake3::hash(&self.buf);
        self.buf.extend_from_slice(hash.as_bytes());
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension(format!("tmp{}", std::process::id()));
        let mut f = fs::File::create(&tmp)?;
        f.write_all(&self.buf)?;
        f.sync_all().ok();
        drop(f);
        fs::rename(&tmp, path)?;
        Ok(())
    }
}

pub struct Reader {
    buf: Vec<u8>,
    /// (tag, start, end) of each section payload.
    sections: Vec<([u8; 8], usize, usize)>,
}

fn bad(what: &str) -> Error {
    Error::Cache(what.to_string())
}

impl Reader {
    pub fn open(path: &Path) -> Result<Reader> {
        let buf = fs::read(path)?;
        if buf.len() < 16 + 32 || &buf[..8] != MAGIC {
            return Err(bad("not an index cache"));
        }
        let (body, hash) = buf.split_at(buf.len() - 32);
        if blake3::hash(body).as_bytes() != hash {
            return Err(bad("checksum mismatch"));
        }
        if u32::from_le_bytes(body[8..12].try_into().unwrap_or_default()) != VERSION {
            return Err(bad("version"));
        }
        let n = u32::from_le_bytes(body[12..16].try_into().unwrap_or_default());
        let mut sections = Vec::new();
        let mut at = 16usize;
        for _ in 0..n {
            if at + 16 > body.len() {
                return Err(bad("truncated section header"));
            }
            let tag: [u8; 8] = body[at..at + 8].try_into().unwrap_or_default();
            let len =
                u64::from_le_bytes(body[at + 8..at + 16].try_into().unwrap_or_default()) as usize;
            let start = at + 16;
            let end = start
                .checked_add(len)
                .filter(|&e| e <= body.len())
                .ok_or_else(|| bad("section length"))?;
            sections.push((tag, start, end));
            at = end;
        }
        Ok(Reader { buf, sections })
    }

    pub fn section(&self, tag: &[u8; 8]) -> Result<Cursor<'_>> {
        self.sections
            .iter()
            .find(|s| &s.0 == tag)
            .map(|&(_, s, e)| Cursor {
                b: &self.buf[s..e],
                i: 0,
            })
            .ok_or_else(|| bad("missing section"))
    }
}

pub struct Cursor<'a> {
    b: &'a [u8],
    i: usize,
}

impl Cursor<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8]> {
        let end = self
            .i
            .checked_add(n)
            .filter(|&e| e <= self.b.len())
            .ok_or_else(|| bad("truncated"))?;
        let s = &self.b[self.i..end];
        self.i = end;
        Ok(s)
    }
    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    pub fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(
            self.take(4)?.try_into().unwrap_or_default(),
        ))
    }
    pub fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(
            self.take(8)?.try_into().unwrap_or_default(),
        ))
    }
    pub fn i64(&mut self) -> Result<i64> {
        Ok(i64::from_le_bytes(
            self.take(8)?.try_into().unwrap_or_default(),
        ))
    }
    fn len(&mut self, elem: usize) -> Result<usize> {
        let n = self.u64()? as usize;
        if n.checked_mul(elem)
            .is_none_or(|b| b > self.b.len() - self.i)
        {
            return Err(bad("array length"));
        }
        Ok(n)
    }
    pub fn bytes(&mut self) -> Result<&[u8]> {
        let n = self.len(1)?;
        self.take(n)
    }
    pub fn u32s(&mut self) -> Result<Vec<u32>> {
        let n = self.len(4)?;
        Ok(self
            .take(n * 4)?
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect())
    }
    pub fn u64s(&mut self) -> Result<Vec<u64>> {
        let n = self.len(8)?;
        Ok(self
            .take(n * 8)?
            .chunks_exact(8)
            .map(|c| u64::from_le_bytes(c.try_into().unwrap_or_default()))
            .collect())
    }
    pub fn i64s(&mut self) -> Result<Vec<i64>> {
        Ok(self.u64s()?.into_iter().map(|v| v as i64).collect())
    }
    pub fn bitmap(&mut self) -> Result<roaring::RoaringBitmap> {
        let b = self.bytes()?;
        roaring::RoaringBitmap::deserialize_from(b).map_err(|_| bad("bitmap"))
    }
}

/// Path of the cache file for a key.
pub fn path_for(cache_dir: &Path, key: &str) -> PathBuf {
    cache_dir.join("index").join(format!("{key}.fhvidx"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_corruption() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.fhvidx");
        let mut w = Writer::new();
        w.begin(b"ROWS\0\0\0\0");
        w.u32s(&[1, 2, 3]);
        w.u64s(&[u64::MAX]);
        w.begin(b"META\0\0\0\0");
        w.bytes(b"hello");
        w.finish(&p).unwrap();
        let r = Reader::open(&p).unwrap();
        let mut c = r.section(b"ROWS\0\0\0\0").unwrap();
        assert_eq!(c.u32s().unwrap(), [1, 2, 3]);
        assert_eq!(c.u64s().unwrap(), [u64::MAX]);
        assert_eq!(
            r.section(b"META\0\0\0\0").unwrap().bytes().unwrap(),
            b"hello"
        );
        assert!(r.section(b"NOPE\0\0\0\0").is_err());
        // Flip one byte: rejected.
        let mut b = fs::read(&p).unwrap();
        b[20] ^= 1;
        fs::write(&p, &b).unwrap();
        assert!(Reader::open(&p).is_err());
    }
}
