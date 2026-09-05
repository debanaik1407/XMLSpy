//! Rope of pieces — the edit buffer for documents too large to copy.
//!
//! A 10 GiB XML file must stay on disk while the user edits it. This crate keeps the
//! original bytes **immutable** and records edits as a list of pieces that point either
//! into the original or into an append-only "add" buffer:
//!
//! ```text
//! original: <PurchaseOrders>\n  <Order id="0">\n …10 GiB… </PurchaseOrders>\n
//! pieces:   [O 0..8192] [A 0..3] [O 8192..10_737_418_240]
//!                              └── the user typed "ab" → "abc"
//! ```
//!
//! Consequences, which are the whole point:
//!
//! * `insert` / `delete` / `replace` touch **O(pieces)**, never O(document). A 3-byte
//!   edit in a 10 GiB file splits one piece and appends three bytes.
//! * Saving streams the pieces: unchanged runs are handed to the writer as contiguous
//!   slices of the original (in the browser, as zero-copy `Blob` parts), so the write is
//!   bounded by the *destination*, not by a re-serialisation of the document.
//!   [`Rope::unchanged_ratio`] and [`Rope::original_runs`] report how much of the file is
//!   still passing through untouched — that is the number behind the "3-byte edit in a
//!   10 GiB file" performance gate.
//! * Nothing is ever copied twice: deleted bytes are dropped from the piece list, not
//!   from the buffers, so undo can be implemented by keeping the old piece list.
//!
//! The piece list is kept small by [`Rope::coalesce`], which runs after every mutation and
//! merges adjacent pieces from the same buffer. `tests/props.rs` hammers the buffer with
//! deterministic pseudo-random operation sequences and compares against a `Vec<u8>`
//! oracle after **every** operation — the round-trip property the design document asks
//! for, without a property-testing dependency (the workspace has none).
//!
//! Line-oriented helpers ([`Rope::line_range`], [`Rope::insert_line_after`],
//! [`Rope::delete_line`]) mirror the browser's document model, which is line-granular
//! today; they are O(document) because they count newlines, and the *sparse* line table
//! for multi-GB files comes from the structural index checkpoints, not from here.

#![no_std]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

extern crate alloc;

#[cfg(test)]
extern crate std;

use alloc::vec::Vec;
use core::ops::Range;

/// Which buffer a piece points into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// The immutable bytes the rope was created from.
    Original,
    /// The append-only buffer every edit is written to.
    Add,
}

/// One run of bytes: `buffer[start..start + len]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Piece {
    /// Buffer this piece reads from.
    pub src: Source,
    /// Start offset inside that buffer.
    pub start: usize,
    /// Length in bytes.
    pub len: usize,
}

impl Piece {
    /// Offset just past the piece, inside its buffer.
    pub fn end(&self) -> usize {
        self.start + self.len
    }

    /// True when the piece contributes no bytes.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

/// What [`Rope::stats`] reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RopeStats {
    /// Pieces in the list.
    pub pieces: usize,
    /// Bytes the document currently has.
    pub len: usize,
    /// Bytes in the immutable original buffer.
    pub original_bytes: usize,
    /// Bytes in the append-only add buffer (including bytes no piece references any more).
    pub add_bytes: usize,
    /// Longest single run of untouched original bytes.
    pub longest_original_run: usize,
    /// Number of untouched original runs — one `write`/`Blob` part each when saving.
    pub original_runs: usize,
}

/// A document plus its edits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rope {
    original: Vec<u8>,
    add: Vec<u8>,
    pieces: Vec<Piece>,
    len: usize,
}

impl Rope {
    /// Take ownership of `original` as the immutable base buffer.
    pub fn new(original: Vec<u8>) -> Rope {
        let len = original.len();
        let mut pieces = Vec::with_capacity(1);
        if len > 0 {
            pieces.push(Piece {
                src: Source::Original,
                start: 0,
                len,
            });
        }
        Rope {
            original,
            add: Vec::new(),
            pieces,
            len,
        }
    }

    /// Copy `bytes` into a new base buffer.
    pub fn from_slice(bytes: &[u8]) -> Rope {
        Rope::new(bytes.to_vec())
    }

    /// An empty rope.
    pub fn empty() -> Rope {
        Rope::new(Vec::new())
    }

    /// Current document length in bytes.
    pub fn len(&self) -> usize {
        self.len
    }

    /// True when the document is empty.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The piece list.
    pub fn pieces(&self) -> &[Piece] {
        &self.pieces
    }

    /// The immutable base buffer.
    pub fn original(&self) -> &[u8] {
        &self.original
    }

    /// The append-only edit buffer.
    pub fn added(&self) -> &[u8] {
        &self.add
    }

    /// Bytes of the base buffer that are still referenced, over the document length.
    ///
    /// `1.0` means nothing was edited; a 3-byte insert in a 10 GiB file is still
    /// `≈ 1.0`, which is why saving it costs one copy of the *destination*.
    pub fn unchanged_ratio(&self) -> f64 {
        if self.len == 0 {
            return 1.0;
        }
        let kept: usize = self
            .pieces
            .iter()
            .filter(|p| p.src == Source::Original)
            .map(|p| p.len)
            .sum();
        kept as f64 / self.len as f64
    }

    /// Untouched runs of the original, in document order: `(start, len)` inside
    /// [`Rope::original`]. A streamed save writes these directly and only the `Add` runs
    /// come from the edit buffer.
    pub fn original_runs(&self) -> Vec<(usize, usize)> {
        self.pieces
            .iter()
            .filter(|p| p.src == Source::Original)
            .map(|p| (p.start, p.len))
            .collect()
    }

    /// Counters for the status line and the performance gates.
    pub fn stats(&self) -> RopeStats {
        let mut s = RopeStats {
            pieces: self.pieces.len(),
            len: self.len,
            original_bytes: self.original.len(),
            add_bytes: self.add.len(),
            longest_original_run: 0,
            original_runs: 0,
        };
        for p in &self.pieces {
            if p.src == Source::Original {
                s.original_runs += 1;
                if p.len > s.longest_original_run {
                    s.longest_original_run = p.len;
                }
            }
        }
        s
    }

    /// The whole document as a fresh buffer. O(document) — used by tests and by small
    /// documents; large ones are saved with [`Rope::try_each_chunk`].
    pub fn to_vec(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.len);
        self.each_chunk(|c| out.extend_from_slice(c));
        out
    }

    /// Hand every piece's bytes to `f`, in document order.
    pub fn each_chunk<F: FnMut(&[u8])>(&self, mut f: F) {
        for p in &self.pieces {
            let buf = match p.src {
                Source::Original => &self.original,
                Source::Add => &self.add,
            };
            f(&buf[p.start..p.end()]);
        }
    }

    /// [`Rope::each_chunk`] with a fallible callback — the streamed-save path:
    ///
    /// ```ignore
    /// rope.try_each_chunk(|chunk| writer.write_all(chunk).map_err(|e| e.to_string()))?;
    /// ```
    pub fn try_each_chunk<E, F: FnMut(&[u8]) -> Result<(), E>>(&self, mut f: F) -> Result<(), E> {
        for p in &self.pieces {
            let buf = match p.src {
                Source::Original => &self.original,
                Source::Add => &self.add,
            };
            f(&buf[p.start..p.end()])?;
        }
        Ok(())
    }

    /// The byte at document offset `i`.
    pub fn byte_at(&self, i: usize) -> Option<u8> {
        let (p, off) = self.locate(i);
        let piece = self.pieces.get(p)?;
        let buf = match piece.src {
            Source::Original => &self.original,
            Source::Add => &self.add,
        };
        buf.get(piece.start + off).copied()
    }

    /// A copy of `range`, clamped to the document.
    pub fn slice(&self, range: Range<usize>) -> Vec<u8> {
        let from = range.start.min(self.len);
        let to = range.end.min(self.len);
        let mut out = Vec::with_capacity(to.saturating_sub(from));
        if from >= to {
            return out;
        }
        let mut pos = 0usize;
        for p in &self.pieces {
            let piece_end = pos + p.len;
            if piece_end <= from {
                pos = piece_end;
                continue;
            }
            if pos >= to {
                break;
            }
            let buf = match p.src {
                Source::Original => &self.original,
                Source::Add => &self.add,
            };
            let s = p.start + from.saturating_sub(pos);
            let e = p.start + (to - pos).min(p.len);
            out.extend_from_slice(&buf[s..e]);
            pos = piece_end;
        }
        out
    }

    /// Insert `bytes` at document offset `at` (clamped).
    pub fn insert(&mut self, at: usize, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let at = at.min(self.len);
        let i = self.split_at(at);
        let start = self.add.len();
        self.add.extend_from_slice(bytes);
        self.pieces.insert(
            i,
            Piece {
                src: Source::Add,
                start,
                len: bytes.len(),
            },
        );
        self.len += bytes.len();
        self.coalesce();
    }

    /// Delete `range` (clamped). The bytes stay in their buffers; only the piece list
    /// changes, which is what makes an undo stack cheap.
    pub fn delete(&mut self, range: Range<usize>) {
        let from = range.start.min(self.len);
        let to = range.end.min(self.len);
        if from >= to {
            return;
        }
        let i = self.split_at(from);
        let j = self.split_at(to);
        if j > i {
            self.pieces.drain(i..j);
        }
        self.len -= to - from;
        self.coalesce();
    }

    /// Delete `range` and insert `bytes` in its place.
    pub fn replace(&mut self, range: Range<usize>, bytes: &[u8]) {
        let at = range.start.min(self.len);
        self.delete(range);
        self.insert(at, bytes);
    }

    /// Merge adjacent pieces that come from the same buffer and are contiguous there.
    /// Returns how many pieces disappeared. Runs automatically after every mutation.
    pub fn coalesce(&mut self) -> usize {
        let mut out: Vec<Piece> = Vec::with_capacity(self.pieces.len());
        let mut removed = 0usize;
        for p in &self.pieces {
            if p.len == 0 {
                removed += 1;
                continue;
            }
            let merged = match out.last_mut() {
                Some(last) if last.src == p.src && last.end() == p.start => {
                    last.len += p.len;
                    true
                }
                _ => false,
            };
            if merged {
                removed += 1;
            } else {
                out.push(*p);
            }
        }
        self.pieces = out;
        removed
    }

    // ------------------------------------------------------------ line-oriented helpers

    /// Number of lines: one more than the number of `\n` bytes.
    pub fn line_count(&self) -> usize {
        let mut n = 1usize;
        self.each_chunk(|c| {
            for b in c {
                if *b == b'\n' {
                    n += 1;
                }
            }
        });
        n
    }

    /// Start offset of every line (0-based line numbers), capped at `limit` entries.
    pub fn line_starts(&self, limit: usize) -> Vec<usize> {
        let mut out = Vec::new();
        if limit == 0 {
            return out;
        }
        out.push(0usize);
        let mut off = 0usize;
        self.each_chunk(|c| {
            for b in c {
                off += 1;
                if *b == b'\n' && out.len() < limit {
                    out.push(off);
                }
            }
        });
        out
    }

    /// Byte range of line `line` (0-based), excluding its trailing `\n`.
    pub fn line_range(&self, line: usize) -> Option<Range<usize>> {
        let starts = self.line_starts(line + 2);
        let start = *starts.get(line)?;
        let end = match starts.get(line + 1) {
            Some(next) => next.saturating_sub(1),
            None => self.len,
        };
        Some(start..end.max(start))
    }

    /// The bytes of line `line` without its terminator.
    pub fn line(&self, line: usize) -> Option<Vec<u8>> {
        let r = self.line_range(line)?;
        Some(self.slice(r))
    }

    /// Insert `text` as a new line after line `line` (0-based), adding the newline that
    /// separates them. Mirrors the browser model's `insertLineAfter`.
    pub fn insert_line_after(&mut self, line: usize, text: &[u8]) {
        let Some(r) = self.line_range(line) else {
            return;
        };
        if r.end < self.len && self.byte_at(r.end) == Some(b'\n') {
            let mut buf = Vec::with_capacity(text.len() + 1);
            buf.extend_from_slice(text);
            buf.push(b'\n');
            self.insert(r.end + 1, &buf);
        } else {
            let mut buf = Vec::with_capacity(text.len() + 1);
            buf.push(b'\n');
            buf.extend_from_slice(text);
            self.insert(r.end, &buf);
        }
    }

    /// Delete line `line` (0-based) together with the newline that separates it from its
    /// neighbour. Mirrors the browser model's `deleteLine`.
    pub fn delete_line(&mut self, line: usize) {
        let Some(r) = self.line_range(line) else {
            return;
        };
        if r.end < self.len && self.byte_at(r.end) == Some(b'\n') {
            self.delete(r.start..r.end + 1);
        } else if r.start > 0 {
            self.delete(r.start - 1..r.end);
        } else {
            self.delete(r.start..r.end);
        }
    }

    // ------------------------------------------------------------------------ internals

    /// Piece containing document offset `at`, and the offset inside it.
    /// Returns `(pieces.len(), 0)` for the append point.
    fn locate(&self, at: usize) -> (usize, usize) {
        let mut acc = 0usize;
        for (i, p) in self.pieces.iter().enumerate() {
            if at < acc + p.len {
                return (i, at - acc);
            }
            acc += p.len;
        }
        (self.pieces.len(), 0)
    }

    /// Make sure a piece boundary sits at document offset `at`; return the index of the
    /// piece that now starts there.
    fn split_at(&mut self, at: usize) -> usize {
        let (i, off) = self.locate(at);
        if i >= self.pieces.len() {
            return self.pieces.len();
        }
        if off == 0 {
            return i;
        }
        let p = self.pieces[i];
        self.pieces[i].len = off;
        self.pieces.insert(
            i + 1,
            Piece {
                src: p.src,
                start: p.start + off,
                len: p.len - off,
            },
        );
        i + 1
    }
}

impl Default for Rope {
    fn default() -> Self {
        Rope::empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_rope_is_one_piece() {
        let r = Rope::from_slice(b"<a/>");
        assert_eq!(r.len(), 4);
        assert_eq!(r.pieces().len(), 1);
        assert_eq!(r.to_vec(), b"<a/>");
        assert_eq!(r.unchanged_ratio(), 1.0);
        assert_eq!(r.original_runs(), alloc::vec![(0, 4)]);
        assert!(Rope::empty().is_empty());
        assert_eq!(Rope::empty().pieces().len(), 0);
        assert_eq!(Rope::default(), Rope::empty());
    }

    #[test]
    fn one_insert_splits_the_original_in_three() {
        let mut r = Rope::from_slice(b"<a></a>");
        r.insert(3, b"hi");
        assert_eq!(r.to_vec(), b"<a>hi</a>");
        assert_eq!(r.len(), 9);
        assert_eq!(r.pieces().len(), 3);
        assert_eq!(r.original_runs(), alloc::vec![(0, 3), (3, 4)]);
        assert!((r.unchanged_ratio() - 7.0 / 9.0).abs() < 1e-9);
        assert_eq!(r.byte_at(3), Some(b'h'));
        assert_eq!(r.byte_at(9), None);
        assert_eq!(r.slice(2..6), b">hi<".to_vec());
    }

    #[test]
    fn adjacent_inserts_coalesce() {
        let mut r = Rope::from_slice(b"xy");
        r.insert(1, b"a");
        r.insert(2, b"b"); // lands right after "a" in both document and add buffer
        assert_eq!(r.to_vec(), b"xaby");
        assert_eq!(r.pieces().len(), 3, "the two inserts became one add piece");
    }

    #[test]
    fn delete_and_replace() {
        let mut r = Rope::from_slice(b"<root>text</root>");
        r.delete(6..10);
        assert_eq!(r.to_vec(), b"<root></root>");
        r.replace(6..6, b"new");
        assert_eq!(r.to_vec(), b"<root>new</root>");
        // Clamped, empty and inverted ranges are no-ops.
        let len = r.len();
        r.delete(1000..2000);
        r.delete(5..5);
        r.delete(9..3);
        assert_eq!(r.len(), len);
        assert_eq!(r.to_vec(), b"<root>new</root>");
        r.delete(0..r.len());
        assert!(r.is_empty());
        assert_eq!(r.unchanged_ratio(), 1.0);
    }

    #[test]
    fn streamed_save_visits_every_byte_once() {
        let mut r = Rope::from_slice(b"<a>0123456789</a>");
        r.insert(3, b"XYZ");
        r.delete(10..12);
        let mut chunks = Vec::new();
        r.try_each_chunk::<(), _>(|c| {
            chunks.push(c.to_vec());
            Ok(())
        })
        .unwrap();
        let joined: Vec<u8> = chunks.concat();
        assert_eq!(joined, r.to_vec());
        assert_eq!(chunks.len(), r.pieces().len());
        // An error stops the stream.
        let err = r
            .try_each_chunk::<&str, _>(|_| Err("disk full"))
            .unwrap_err();
        assert_eq!(err, "disk full");
    }

    #[test]
    fn line_helpers_match_the_bytes() {
        let mut r = Rope::from_slice(b"<a>\n<b/>\n</a>\n");
        assert_eq!(r.line_count(), 4); // three newlines → the last "line" is empty
        assert_eq!(r.line_starts(10), alloc::vec![0, 4, 9, 14]);
        assert_eq!(r.line_range(0), Some(0..3));
        assert_eq!(r.line(1).as_deref(), Some(&b"<b/>"[..]));
        r.insert_line_after(1, b"<c/>");
        assert_eq!(r.to_vec(), b"<a>\n<b/>\n<c/>\n</a>\n");
        r.delete_line(2);
        assert_eq!(r.to_vec(), b"<a>\n<b/>\n</a>\n");
        // Appending after the last line adds the separator first.
        r.delete_line(3);
        r.insert_line_after(2, b"<!-- tail -->");
        assert_eq!(r.to_vec(), b"<a>\n<b/>\n</a>\n<!-- tail -->");
        assert_eq!(r.line_count(), 4);
        assert!(r.line(99).is_none());
    }

    #[test]
    fn stats_describe_the_buffers() {
        let mut r = Rope::from_slice(b"0123456789");
        r.insert(5, b"ab");
        r.delete(0..2);
        let s = r.stats();
        assert_eq!(s.pieces, r.pieces().len());
        assert_eq!(s.len, r.len());
        assert_eq!(s.original_bytes, 10);
        assert_eq!(s.add_bytes, 2);
        assert_eq!(s.original_runs, r.original_runs().len());
        assert!(s.longest_original_run > 0);
        assert!(Piece {
            src: Source::Add,
            start: 0,
            len: 0
        }
        .is_empty());
        assert_eq!(
            Piece {
                src: Source::Original,
                start: 2,
                len: 3
            }
            .end(),
            5
        );
    }
}
