//! CRC-32 (ISO-HDLC / zlib polynomial), used to make the write-ahead journal
//! self-validating.
//!
//! Written out rather than pulled from a crate because the workspace has **zero external
//! dependencies**: that is what lets `cargo test --offline` and the WASM build work on a
//! machine with no network, and a 30-line CRC is not worth breaking it for.

use std::sync::OnceLock;

/// The reflected polynomial used by zlib, PNG, gzip and ZIP: `0xEDB88320`.
pub const POLY: u32 = 0xEDB8_8320;

fn table() -> &'static [u32; 256] {
    static TABLE: OnceLock<[u32; 256]> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut t = [0u32; 256];
        for (i, slot) in t.iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { POLY ^ (c >> 1) } else { c >> 1 };
            }
            *slot = c;
        }
        t
    })
}

/// Continue a CRC over `data`. Seed with `u32::MAX` and XOR the result to match
/// [`crc32`], or keep the running value to hash several buffers as one.
pub fn crc32_update(crc: u32, data: &[u8]) -> u32 {
    let t = table();
    let mut c = crc;
    for b in data {
        let idx = ((c ^ u32::from(*b)) & 0xFF) as usize;
        c = t[idx] ^ (c >> 8);
    }
    c
}

/// CRC-32 of `data`, compatible with `zlib`/`gzip`/`cksum --crc`.
pub fn crc32(data: &[u8]) -> u32 {
    crc32_update(u32::MAX, data) ^ u32::MAX
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_vectors() {
        // The classic check value for CRC-32/ISO-HDLC.
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
        assert_eq!(crc32(b"a"), 0xE8B7_BE43);
        assert_eq!(
            crc32(b"The quick brown fox jumps over the lazy dog"),
            0x414F_A339
        );
    }

    #[test]
    fn update_is_incremental() {
        let whole = crc32(b"hello world");
        let mut c = u32::MAX;
        c = crc32_update(c, b"hello");
        c = crc32_update(c, b" ");
        c = crc32_update(c, b"world");
        assert_eq!(c ^ u32::MAX, whole);
    }
}
