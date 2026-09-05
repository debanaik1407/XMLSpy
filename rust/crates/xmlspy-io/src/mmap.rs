//! `mmap` — the native [`ByteSource`].
//!
//! The browser streams a document with `Blob.slice()`; the native engine maps it instead.
//! Mapping is what makes random access free: "give me lines 4 000 000..4 000 040" becomes a
//! pointer into the page cache rather than a `seek` + `read` round trip, and the OS evicts
//! pages under memory pressure so a 10 GiB document still costs the process almost nothing.
//!
//! # Audit
//!
//! This is the only module in the workspace that contains `unsafe`. Every invariant the
//! code relies on is listed here, with the reason it holds.
//!
//! 1. **Only three libc symbols are declared** — `mmap`, `munmap`, `madvise` — as
//!    `extern "C"`. No `libc` crate, no bindings generator, no variadic calls. The
//!    signatures match POSIX (`mmap(2)`/`munmap(2)`/`madvise(2)`) on every 64-bit unix
//!    target Rust supports, where `size_t == usize`, `int == c_int` and `off_t == i64`.
//! 2. **The mapping is read-only and private** (`PROT_READ | MAP_PRIVATE`). Nothing in this
//!    crate can write through it, so there is no aliasing `&mut` and no data race between
//!    threads that all hold `&[u8]` into the same mapping.
//! 3. **The file descriptor outlives the mapping**: `Mmap` owns the `File` it was created
//!    from and drops it *after* `munmap` (field order in `Drop` is explicit). Linux and
//!    macOS both keep a mapping valid after `close`, but owning the handle means the inode
//!    cannot be recycled underneath us.
//! 4. **Zero-length files are never mapped.** `mmap(len = 0)` fails with `EINVAL`, so an
//!    empty file yields `ptr == null`, `len == 0`, and `as_slice()` returns `&[]` without
//!    dereferencing anything.
//! 5. **The pointer is only turned into a slice with the exact mapped length**, and
//!    `Mmap::slice` range-checks against that length, so no slice can reach past the
//!    mapping. `MAP_FAILED` (`(void *)-1`) is compared before the pointer is ever used.
//! 6. **`munmap` is called exactly once**, from `Drop`, and its result is discarded rather
//!    than `unwrap`ped — a `Drop` impl must not panic. `Mmap` is not `Copy`/`Clone`, so
//!    there is no second owner to double-unmap.
//! 7. **`Mmap` is deliberately not `Send`/`Sync`** (the raw pointer makes it `!Send`
//!    automatically and we do not opt in). Callers that need the bytes on several threads
//!    take `&[u8]` from `as_slice()` first — `&[u8]` is `Send + Sync` — which is exactly
//!    what the parallel index builder does.
//! 8. **`madvise(MADV_SEQUENTIAL)` is best effort.** Its return value is ignored: it is a
//!    hint about read-ahead, and a platform that does not implement it must not turn a
//!    successful mapping into an error.
//!
//! ## Known limitation, stated plainly
//!
//! If another process truncates the file while it is mapped, a page fault past the new end
//! raises `SIGBUS` and kills the process — this is inherent to `mmap` and is why the index
//! cache is keyed on a fingerprint (length + mtime + a hash of the first and last 4 KiB,
//! see [`crate::cache`]): a stale mapping is never *used*, but a racing writer can still
//! crash the reader. XMLSpy-rs opens documents read-only and does not rewrite them in
//! place (edits are saved through a temporary file and renamed), so the window is limited
//! to foreign writers.
//!
//! Platforms without a usable `mmap` (Windows, 32-bit unix) fall back to
//! [`crate::file::FileSource`], which streams with `seek` + `read`; [`crate::open_byte_source`]
//! picks between the two and reports which one it chose.

#![allow(unsafe_code)]

use std::fs::File;
use std::io;
use std::ops::Deref;
use std::path::Path;

use xmlspy_core::{ByteSource, SourceError};

/// True when this target has a real, audited `mmap` implementation.
pub const HAVE_MMAP: bool = cfg!(all(unix, target_pointer_width = "64"));

/// Page size assumed for alignment bookkeeping. Every platform we map on uses 4 KiB or
/// 16 KiB pages; the value is only used for reporting and read-ahead hints, never for
/// pointer arithmetic, so an incorrect guess cannot cause unsoundness.
pub const PAGE_SIZE: usize = 4096;

#[cfg(all(unix, target_pointer_width = "64"))]
mod imp {
    use core::ffi::{c_int, c_void};

    /// `PROT_READ` — 1 on Linux, macOS and the BSDs.
    pub const PROT_READ: c_int = 1;
    /// `MAP_PRIVATE` — 2 on Linux, macOS and the BSDs.
    pub const MAP_PRIVATE: c_int = 2;
    /// `MADV_SEQUENTIAL` — 2 on Linux and macOS.
    pub const MADV_SEQUENTIAL: c_int = 2;

    extern "C" {
        pub fn mmap(
            addr: *mut c_void,
            len: usize,
            prot: c_int,
            flags: c_int,
            fd: c_int,
            off: i64,
        ) -> *mut c_void;
        pub fn munmap(addr: *mut c_void, len: usize) -> c_int;
        pub fn madvise(addr: *mut c_void, len: usize, advice: c_int) -> c_int;
    }
}

/// A read-only memory mapping of a file.
///
/// Dereferences to the file's bytes. Dropping it unmaps.
pub struct Mmap {
    /// Base address, or null for an empty (unmapped) file.
    ptr: *mut u8,
    /// Mapped length in bytes.
    len: usize,
    /// Keeps the descriptor alive for as long as the mapping exists.
    file: File,
}

impl Mmap {
    /// Map `path` read-only.
    ///
    /// # Errors
    /// * the file cannot be opened or stat'ed;
    /// * the platform has no audited `mmap` ([`HAVE_MMAP`] is false) — callers should then
    ///   use [`crate::file::FileSource`];
    /// * the file is larger than the address space;
    /// * the `mmap` syscall fails (`errno` is reported through [`io::Error`]).
    pub fn open(path: &Path) -> io::Result<Mmap> {
        let file = File::open(path)?;
        let size = file.metadata()?.len();
        let Ok(len) = usize::try_from(size) else {
            return Err(io::Error::new(
                io::ErrorKind::OutOfMemory,
                format!("{path:?}: {size} bytes does not fit in this address space"),
            ));
        };
        if len == 0 {
            // Invariant 4: never call mmap with length 0.
            return Ok(Mmap {
                ptr: core::ptr::null_mut(),
                len: 0,
                file,
            });
        }

        #[cfg(all(unix, target_pointer_width = "64"))]
        let mapped = {
            use std::os::unix::io::AsRawFd;
            // SAFETY: `file` is open and owns a valid descriptor, `len > 0`, and the
            // arguments are the documented constants for a read-only private mapping
            // (invariants 1, 2, 3). The result is checked against MAP_FAILED before use
            // (invariant 5).
            let p = unsafe {
                imp::mmap(
                    core::ptr::null_mut(),
                    len,
                    imp::PROT_READ,
                    imp::MAP_PRIVATE,
                    file.as_raw_fd(),
                    0,
                )
            };
            if p as usize == usize::MAX {
                Err(io::Error::last_os_error())
            } else {
                // SAFETY: `p` is a valid mapping of exactly `len` bytes; the hint cannot
                // invalidate it and its result is intentionally ignored (invariant 8).
                unsafe {
                    let _ = imp::madvise(p, len, imp::MADV_SEQUENTIAL);
                }
                Ok(Mmap {
                    ptr: p as *mut u8,
                    len,
                    file,
                })
            }
        };

        #[cfg(not(all(unix, target_pointer_width = "64")))]
        let mapped = {
            drop(file);
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "mmap is not available on this platform; use the buffered byte source",
            ))
        };

        mapped
    }

    /// Mapped length in bytes.
    pub fn len(&self) -> usize {
        self.len
    }

    /// True when the file was empty (nothing is mapped).
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// True when the bytes really come from a mapping rather than a fallback buffer.
    pub fn is_mapped(&self) -> bool {
        !self.ptr.is_null()
    }

    /// The whole file as a byte slice.
    pub fn as_slice(&self) -> &[u8] {
        if self.ptr.is_null() || self.len == 0 {
            return &[];
        }
        // SAFETY: invariant 5 — `ptr` is the base of a live mapping of exactly `len`
        // readable bytes, kept alive by `self.file` (invariant 3), and the mapping is
        // read-only (invariant 2) so the slice cannot alias a mutable reference.
        unsafe { core::slice::from_raw_parts(self.ptr as *const u8, self.len) }
    }

    /// Bounds-checked sub-slice; `None` when the range is not fully inside the file.
    pub fn slice(&self, start: usize, end: usize) -> Option<&[u8]> {
        if start > end || end > self.len {
            return None;
        }
        self.as_slice().get(start..end)
    }

    /// The open file handle behind the mapping.
    pub fn file(&self) -> &File {
        &self.file
    }
}

impl core::fmt::Debug for Mmap {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Mmap")
            .field("len", &self.len)
            .field("mapped", &self.is_mapped())
            .finish()
    }
}

impl Deref for Mmap {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        self.as_slice()
    }
}

impl AsRef<[u8]> for Mmap {
    fn as_ref(&self) -> &[u8] {
        self.as_slice()
    }
}

impl Drop for Mmap {
    fn drop(&mut self) {
        if self.ptr.is_null() || self.len == 0 {
            return;
        }
        // SAFETY: invariant 6 — this pointer/length pair came from `mmap` in `open`,
        // `Drop` runs exactly once, and the result is discarded instead of panicking.
        #[cfg(all(unix, target_pointer_width = "64"))]
        unsafe {
            let _ = imp::munmap(self.ptr as *mut core::ffi::c_void, self.len);
        }
        self.ptr = core::ptr::null_mut();
        self.len = 0;
    }
}

/// A [`ByteSource`] backed by a mapping: `chunk()` is a slice, never a copy or a syscall.
#[derive(Debug)]
pub struct MmapSource {
    map: Mmap,
}

impl MmapSource {
    /// Map `path` and wrap it.
    pub fn open(path: &Path) -> io::Result<MmapSource> {
        Ok(MmapSource {
            map: Mmap::open(path)?,
        })
    }

    /// Wrap an existing mapping.
    pub fn new(map: Mmap) -> MmapSource {
        MmapSource { map }
    }

    /// The mapping.
    pub fn mmap(&self) -> &Mmap {
        &self.map
    }
}

impl ByteSource for MmapSource {
    fn len(&self) -> u64 {
        self.map.len() as u64
    }

    fn chunk(&mut self, offset: u64, len: usize) -> Result<&[u8], SourceError> {
        let total = self.map.len() as u64;
        if offset > total {
            return Err(SourceError::OutOfBounds { offset, len: total });
        }
        let start = offset as usize;
        let end = self.map.len().min(start.saturating_add(len));
        Ok(self.map.slice(start, end).unwrap_or(&[]))
    }

    fn as_slice(&self) -> Option<&[u8]> {
        Some(self.map.as_slice())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp(name: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("xmlspy-mmap-{}-{}", std::process::id(), name));
        p
    }

    #[test]
    fn maps_a_file_and_reads_it_back() {
        if !HAVE_MMAP {
            return; // the buffered fallback is covered in tests/io.rs
        }
        let path = temp("basic.xml");
        {
            let mut f = File::create(&path).unwrap();
            f.write_all(b"<a>hello mmap</a>").unwrap();
            f.flush().unwrap();
        }
        let m = Mmap::open(&path).unwrap();
        assert_eq!(m.len(), 17);
        assert!(m.is_mapped());
        assert_eq!(&m[..], b"<a>hello mmap</a>");
        assert_eq!(m.slice(3, 8), Some(&b"hello"[..]));
        assert_eq!(m.slice(8, 3), None);
        assert_eq!(m.slice(0, 17), Some(&b"<a>hello mmap</a>"[..]));
        assert_eq!(m.slice(0, 18), None);
        let mut src = MmapSource::new(Mmap::open(&path).unwrap());
        assert_eq!(src.len(), 17);
        assert_eq!(src.chunk(3, 5).unwrap(), b"hello");
        assert_eq!(src.chunk(12, 99).unwrap(), b"p</a>");
        assert_eq!(src.chunk(17, 1).unwrap(), b"");
        assert!(src.chunk(18, 1).is_err());
        assert_eq!(src.as_slice().map(|s| s.len()), Some(17));
        drop(src);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn empty_file_maps_to_an_empty_slice() {
        if !HAVE_MMAP {
            return;
        }
        let path = temp("empty.xml");
        File::create(&path).unwrap();
        let m = Mmap::open(&path).unwrap();
        assert_eq!(m.len(), 0);
        assert!(m.is_empty());
        assert!(!m.is_mapped());
        assert_eq!(m.as_slice(), &[]);
        let mut src = MmapSource::new(Mmap::open(&path).unwrap());
        assert_eq!(src.chunk(0, 8).unwrap(), b"");
        assert!(src.chunk(1, 8).is_err());
        drop(src);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn missing_file_is_an_io_error() {
        let path = temp("does-not-exist.xml");
        std::fs::remove_file(&path).ok();
        assert!(Mmap::open(&path).is_err());
    }
}
