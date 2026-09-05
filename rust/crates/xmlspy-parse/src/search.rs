//! Streaming literal search over a chunked byte stream.
//!
//! The browser's Find-in-Files and the CLI's `search` command both stream the file in
//! 8 MiB chunks; [`Finder`] keeps `needle.len() - 1` bytes of carry-over so a hit that
//! straddles a chunk boundary is found exactly once, and tracks line/column as it goes
//! so results can be clicked straight into the Text View.

use alloc::vec::Vec;

/// One match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hit {
    /// Absolute byte offset of the first byte of the match.
    pub offset: u64,
    /// 1-based line.
    pub line: u64,
    /// 1-based column, in bytes.
    pub col: u64,
}

/// Chunk-resumable literal finder (ASCII case-insensitive on request).
pub struct Finder {
    needle: Vec<u8>,
    ci: bool,
    max_hits: usize,
    carry: Vec<u8>,
    carry_base: u64,
    line: u64,
    line_start: u64,
    counted_to: u64,
    total: u64,
    hits: Vec<Hit>,
}

#[inline]
fn fold(b: u8, ci: bool) -> u8 {
    if ci {
        b.to_ascii_lowercase()
    } else {
        b
    }
}

impl Finder {
    /// Create a finder for `needle`.
    pub fn new(needle: &[u8], ci: bool, max_hits: usize) -> Self {
        Self {
            needle: needle.iter().map(|b| fold(*b, ci)).collect(),
            ci,
            max_hits,
            carry: Vec::new(),
            carry_base: 0,
            line: 1,
            line_start: 0,
            counted_to: 0,
            total: 0,
            hits: Vec::new(),
        }
    }

    /// Matches recorded so far (capped at `max_hits`).
    pub fn hits(&self) -> &[Hit] {
        &self.hits
    }

    /// Total number of matches seen, including those beyond `max_hits`.
    pub fn total(&self) -> u64 {
        self.total
    }

    /// Drop the recorded matches, keeping the running totals (paged UIs).
    pub fn clear_hits(&mut self) {
        self.hits.clear();
    }

    fn count_newlines(&mut self, buf: &[u8], buf_base: u64, upto: usize) {
        let start = (self.counted_to.saturating_sub(buf_base)) as usize;
        for (k, b) in buf.iter().enumerate().take(upto).skip(start) {
            if *b == b'\n' {
                self.line += 1;
                self.line_start = buf_base + k as u64 + 1;
            }
        }
        self.counted_to = buf_base + upto as u64;
    }

    /// Feed the next chunk; `base` is the absolute offset of `chunk[0]`.
    pub fn feed(&mut self, chunk: &[u8], base: u64) {
        if self.needle.is_empty() || chunk.is_empty() {
            return;
        }
        let keep = self.needle.len() - 1;
        let (buf, buf_base): (Vec<u8>, u64) = if self.carry.is_empty() {
            (chunk.to_vec(), base)
        } else {
            let mut v = Vec::with_capacity(self.carry.len() + chunk.len());
            v.extend_from_slice(&self.carry);
            v.extend_from_slice(chunk);
            (v, self.carry_base)
        };
        if self.counted_to < buf_base {
            self.counted_to = buf_base;
        }

        let n = buf.len();
        let m = self.needle.len();
        if n >= m {
            let first = self.needle[0];
            let mut p = 0usize;
            while p + m <= n {
                if fold(buf[p], self.ci) != first {
                    p += 1;
                    continue;
                }
                let mut ok = true;
                for k in 1..m {
                    if fold(buf[p + k], self.ci) != self.needle[k] {
                        ok = false;
                        break;
                    }
                }
                if ok {
                    self.count_newlines(&buf, buf_base, p);
                    let offset = buf_base + p as u64;
                    self.total += 1;
                    if self.hits.len() < self.max_hits {
                        self.hits.push(Hit {
                            offset,
                            line: self.line,
                            col: offset - self.line_start + 1,
                        });
                    }
                }
                p += 1;
            }
        }

        // Count newlines up to the start of the carry region, then keep the tail so a
        // match spanning this boundary is still found (exactly once) in the next chunk.
        let keep = core::cmp::min(keep, n);
        self.count_newlines(&buf, buf_base, n - keep);
        self.carry.clear();
        self.carry.extend_from_slice(&buf[n - keep..]);
        self.carry_base = buf_base + (n - keep) as u64;
    }

    /// Flush trailing state so `line`/`total` describe the whole stream.
    pub fn finish(&mut self) {
        let carry = core::mem::take(&mut self.carry);
        let base = self.carry_base;
        self.count_newlines(&carry, base, carry.len());
        self.carry = carry;
        self.carry.clear();
    }

    /// 1-based line count of everything consumed so far.
    pub fn line(&self) -> u64 {
        self.line
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate alloc;

    fn run(hay: &[u8], needle: &[u8], ci: bool, chunk: usize) -> (Vec<Hit>, u64) {
        let mut f = Finder::new(needle, ci, 1_000_000);
        let mut off = 0usize;
        while off < hay.len() {
            let end = core::cmp::min(hay.len(), off + chunk);
            f.feed(&hay[off..end], off as u64);
            off = end;
        }
        f.finish();
        (f.hits().to_vec(), f.total())
    }

    #[test]
    fn finds_every_hit_at_every_chunk_size() {
        let hay = b"<a>needle</a>\n<b>x needle y</b>\nneedle\n".to_vec();
        let (want, wtotal) = run(&hay, b"needle", false, hay.len());
        assert_eq!(wtotal, 3);
        assert_eq!(want[0].line, 1);
        assert_eq!(want[1].line, 2);
        assert_eq!(want[2].line, 3);
        assert_eq!(want[2].col, 1);
        for chunk in 1..=hay.len() {
            let (got, total) = run(&hay, b"needle", false, chunk);
            assert_eq!(total, wtotal, "chunk {chunk}");
            assert_eq!(got, want, "chunk {chunk}");
        }
    }

    #[test]
    fn case_insensitive_and_overlapping() {
        let (hits, total) = run(b"aaaa NeEdLe", b"needle", true, 3);
        assert_eq!(total, 1);
        assert_eq!(hits[0].offset, 5);
        let (_, t2) = run(b"aaaa", b"aa", false, 2);
        assert_eq!(t2, 3, "overlapping matches are all reported");
    }

    #[test]
    fn respects_max_hits_but_keeps_counting() {
        let mut f = Finder::new(b"x", false, 2);
        f.feed(b"xxxxx", 0);
        f.finish();
        assert_eq!(f.hits().len(), 2);
        assert_eq!(f.total(), 5);
        f.clear_hits();
        assert!(f.hits().is_empty());
        assert_eq!(f.total(), 5);
    }

    #[test]
    fn empty_needle_is_a_no_op() {
        let (hits, total) = run(b"abc", b"", false, 1);
        assert!(hits.is_empty());
        assert_eq!(total, 0);
    }
}
