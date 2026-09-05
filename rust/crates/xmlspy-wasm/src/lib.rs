//! WebAssembly bindings for the XMLSpy-rs engine.
//!
//! Deliberately a **plain C ABI** rather than `wasm-bindgen`: the module is loaded from a
//! Blob-URL Web Worker inside a single-file bundle, so it must be one self-contained
//! `.wasm` with no JS glue, no import object and no build-time codegen. The host only
//! needs `WebAssembly.instantiate(bytes, {})`.
//!
//! Offsets cross the boundary as `f64` (exact for every integer below 2^53 — far beyond
//! the 1 TiB the engine addresses), which avoids forcing BigInt on the JS side.
//!
//! Results are returned as one flat buffer: the scanner hands back a `.xsi` image
//! (see `xmlspy_index::xsi`) that JavaScript maps to typed arrays without re-parsing,
//! and the finder hands back `f64` triples `(offset, line, col)`.
//!
//! ```text
//! JS                                   WASM
//! ─────────────────────────────────────────────────────────────────
//! p = xs_alloc(8 MiB)                  bump into the linear memory
//! copy Blob.slice → memory[p..]
//! xs_scanner_feed(s, p, n, base)       resumable state machine
//! …repeat…                             progress via xs_scanner_*
//! xs_scanner_finish(s, total)          serialises .xsi
//! xs_scanner_xsi_ptr/len(s)            zero-copy typed-array views
//! ```

#![warn(missing_docs)]

use xmlspy_index::xsi;
use xmlspy_parse::{Finder, Scanner, ScannerConfig};

/// ABI version; the JS loader refuses a module it does not understand.
pub const ABI_VERSION: u32 = 1;

/// Bytes per hit record in the finder's result buffer (`offset`, `line`, `col` as `f64`).
pub const HIT_STRIDE: usize = 24;

/// Returns [`ABI_VERSION`].
#[no_mangle]
pub extern "C" fn xs_abi_version() -> u32 {
    ABI_VERSION
}

/// Allocate `len` bytes inside the WASM linear memory and return the pointer.
///
/// # Safety
/// The caller must release the block with [`xs_free`] using the same `len`.
#[no_mangle]
pub extern "C" fn xs_alloc(len: usize) -> *mut u8 {
    let mut v: Vec<u8> = Vec::with_capacity(len);
    let p = v.as_mut_ptr();
    core::mem::forget(v);
    p
}

/// Release a block obtained from [`xs_alloc`].
///
/// # Safety
/// `ptr`/`len` must come from a previous [`xs_alloc`] call and not be used afterwards.
#[no_mangle]
pub unsafe extern "C" fn xs_free(ptr: *mut u8, len: usize) {
    if !ptr.is_null() && len > 0 {
        drop(unsafe { Vec::from_raw_parts(ptr, 0, len) });
    }
}

/// Scanner + its serialised index, owned by the host through an opaque pointer.
pub struct ScannerHandle {
    scanner: Scanner,
    xsi: Vec<u8>,
}

/// Create a scanner. Returns an opaque handle to be freed with [`xs_scanner_free`].
#[no_mangle]
pub extern "C" fn xs_scanner_new(
    max_indexed: u32,
    stride: u32,
    max_errors: u32,
) -> *mut ScannerHandle {
    let cfg = ScannerConfig {
        max_indexed,
        stride: stride.max(1),
        max_errors,
    };
    Box::into_raw(Box::new(ScannerHandle {
        scanner: Scanner::new(cfg),
        xsi: Vec::new(),
    }))
}

/// Free a scanner handle.
///
/// # Safety
/// `h` must have come from [`xs_scanner_new`] and must not be used again.
#[no_mangle]
pub unsafe extern "C" fn xs_scanner_free(h: *mut ScannerHandle) {
    if !h.is_null() {
        drop(unsafe { Box::from_raw(h) });
    }
}

/// Feed one chunk; `base` is the absolute offset of `ptr[0]` in the document.
///
/// # Safety
/// `ptr` must point to `len` readable bytes inside the linear memory.
#[no_mangle]
pub unsafe extern "C" fn xs_scanner_feed(
    h: *mut ScannerHandle,
    ptr: *const u8,
    len: usize,
    base: f64,
) {
    let Some(h) = (unsafe { h.as_mut() }) else {
        return;
    };
    if ptr.is_null() || len == 0 {
        return;
    }
    let buf = unsafe { core::slice::from_raw_parts(ptr, len) };
    h.scanner.feed(buf, base as u64);
}

/// Current 1-based line.
///
/// # Safety
/// `h` must be a live scanner handle.
#[no_mangle]
pub unsafe extern "C" fn xs_scanner_line(h: *const ScannerHandle) -> f64 {
    unsafe { h.as_ref() }.map_or(0.0, |h| h.scanner.progress().line as f64)
}

/// Elements seen so far.
///
/// # Safety
/// `h` must be a live scanner handle.
#[no_mangle]
pub unsafe extern "C" fn xs_scanner_elements(h: *const ScannerHandle) -> f64 {
    unsafe { h.as_ref() }.map_or(0.0, |h| h.scanner.progress().elements as f64)
}

/// Diagnostics seen so far.
///
/// # Safety
/// `h` must be a live scanner handle.
#[no_mangle]
pub unsafe extern "C" fn xs_scanner_errors(h: *const ScannerHandle) -> f64 {
    unsafe { h.as_ref() }.map_or(0.0, |h| h.scanner.progress().errors as f64)
}

/// Finish the document and serialise the index; then read it with
/// [`xs_scanner_xsi_ptr`] / [`xs_scanner_xsi_len`].
///
/// # Safety
/// `h` must be a live scanner handle.
#[no_mangle]
pub unsafe extern "C" fn xs_scanner_finish(h: *mut ScannerHandle, total: f64) {
    let Some(h) = (unsafe { h.as_mut() }) else {
        return;
    };
    h.scanner.finish(total as u64);
    let ix = h.scanner.index_snapshot();
    h.xsi = xsi::encode(&ix, false);
}

/// Serialise the index built *so far* without ending the document
/// (progressive open / cancelled scan). Sets the `.xsi` partial flag.
///
/// # Safety
/// `h` must be a live scanner handle.
#[no_mangle]
pub unsafe extern "C" fn xs_scanner_snapshot(h: *mut ScannerHandle) {
    let Some(h) = (unsafe { h.as_mut() }) else {
        return;
    };
    let ix = h.scanner.index_snapshot();
    h.xsi = xsi::encode(&ix, true);
}

/// Pointer to the serialised `.xsi` image.
///
/// # Safety
/// `h` must be a live scanner handle; the buffer is valid until the next
/// finish/snapshot call or until the handle is freed.
#[no_mangle]
pub unsafe extern "C" fn xs_scanner_xsi_ptr(h: *const ScannerHandle) -> *const u8 {
    unsafe { h.as_ref() }.map_or(core::ptr::null(), |h| h.xsi.as_ptr())
}

/// Length of the serialised `.xsi` image.
///
/// # Safety
/// `h` must be a live scanner handle.
#[no_mangle]
pub unsafe extern "C" fn xs_scanner_xsi_len(h: *const ScannerHandle) -> usize {
    unsafe { h.as_ref() }.map_or(0, |h| h.xsi.len())
}

/// Finder + its serialised hit buffer.
pub struct FinderHandle {
    finder: Finder,
    hits: Vec<u8>,
}

/// Create a streaming literal finder.
///
/// # Safety
/// `ptr` must point to `len` readable bytes.
#[no_mangle]
pub unsafe extern "C" fn xs_finder_new(
    ptr: *const u8,
    len: usize,
    case_insensitive: u32,
    max_hits: usize,
) -> *mut FinderHandle {
    let needle: &[u8] = if ptr.is_null() || len == 0 {
        &[]
    } else {
        unsafe { core::slice::from_raw_parts(ptr, len) }
    };
    Box::into_raw(Box::new(FinderHandle {
        finder: Finder::new(needle, case_insensitive != 0, max_hits),
        hits: Vec::new(),
    }))
}

/// Free a finder handle.
///
/// # Safety
/// `h` must have come from [`xs_finder_new`].
#[no_mangle]
pub unsafe extern "C" fn xs_finder_free(h: *mut FinderHandle) {
    if !h.is_null() {
        drop(unsafe { Box::from_raw(h) });
    }
}

/// Feed one chunk to the finder.
///
/// # Safety
/// `ptr` must point to `len` readable bytes inside the linear memory.
#[no_mangle]
pub unsafe extern "C" fn xs_finder_feed(
    h: *mut FinderHandle,
    ptr: *const u8,
    len: usize,
    base: f64,
) {
    let Some(h) = (unsafe { h.as_mut() }) else {
        return;
    };
    if ptr.is_null() || len == 0 {
        return;
    }
    let buf = unsafe { core::slice::from_raw_parts(ptr, len) };
    h.finder.feed(buf, base as u64);
}

/// Flush trailing state at end of stream.
///
/// # Safety
/// `h` must be a live finder handle.
#[no_mangle]
pub unsafe extern "C" fn xs_finder_finish(h: *mut FinderHandle) {
    if let Some(h) = unsafe { h.as_mut() } {
        h.finder.finish();
    }
}

/// Total number of matches seen (may exceed the number of recorded hits).
///
/// # Safety
/// `h` must be a live finder handle.
#[no_mangle]
pub unsafe extern "C" fn xs_finder_total(h: *const FinderHandle) -> f64 {
    unsafe { h.as_ref() }.map_or(0.0, |h| h.finder.total() as f64)
}

/// Serialise recorded hits into `f64` triples and return how many there are.
///
/// # Safety
/// `h` must be a live finder handle.
#[no_mangle]
pub unsafe extern "C" fn xs_finder_snapshot(h: *mut FinderHandle) -> usize {
    let Some(h) = (unsafe { h.as_mut() }) else {
        return 0;
    };
    let hits = h.finder.hits();
    let mut buf = Vec::with_capacity(hits.len() * HIT_STRIDE);
    for hit in hits {
        buf.extend_from_slice(&(hit.offset as f64).to_le_bytes());
        buf.extend_from_slice(&(hit.line as f64).to_le_bytes());
        buf.extend_from_slice(&(hit.col as f64).to_le_bytes());
    }
    h.hits = buf;
    hits.len()
}

/// Pointer to the buffer filled by [`xs_finder_snapshot`].
///
/// # Safety
/// `h` must be a live finder handle.
#[no_mangle]
pub unsafe extern "C" fn xs_finder_hits_ptr(h: *const FinderHandle) -> *const u8 {
    unsafe { h.as_ref() }.map_or(core::ptr::null(), |h| h.hits.as_ptr())
}

/// Drop recorded hits, keep the running totals (paged result lists).
///
/// # Safety
/// `h` must be a live finder handle.
#[no_mangle]
pub unsafe extern "C" fn xs_finder_clear_hits(h: *mut FinderHandle) {
    if let Some(h) = unsafe { h.as_mut() } {
        h.finder.clear_hits();
        h.hits.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scanner_abi_round_trip() {
        let src = b"<a><b x=\"1\"/></a>";
        let h = xs_scanner_new(1000, 32, 100);
        unsafe {
            xs_scanner_feed(h, src.as_ptr(), src.len(), 0.0);
            assert_eq!(xs_scanner_elements(h), 2.0);
            xs_scanner_finish(h, src.len() as f64);
            let len = xs_scanner_xsi_len(h);
            let ptr = xs_scanner_xsi_ptr(h);
            let buf = core::slice::from_raw_parts(ptr, len);
            let ix = xsi::decode(buf).unwrap();
            assert_eq!(ix.total_elements, 2);
            assert_eq!(ix.total_attributes, 1);
            assert_eq!(ix.error_count, 0);
            assert_eq!(ix.name_of(1), Some("b"));
            xs_scanner_free(h);
        }
    }

    #[test]
    fn finder_abi_round_trip() {
        let hay = b"one needle two needle";
        let needle = b"needle";
        unsafe {
            let h = xs_finder_new(needle.as_ptr(), needle.len(), 0, 10);
            xs_finder_feed(h, hay.as_ptr(), hay.len(), 0.0);
            xs_finder_finish(h);
            assert_eq!(xs_finder_total(h), 2.0);
            let n = xs_finder_snapshot(h);
            assert_eq!(n, 2);
            let raw = core::slice::from_raw_parts(xs_finder_hits_ptr(h), n * HIT_STRIDE);
            let mut first = [0u8; 8];
            first.copy_from_slice(&raw[0..8]);
            assert_eq!(f64::from_le_bytes(first), 4.0);
            xs_finder_clear_hits(h);
            assert_eq!(xs_finder_snapshot(h), 0);
            xs_finder_free(h);
        }
    }

    #[test]
    fn alloc_free_is_reusable() {
        let p = xs_alloc(1024);
        assert!(!p.is_null());
        unsafe { xs_free(p, 1024) };
        assert_eq!(xs_abi_version(), ABI_VERSION);
    }
}
