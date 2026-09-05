//! Byte classification helpers.
//!
//! The hot loop never touches UTF-8: XML delimiters are all ASCII, so bytes ≥ 0x80
//! are simply "name characters". Character-data runs are skipped with a SWAR
//! (SIMD-Within-A-Register) scan that tests eight bytes per iteration for the four
//! bytes that can end a text run — `<`, `&`, `]` and `\n`. On `wasm32` with
//! `-C target-feature=+simd128` LLVM lowers this to `v128` compares; everywhere else
//! it stays branch-light scalar code, and the result is bit-identical either way.

/// Space, tab, CR or LF.
#[inline]
pub const fn is_ws(b: u8) -> bool {
    b == b' ' || b == b'\t' || b == b'\n' || b == b'\r'
}

/// First character of an XML Name (XML 1.0 §2.3 [4]); every byte ≥ 0x80 is accepted
/// so UTF-8 names work without decoding.
#[inline]
pub const fn is_name_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_' || b == b':' || b >= 0x80
}

/// Subsequent character of an XML Name (XML 1.0 §2.3 [4a]).
#[inline]
pub const fn is_name_char(b: u8) -> bool {
    is_name_start(b) || b.is_ascii_digit() || b == b'-' || b == b'.' || b == 0xb7
}

const LO: u64 = 0x0101_0101_0101_0101;
const HI: u64 = 0x8080_8080_8080_8080;

#[inline]
const fn has_zero(v: u64) -> bool {
    v.wrapping_sub(LO) & !v & HI != 0
}

#[inline]
const fn has_byte(v: u64, b: u8) -> bool {
    has_zero(v ^ (LO.wrapping_mul(b as u64)))
}

/// Index of the next occurrence of any of four bytes at or after `from`, or `buf.len()`.
///
/// Eight bytes are classified per iteration; pass the same byte twice to search for
/// fewer than four distinct delimiters.
#[inline]
pub fn find_any4(buf: &[u8], from: usize, a: u8, b: u8, c: u8, d: u8) -> usize {
    let mut i = from;
    let n = buf.len();
    while i + 8 <= n {
        let mut w = [0u8; 8];
        w.copy_from_slice(&buf[i..i + 8]);
        let v = u64::from_le_bytes(w);
        if has_byte(v, a) || has_byte(v, b) || has_byte(v, c) || has_byte(v, d) {
            break;
        }
        i += 8;
    }
    while i < n {
        let x = buf[i];
        if x == a || x == b || x == c || x == d {
            return i;
        }
        i += 1;
    }
    n
}

/// Index of the next byte in `{'<', '&', ']', '\n'}` at or after `from`,
/// or `buf.len()` when the rest of the buffer is plain character data.
#[inline]
pub fn find_text_delim(buf: &[u8], from: usize) -> usize {
    find_any4(buf, from, b'<', b'&', b']', b'\n')
}

/// Index of the first byte at or after `from` that cannot continue an XML Name.
#[inline]
pub fn find_name_end(buf: &[u8], from: usize) -> usize {
    let mut i = from;
    while i < buf.len() && is_name_char(buf[i]) {
        i += 1;
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate alloc;
    use alloc::vec::Vec;

    #[test]
    fn classes() {
        assert!(is_ws(b' ') && is_ws(b'\n') && !is_ws(b'x'));
        assert!(is_name_start(b'a') && is_name_start(b':') && is_name_start(0xc3));
        assert!(!is_name_start(b'-') && is_name_char(b'-') && is_name_char(b'7'));
    }

    #[test]
    fn swar_matches_naive_for_every_alignment() {
        let mut data: Vec<u8> = Vec::new();
        for i in 0..512u32 {
            data.push((i % 251) as u8);
        }
        for &probe in b"<&]\n".iter() {
            for pos in 0..data.len() {
                let mut d = data.clone();
                for x in d.iter_mut() {
                    if *x == b'<' || *x == b'&' || *x == b']' || *x == b'\n' {
                        *x = b'x';
                    }
                }
                d[pos] = probe;
                for from in 0..=pos {
                    assert_eq!(
                        find_text_delim(&d, from),
                        pos,
                        "probe {probe} pos {pos} from {from}"
                    );
                }
            }
        }
        let clean = alloc::vec![b'x'; 100];
        assert_eq!(find_text_delim(&clean, 0), 100);
    }
}
