//! Core types shared by every crate of the XMLSpy-rs engine.
//!
//! This crate is `no_std` (+ `alloc`) so the whole parsing/indexing pipeline can be
//! compiled to `wasm32-unknown-unknown` without pulling in the standard library,
//! and reused unchanged by the native CLI and server.

#![no_std]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

extern crate alloc;

#[cfg(test)]
extern crate std;

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

/// Largest byte offset the engine addresses (1 TiB, comfortably inside the
/// 40-bit budget used by the on-disk index and by JavaScript's `Number`).
pub const MAX_OFFSET: u64 = 1 << 40;

/// Severity of a diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// Violates a well-formedness constraint of XML 1.0.
    Error,
    /// Suspicious but not fatal.
    Warning,
}

impl Severity {
    /// Wire encoding used by the `.xsi` index and the WASM ABI.
    pub const fn as_u8(self) -> u8 {
        match self {
            Severity::Error => 0,
            Severity::Warning => 1,
        }
    }

    /// Inverse of [`Severity::as_u8`]; anything unknown decodes as an error.
    pub const fn from_u8(v: u8) -> Self {
        match v {
            1 => Severity::Warning,
            _ => Severity::Error,
        }
    }
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Severity::Error => f.write_str("error"),
            Severity::Warning => f.write_str("warning"),
        }
    }
}

/// A well-formedness (or other) diagnostic produced by the scanner.
///
/// Messages cite the XML 1.0 production they violate so the Messages window can
/// show the same text as XMLSpy's validator, and `fix` carries the SmartFix hint
/// that the UI can apply automatically.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WfError {
    /// Absolute byte offset in the document.
    pub offset: u64,
    /// 1-based line number.
    pub line: u64,
    /// 1-based column, counted in bytes.
    pub col: u64,
    /// Human readable, spec-cited message.
    pub msg: String,
    /// Severity of the diagnostic.
    pub severity: Severity,
    /// Optional SmartFix suggestion.
    pub fix: Option<String>,
}

impl WfError {
    /// Create an error-severity diagnostic.
    pub fn error(offset: u64, line: u64, col: u64, msg: impl Into<String>) -> Self {
        Self {
            offset,
            line,
            col,
            msg: msg.into(),
            severity: Severity::Error,
            fix: None,
        }
    }

    /// Attach a SmartFix suggestion.
    #[must_use]
    pub fn with_fix(mut self, fix: impl Into<String>) -> Self {
        self.fix = Some(fix.into());
        self
    }

    /// Mark this diagnostic as a warning.
    #[must_use]
    pub fn as_warning(mut self) -> Self {
        self.severity = Severity::Warning;
        self
    }
}

impl fmt::Display for WfError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}: Ln {}, Col {}: {}",
            self.severity, self.line, self.col, self.msg
        )
    }
}

/// Failure modes of a [`ByteSource`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceError {
    /// The requested range lies outside the source.
    OutOfBounds {
        /// Requested offset.
        offset: u64,
        /// Length of the source.
        len: u64,
    },
    /// The backend failed (I/O error, detached buffer, …).
    Backend(String),
}

impl fmt::Display for SourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SourceError::OutOfBounds { offset, len } => {
                write!(f, "offset {offset} is outside the source (len {len})")
            }
            SourceError::Backend(m) => write!(f, "byte source failed: {m}"),
        }
    }
}

/// A random-access, chunk-oriented view over the bytes of a document.
///
/// Native builds back this with `mmap` (or buffered `pread`), the browser backs it
/// with `Blob.slice`; neither ever needs the whole document in memory.
pub trait ByteSource {
    /// Total length of the document in bytes.
    fn len(&self) -> u64;

    /// True when the document is empty.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Read up to `len` bytes starting at `offset`.
    ///
    /// Implementations may return fewer bytes than requested (at EOF), but never
    /// zero bytes for an in-bounds, non-empty request.
    fn chunk(&mut self, offset: u64, len: usize) -> Result<&[u8], SourceError>;

    /// The whole document at once, when the backend is random-access (native `mmap`,
    /// an in-memory buffer). `None` when the bytes can only be streamed, which is what
    /// the browser's `Blob.slice` worker does.
    ///
    /// The parallel index builder requires `Some`: it hands each thread a `&[u8]` slice
    /// of the document rather than a mutable reader.
    fn as_slice(&self) -> Option<&[u8]> {
        None
    }
}

/// A [`ByteSource`] over a byte slice already in memory (tests, small documents).
#[derive(Debug, Clone)]
pub struct SliceSource<'a> {
    bytes: &'a [u8],
}

impl<'a> SliceSource<'a> {
    /// Wrap a slice.
    pub const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes }
    }
}

impl ByteSource for SliceSource<'_> {
    fn len(&self) -> u64 {
        self.bytes.len() as u64
    }

    fn chunk(&mut self, offset: u64, len: usize) -> Result<&[u8], SourceError> {
        let total = self.bytes.len() as u64;
        if offset > total {
            return Err(SourceError::OutOfBounds { offset, len: total });
        }
        let start = offset as usize;
        let end = core::cmp::min(self.bytes.len(), start.saturating_add(len));
        Ok(&self.bytes[start..end])
    }

    fn as_slice(&self) -> Option<&[u8]> {
        Some(self.bytes)
    }
}

/// A [`ByteSource`] over an owned buffer.
#[derive(Debug, Clone, Default)]
pub struct VecSource {
    bytes: Vec<u8>,
}

impl VecSource {
    /// Wrap an owned buffer.
    pub const fn new(bytes: Vec<u8>) -> Self {
        Self { bytes }
    }
}

impl ByteSource for VecSource {
    fn len(&self) -> u64 {
        self.bytes.len() as u64
    }

    fn chunk(&mut self, offset: u64, len: usize) -> Result<&[u8], SourceError> {
        let total = self.bytes.len() as u64;
        if offset > total {
            return Err(SourceError::OutOfBounds { offset, len: total });
        }
        let start = offset as usize;
        let end = core::cmp::min(self.bytes.len(), start.saturating_add(len));
        Ok(&self.bytes[start..end])
    }

    fn as_slice(&self) -> Option<&[u8]> {
        Some(&self.bytes)
    }
}

/// Default streaming chunk size: 8 MiB = 2048 × 4 KiB pages.
pub const CHUNK_SIZE: usize = 8 * 1024 * 1024;

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    #[test]
    fn slice_source_reads_and_clamps() {
        let data = b"<a>hello</a>";
        let mut s = SliceSource::new(data);
        assert_eq!(s.len(), 12);
        assert_eq!(s.chunk(0, 3).unwrap(), b"<a>");
        assert_eq!(s.chunk(9, 999).unwrap(), b"/a>");
        assert!(s.chunk(13, 1).is_err());
        assert_eq!(s.as_slice(), Some(&b"<a>hello</a>"[..]));
        assert_eq!(VecSource::new(Vec::new()).as_slice(), Some(&b""[..]));
    }

    #[test]
    fn error_display_is_spec_shaped() {
        let e = WfError::error(10, 2, 5, "boom").with_fix("do it");
        assert_eq!(e.to_string(), "error: Ln 2, Col 5: boom");
        assert_eq!(e.fix.as_deref(), Some("do it"));
        assert_eq!(
            Severity::from_u8(Severity::Warning.as_u8()),
            Severity::Warning
        );
        assert_eq!("warning".to_string(), Severity::Warning.to_string());
    }
}
