//! Native I/O for the XMLSpy-rs engine.
//!
//! Three things the browser gets from the platform and the native engine has to provide
//! itself:
//!
//! * **random access to a document** — [`mmap::Mmap`] maps the file read-only (audited,
//!   the only `unsafe` in the workspace) and [`file::FileSource`] is the buffered
//!   fallback; [`open_byte_source`] picks between them and both implement
//!   [`xmlspy_core::ByteSource`], so the scanner cannot tell which one it got;
//! * **a cache that survives the process** — [`cache::IndexCache`] stores `.xsi` indexes
//!   keyed by a [`cache::Fingerprint`] (length + mtime + a CRC of the first and last
//!   4 KiB) under a byte budget with LRU eviction. This is what makes re-opening a
//!   multi-GB document instant instead of a re-scan;
//! * **a build that survives a crash** — [`journal::Journal`] is a CRC-guarded
//!   write-ahead log of segment results; [`journal::recover`] reads back whatever a
//!   killed process managed to flush, including a torn final entry.
//!
//! The crate is `std` (it touches the filesystem) but has **no external dependencies**,
//! like the rest of the workspace.

#![warn(missing_docs)]

pub mod cache;
pub mod crc;
pub mod file;
pub mod journal;
pub mod mmap;

pub use cache::{CacheEntry, CacheStats, Fingerprint, IndexCache, DEFAULT_BUDGET, EDGE_BYTES};
pub use crc::{crc32, crc32_update};
pub use file::{open_byte_source, stream_chunks, stream_file, Backed, FileSource};
pub use journal::{
    journal_path_for, recover, Entry, Journal, JournalHeader, RecoveredSegment, Recovery,
};
pub use mmap::{Mmap, MmapSource, HAVE_MMAP, PAGE_SIZE};
pub use xmlspy_core::{ByteSource, SourceError, CHUNK_SIZE};

/// Which backend [`open_byte_source`] chose, for the status line.
pub fn backend_name(src: &Backed) -> &'static str {
    src.kind()
}
