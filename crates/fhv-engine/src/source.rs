//! Read-only access to the bytes of the file being viewed.
//!
//! Two access modes, measured against each other (docs/PERF.md):
//! memory mapping and positional reads (`pread` / `seek_read`). Both open
//! the file read-only; nothing ever writes to it.

use std::fs::File;
use std::io;
use std::ops::Range;
use std::path::{Path, PathBuf};

use memmap2::Mmap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Mmap,
    Pread,
}

pub struct Source {
    path: PathBuf,
    file: File,
    map: Option<Mmap>,
    /// Bytes before the data (a UTF-8 BOM).
    pub data_start: u64,
    pub len: u64,
}

impl Source {
    pub fn open(path: &Path, access: Access, data_start: u64) -> io::Result<Source> {
        let file = File::open(path)?;
        let len = file.metadata()?.len();
        let map = match access {
            // SAFETY: the mapping is read-only. If another process truncates
            // the file while it is mapped, reads past the new end fault
            // (SIGBUS); evidence files are not modified while viewed, and
            // the positional-read mode exists for that risk.
            Access::Mmap if len > 0 => Some(unsafe { Mmap::map(&file)? }),
            _ => None,
        };
        Ok(Source {
            path: path.to_path_buf(),
            file,
            map,
            data_start: data_start.min(len),
            len,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn access(&self) -> Access {
        if self.map.is_some() {
            Access::Mmap
        } else {
            Access::Pread
        }
    }

    /// Bytes in `range` (clamped to the file). With mmap the slice borrows
    /// the mapping; otherwise it is read into `buf`.
    pub fn read<'a>(&'a self, range: Range<u64>, buf: &'a mut Vec<u8>) -> io::Result<&'a [u8]> {
        let end = range.end.min(self.len);
        let start = range.start.min(end);
        if let Some(m) = &self.map {
            return Ok(&m[start as usize..end as usize]);
        }
        let n = (end - start) as usize;
        // Reused buffers keep their length: only growth is zeroed; the read
        // overwrites the rest (no memset over every block).
        buf.truncate(n);
        if buf.len() < n {
            buf.resize(n, 0);
        }
        read_exact_at(&self.file, buf, start)?;
        Ok(&buf[..])
    }

    /// The whole mapping, when mapped.
    pub fn mapped(&self) -> Option<&[u8]> {
        self.map.as_deref()
    }
}

#[cfg(unix)]
fn read_exact_at(f: &File, buf: &mut [u8], off: u64) -> io::Result<()> {
    use std::os::unix::fs::FileExt;
    f.read_exact_at(buf, off)
}

#[cfg(windows)]
fn read_exact_at(f: &File, mut buf: &mut [u8], mut off: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        match f.seek_read(buf, off) {
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => {
                buf = &mut buf[n..];
                off += n as u64;
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}
