//! Buffered byte source — the fallback where `mmap` is unavailable (Windows, 32-bit
//! unix) and the way the CLI streams a file in fixed chunks.
//!
//! Semantics match [`crate::mmap::MmapSource`] exactly, so the scanner cannot tell the
//! two apart: `chunk()` returns the bytes at `offset`, clamped to the end of the file,
//! and never copies more than the caller asked for.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use xmlspy_core::{ByteSource, SourceError, CHUNK_SIZE};

/// A [`ByteSource`] that reads through a reusable buffer with `seek` + `read`.
///
/// Not random access in the mmap sense: asking for a range outside the buffer costs a
/// syscall and a copy, so callers that stream forward (the scanner, the finder) get
/// buffer hits and callers that jump around do not.
pub struct FileSource {
    path: PathBuf,
    file: File,
    total: u64,
    buf: Vec<u8>,
    buf_off: u64,
}

impl FileSource {
    /// Open `path` for reading.
    pub fn open(path: &Path) -> io::Result<FileSource> {
        let file = File::open(path)?;
        let total = file.metadata()?.len();
        Ok(FileSource {
            path: path.to_path_buf(),
            file,
            total,
            buf: Vec::new(),
            buf_off: 0,
        })
    }

    /// A second, independent handle on the same file — one per thread, since a single
    /// `File` has one shared seek position.
    pub fn reopen(&self) -> io::Result<FileSource> {
        FileSource::open(&self.path)
    }

    /// The path this source reads.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Bytes currently held in the read buffer.
    pub fn buffered(&self) -> usize {
        self.buf.len()
    }
}

impl ByteSource for FileSource {
    fn len(&self) -> u64 {
        self.total
    }

    fn chunk(&mut self, offset: u64, len: usize) -> Result<&[u8], SourceError> {
        if offset > self.total {
            return Err(SourceError::OutOfBounds {
                offset,
                len: self.total,
            });
        }
        let want = (self.total - offset).min(len as u64) as usize;
        if want == 0 {
            return Ok(&[]);
        }
        let cached = !self.buf.is_empty()
            && offset >= self.buf_off
            && offset + want as u64 <= self.buf_off + self.buf.len() as u64;
        if !cached {
            self.buf.clear();
            self.buf.resize(want, 0);
            self.file
                .seek(SeekFrom::Start(offset))
                .map_err(|e| SourceError::Backend(e.to_string()))?;
            let mut filled = 0usize;
            while filled < want {
                let n = self
                    .file
                    .read(&mut self.buf[filled..want])
                    .map_err(|e| SourceError::Backend(e.to_string()))?;
                if n == 0 {
                    break;
                }
                filled += n;
            }
            self.buf.truncate(filled);
            self.buf_off = offset;
        }
        let start = (offset - self.buf_off) as usize;
        let end = (start + want).min(self.buf.len());
        Ok(&self.buf[start..end])
    }
}

/// Whatever [`open_byte_source`] managed to give you.
pub enum Backed {
    /// Zero-copy `mmap`.
    Mapped(crate::mmap::MmapSource),
    /// `seek` + `read` through a buffer.
    Buffered(FileSource),
}

impl Backed {
    /// `"mmap"` or `"buffered"` — what the status line reports.
    pub fn kind(&self) -> &'static str {
        match self {
            Backed::Mapped(_) => "mmap",
            Backed::Buffered(_) => "buffered",
        }
    }

    /// True when the bytes are mapped rather than read.
    pub fn is_mapped(&self) -> bool {
        matches!(self, Backed::Mapped(m) if m.mmap().is_mapped())
    }
}

impl ByteSource for Backed {
    fn len(&self) -> u64 {
        match self {
            Backed::Mapped(m) => m.len(),
            Backed::Buffered(f) => f.len(),
        }
    }

    fn chunk(&mut self, offset: u64, len: usize) -> Result<&[u8], SourceError> {
        match self {
            Backed::Mapped(m) => m.chunk(offset, len),
            Backed::Buffered(f) => f.chunk(offset, len),
        }
    }

    fn as_slice(&self) -> Option<&[u8]> {
        match self {
            Backed::Mapped(m) => m.as_slice(),
            Backed::Buffered(_) => None,
        }
    }
}

/// Open a document with the best backend this platform has: `mmap` where it is audited
/// and available, a buffered reader everywhere else.
pub fn open_byte_source(path: &Path) -> io::Result<Backed> {
    match crate::mmap::MmapSource::open(path) {
        Ok(m) => Ok(Backed::Mapped(m)),
        Err(_) => Ok(Backed::Buffered(FileSource::open(path)?)),
    }
}

/// Stream a file in `chunk`-byte pieces, calling `f(bytes, absolute_offset)`.
///
/// Returns the number of bytes read. This is the native twin of the browser worker's
/// `Blob.slice()` loop, and it is what `xmlspy wf|search` use when they do not need
/// random access.
pub fn stream_chunks<F>(path: &Path, chunk: usize, mut f: F) -> io::Result<u64>
where
    F: FnMut(&[u8], u64),
{
    let mut file = File::open(path)?;
    let size = chunk.clamp(1, 1 << 30);
    let mut buf = vec![0u8; size];
    let mut off = 0u64;
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        f(&buf[..n], off);
        off += n as u64;
    }
    Ok(off)
}

/// Stream a file through the default [`CHUNK_SIZE`].
pub fn stream_file<F>(path: &Path, f: F) -> io::Result<u64>
where
    F: FnMut(&[u8], u64),
{
    stream_chunks(path, CHUNK_SIZE, f)
}
