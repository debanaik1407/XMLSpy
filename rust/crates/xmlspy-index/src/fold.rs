//! Code folding, bracket matching and bookmarks — the editor features that the structural
//! index can answer without a DOM and, mostly, without touching the document again.
//!
//! Everything here is a pure function of [`StructuralIndex`] (plus the document bytes where
//! a *line* is needed, because the index stores checkpoints, not the text). That keeps the
//! three features honest about cost:
//!
//! * [`fold_regions`] is one forward pass over the bytes between the first and last
//!   element end — `O(document)` in the worst case, `O(viewport)` when the caller uses
//!   [`fold_regions_in`] for the lines it is about to paint, which is what the text view
//!   does while scrolling a 10 GiB file;
//! * [`bracket_at`] is a binary search plus a walk up the parent chain, so "Ctrl+] to the
//!   matching tag" is `O(log n + depth)` and never scans the document except for the one
//!   tag it lands on;
//! * [`Bookmarks`] and [`FoldSet`] are sorted sets with a tiny stable binary encoding
//!   (`XBK1` / `XFD1`), which is what session restore persists: a bookmark is a *line*,
//!   a collapsed fold is the *start offset* of the region, and both survive a re-index of
//!   an unchanged document.
//!
//! Comments, processing instructions and CDATA sections are not folded: the index records
//! elements only. Folding those would mean storing their ranges too, which is a `.xsi` v2
//! conversation, not something to fake here.

use alloc::vec::Vec;

use crate::{StructuralIndex, END_PENDING, END_UNKNOWN, NO_PARENT};

/// Magic of the [`Bookmarks`] encoding.
pub const MAGIC_BOOKMARKS: [u8; 4] = *b"XBK1";

/// Magic of the [`FoldSet`] encoding.
pub const MAGIC_FOLDS: [u8; 4] = *b"XFD1";

/// True when an `elem_end` value is a usable offset.
///
/// [`END_UNKNOWN`] (still open) and [`END_PENDING`] (closed by a mismatched end tag whose
/// `>` was never read) are both "no offset here", and code that folds or jumps must not
/// treat either as one.
#[inline]
#[must_use]
pub fn is_closed(end: u64) -> bool {
    end < END_PENDING
}

// ---------------------------------------------------------------- offsets -> lines

/// 1-based line containing byte `off`, using the index's line checkpoints.
///
/// `checkpoints[k]` is the start offset of line `k * stride + 1`, so the answer is the
/// largest such checkpoint at or below `off`, plus the newlines between it and `off`.
/// Clamped: an offset past the end of `bytes` reports the last line.
#[must_use]
pub fn line_at(ix: &StructuralIndex, bytes: &[u8], off: u64) -> u64 {
    let len = bytes.len() as u64;
    let off = off.min(len);
    let stride = u64::from(ix.stride.max(1));
    let k = match ix.checkpoints.binary_search(&off) {
        Ok(i) => i,
        Err(0) => 0,
        Err(i) => i - 1,
    };
    let base_off = ix
        .checkpoints
        .get(k)
        .copied()
        .unwrap_or(0)
        .min(off)
        .min(len) as usize;
    let base_line = k as u64 * stride + 1;
    let newlines = bytes[base_off..off as usize]
        .iter()
        .filter(|&&b| b == b'\n')
        .count() as u64;
    base_line + newlines
}

/// Byte offset where 1-based `line` starts — the inverse of [`line_at`].
///
/// [`StructuralIndex::seek_line`] works in 0-based lines (its checkpoint convention), so
/// this converts and then walks the few newlines between the checkpoint and the line.
/// `None` when the line is past the end of `bytes`.
#[must_use]
pub fn line_offset(ix: &StructuralIndex, bytes: &[u8], line: u64) -> Option<u64> {
    if line == 0 {
        return None;
    }
    let (off, skip) = ix.seek_line(line - 1)?;
    let mut pos = (off as usize).min(bytes.len());
    let mut left = skip;
    while left > 0 && pos < bytes.len() {
        if bytes[pos] == b'\n' {
            left -= 1;
        }
        pos += 1;
    }
    (left == 0).then_some(pos as u64)
}

/// 1-based lines for a batch of offsets, in one forward pass.
///
/// The result is parallel to `offs` (order preserved, input may be unsorted). Starting from
/// the checkpoint below the smallest offset is what makes this cheap for a sparse batch —
/// ten fold markers in a 10 GiB document cost one short scan, not ten.
#[must_use]
pub fn lines_for(ix: &StructuralIndex, bytes: &[u8], offs: &[u64]) -> Vec<u64> {
    let mut out = alloc::vec![1u64; offs.len()];
    if offs.is_empty() {
        return out;
    }
    let mut order: Vec<usize> = (0..offs.len()).collect();
    order.sort_by_key(|&i| offs[i]);

    let len = bytes.len();
    let mut pos = (offs[order[0]] as usize).min(len);
    let mut line = line_at(ix, bytes, pos as u64);
    for i in order {
        let target = (offs[i] as usize).min(len);
        while pos < target {
            if bytes[pos] == b'\n' {
                line += 1;
            }
            pos += 1;
        }
        out[i] = line;
    }
    out
}

// ---------------------------------------------------------------- folding

/// One collapsible region: an element that spans more than one line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FoldRegion {
    /// Index slot of the element (an argument to [`StructuralIndex::children_of`] etc.).
    pub id: u32,
    /// Its name, as an index into [`StructuralIndex::names`].
    pub name: u32,
    /// 1-based line of the start tag.
    pub start_line: u64,
    /// 1-based line of the end tag (the last line when the element was never closed).
    pub end_line: u64,
    /// Byte offset of the element's `<`.
    pub start_off: u64,
    /// Byte offset just past the end tag, or the document length when [`FoldRegion::unclosed`].
    pub end_off: u64,
    /// True when the index does not know where the element ends (unclosed or malformed).
    pub unclosed: bool,
}

impl FoldRegion {
    /// Lines this region spans, inclusive.
    #[must_use]
    pub fn span(&self) -> u64 {
        self.end_line.saturating_sub(self.start_line) + 1
    }
}

/// Fold regions of the whole document, in document order.
///
/// `min_lines` is the smallest span worth a fold marker; anything below 2 is treated as 2,
/// because an element on a single line has nothing to hide.
#[must_use]
pub fn fold_regions(ix: &StructuralIndex, bytes: &[u8], min_lines: u64) -> Vec<FoldRegion> {
    fold_regions_in(ix, bytes, min_lines, 1, u64::MAX)
}

/// Fold regions whose lines intersect `from_line..=to_line` — what the text view calls for
/// the viewport it is about to paint.
#[must_use]
pub fn fold_regions_in(
    ix: &StructuralIndex,
    bytes: &[u8],
    min_lines: u64,
    from_line: u64,
    to_line: u64,
) -> Vec<FoldRegion> {
    let min_lines = min_lines.max(2);
    let n = ix.elem_start.len();
    let doc_end = ix.file_len.max(bytes.len() as u64);

    let mut ends: Vec<u64> = Vec::with_capacity(n);
    let mut unclosed: Vec<bool> = alloc::vec![false; n];
    for i in 0..n {
        let e = ix.elem_end.get(i).copied().unwrap_or(END_UNKNOWN);
        if is_closed(e) {
            ends.push(e.min(doc_end));
        } else {
            ends.push(doc_end);
            unclosed[i] = true;
        }
    }
    let end_lines = lines_for(ix, bytes, &ends);

    let mut out = Vec::new();
    for i in 0..n {
        let start_line = *ix.elem_line.get(i).unwrap_or(&1);
        if start_line > to_line {
            continue;
        }
        let end_line = end_lines[i].max(start_line);
        if end_line < from_line {
            continue;
        }
        let span = end_line - start_line + 1;
        if span < min_lines {
            continue;
        }
        out.push(FoldRegion {
            id: i as u32,
            name: *ix.elem_name.get(i).unwrap_or(&0),
            start_line,
            end_line,
            start_off: ix.elem_start[i],
            end_off: ends[i],
            unclosed: unclosed[i],
        });
    }
    out
}

/// Which fold regions are collapsed right now.
///
/// Keyed by the region's start offset, which is stable for an unchanged document (and
/// re-derived from the index when it changes, so a stale entry simply stops matching).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FoldSet {
    starts: Vec<u64>,
}

impl FoldSet {
    /// An empty set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Collapse `region` (idempotent).
    pub fn collapse(&mut self, region: &FoldRegion) {
        self.insert(region.start_off);
    }

    /// Expand `region` (idempotent).
    pub fn expand(&mut self, region: &FoldRegion) {
        self.remove(region.start_off);
    }

    /// True when the region starting at `start_off` is collapsed.
    #[must_use]
    pub fn is_collapsed(&self, start_off: u64) -> bool {
        self.starts.binary_search(&start_off).is_ok()
    }

    fn insert(&mut self, v: u64) {
        if let Err(i) = self.starts.binary_search(&v) {
            self.starts.insert(i, v);
        }
    }

    fn remove(&mut self, v: u64) {
        if let Ok(i) = self.starts.binary_search(&v) {
            self.starts.remove(i);
        }
    }

    /// Collapse or expand, returning the new state (`true` = collapsed).
    pub fn toggle(&mut self, start_off: u64) -> bool {
        match self.starts.binary_search(&start_off) {
            Ok(i) => {
                self.starts.remove(i);
                false
            }
            Err(i) => {
                self.starts.insert(i, start_off);
                true
            }
        }
    }

    /// Collapsed start offsets, ascending.
    #[must_use]
    pub fn starts(&self) -> &[u64] {
        &self.starts
    }

    /// Number of collapsed regions.
    #[must_use]
    pub fn len(&self) -> usize {
        self.starts.len()
    }

    /// True when nothing is collapsed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.starts.is_empty()
    }

    /// Drop every collapsed region (what "Expand All" does).
    pub fn clear(&mut self) {
        self.starts.clear();
    }

    /// Regions of `ix` that this set collapses, in document order.
    #[must_use]
    pub fn collapsed_regions(&self, ix: &StructuralIndex, bytes: &[u8]) -> Vec<FoldRegion> {
        fold_regions(ix, bytes, 2)
            .into_iter()
            .filter(|r| self.is_collapsed(r.start_off))
            .collect()
    }

    /// Stable binary form (`XFD1` + count + ascending `u64` offsets) for session restore.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        encode_set(&MAGIC_FOLDS, &self.starts)
    }

    /// Inverse of [`FoldSet::encode`]; `None` when the buffer is not one.
    #[must_use]
    pub fn decode(buf: &[u8]) -> Option<Self> {
        decode_set(&MAGIC_FOLDS, buf).map(|starts| Self { starts })
    }
}

// ---------------------------------------------------------------- bracket matching

/// A matched pair of tags: the element whose start tag and end tag the editor highlights.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BracketPair {
    /// Index slot of the element.
    pub id: u32,
    /// Offset of the start tag's `<`.
    pub open: u64,
    /// Offset just past the end tag's `>` (the document length when unclosed).
    pub close: u64,
    /// True when the index does not know where the element ends.
    pub unclosed: bool,
}

/// Just past the `>` of the tag that starts at `at`, ignoring `>` inside quoted attribute
/// values. Returns `bytes.len()` when the tag never closes.
#[must_use]
pub fn tag_end(bytes: &[u8], at: usize) -> usize {
    let mut quote = 0u8;
    let mut k = at.min(bytes.len());
    while k < bytes.len() {
        let b = bytes[k];
        if quote != 0 {
            if b == quote {
                quote = 0;
            }
        } else if b == b'"' || b == b'\'' {
            quote = b;
        } else if b == b'>' {
            return k + 1;
        }
        k += 1;
    }
    bytes.len()
}

/// The element whose start **or** end tag contains the byte at `off`.
///
/// This is bracket matching as an editor does it: the caret on `<a …>` or on `</a>` lights
/// up both tags. A caret in character data matches nothing (use [`enclosing`] for "which
/// element am I in"). `O(log n + depth)`, plus a scan of the one tag it lands on.
#[must_use]
pub fn bracket_at(ix: &StructuralIndex, bytes: &[u8], off: u64) -> Option<BracketPair> {
    let mut i = match ix.elem_start.binary_search(&off) {
        Ok(k) => k,
        Err(0) => return None,
        Err(k) => k - 1,
    };
    loop {
        let start = ix.elem_start[i];
        if off < tag_end(bytes, start as usize) as u64 {
            return Some(pair(ix, i, bytes));
        }
        let e = ix.elem_end.get(i).copied().unwrap_or(END_UNKNOWN);
        if is_closed(e) {
            let e = (e as usize).min(bytes.len());
            if off >= end_tag_start(bytes, e) as u64 && off < e as u64 {
                return Some(pair(ix, i, bytes));
            }
        }
        let p = ix.elem_parent[i];
        // Parents always have a smaller slot than their children (slots are handed out in
        // document order), which also makes this walk terminate on a corrupt index.
        if p <= NO_PARENT || p as usize >= i {
            return None;
        }
        i = p as usize;
    }
}

/// Innermost element containing `off` (start tag, content or end tag).
#[must_use]
pub fn enclosing(ix: &StructuralIndex, off: u64) -> Option<u32> {
    let mut i = match ix.elem_start.binary_search(&off) {
        Ok(k) => k,
        Err(0) => return None,
        Err(k) => k - 1,
    };
    loop {
        let e = ix.elem_end.get(i).copied().unwrap_or(END_UNKNOWN);
        let end = if is_closed(e) { e } else { ix.file_len.max(off + 1) };
        if off < end {
            return Some(i as u32);
        }
        let p = ix.elem_parent[i];
        if p <= NO_PARENT || p as usize >= i {
            return None;
        }
        i = p as usize;
    }
}

fn pair(ix: &StructuralIndex, i: usize, bytes: &[u8]) -> BracketPair {
    let e = ix.elem_end.get(i).copied().unwrap_or(END_UNKNOWN);
    let closed = is_closed(e);
    BracketPair {
        id: i as u32,
        open: ix.elem_start[i],
        close: if closed {
            e.min(ix.file_len.max(bytes.len() as u64))
        } else {
            ix.file_len.max(bytes.len() as u64)
        },
        unclosed: !closed,
    }
}

/// Where the end tag that finishes at `end` (just past its `>`) begins.
fn end_tag_start(bytes: &[u8], end: usize) -> usize {
    // A tag longer than this is not a tag; the bound keeps a malformed document from
    // turning one bracket match into a scan of the whole file.
    let floor = end.saturating_sub(4096);
    let mut k = end.min(bytes.len());
    while k > floor {
        k -= 1;
        if bytes[k] == b'<' {
            return k;
        }
    }
    floor
}

// ---------------------------------------------------------------- bookmarks

/// Bookmarked lines: Ctrl+F2 toggles one, F2 / Shift+F2 walk them, wrapping at the ends.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Bookmarks {
    lines: Vec<u64>,
}

impl Bookmarks {
    /// No bookmarks.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// From any iterator of 1-based line numbers (duplicates dropped, order irrelevant).
    #[must_use]
    pub fn from_lines<I: IntoIterator<Item = u64>>(lines: I) -> Self {
        let mut v: Vec<u64> = lines.into_iter().collect();
        v.sort_unstable();
        v.dedup();
        Self { lines: v }
    }

    /// Bookmarked lines, ascending.
    #[must_use]
    pub fn lines(&self) -> &[u64] {
        &self.lines
    }

    /// How many lines are bookmarked.
    #[must_use]
    pub fn len(&self) -> usize {
        self.lines.len()
    }

    /// True when there are none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// True when `line` is bookmarked.
    #[must_use]
    pub fn contains(&self, line: u64) -> bool {
        self.lines.binary_search(&line).is_ok()
    }

    /// Bookmark `line`; true when it was not already bookmarked.
    pub fn add(&mut self, line: u64) -> bool {
        match self.lines.binary_search(&line) {
            Ok(_) => false,
            Err(i) => {
                self.lines.insert(i, line);
                true
            }
        }
    }

    /// Remove the bookmark on `line`; true when there was one.
    pub fn remove(&mut self, line: u64) -> bool {
        match self.lines.binary_search(&line) {
            Ok(i) => {
                self.lines.remove(i);
                true
            }
            Err(_) => false,
        }
    }

    /// Toggle `line` (Ctrl+F2); true when it is bookmarked afterwards.
    pub fn toggle(&mut self, line: u64) -> bool {
        if self.remove(line) {
            false
        } else {
            self.add(line)
        }
    }

    /// Clear every bookmark.
    pub fn clear(&mut self) {
        self.lines.clear();
    }

    /// The first bookmark after `line`, wrapping to the first one. `None` when there are no
    /// bookmarks at all.
    #[must_use]
    pub fn next(&self, line: u64) -> Option<u64> {
        if self.lines.is_empty() {
            return None;
        }
        match self.lines.binary_search(&line) {
            Ok(i) => Some(self.lines[(i + 1) % self.lines.len()]),
            Err(i) => Some(self.lines[i % self.lines.len()]),
        }
    }

    /// The last bookmark before `line`, wrapping to the last one.
    #[must_use]
    pub fn prev(&self, line: u64) -> Option<u64> {
        if self.lines.is_empty() {
            return None;
        }
        let n = self.lines.len();
        match self.lines.binary_search(&line) {
            Ok(i) => Some(self.lines[(i + n - 1) % n]),
            Err(0) => Some(self.lines[n - 1]),
            Err(i) => Some(self.lines[i - 1]),
        }
    }

    /// Stable binary form (`XBK1` + count + ascending `u64` lines) for session restore.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        encode_set(&MAGIC_BOOKMARKS, &self.lines)
    }

    /// Inverse of [`Bookmarks::encode`]; `None` when the buffer is not one.
    #[must_use]
    pub fn decode(buf: &[u8]) -> Option<Self> {
        decode_set(&MAGIC_BOOKMARKS, buf).map(|lines| Self { lines })
    }
}

// ---------------------------------------------------------------- shared encoding

fn encode_set(magic: &[u8; 4], v: &[u64]) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + v.len() * 8);
    out.extend_from_slice(magic);
    out.extend_from_slice(&(v.len() as u32).to_le_bytes());
    for x in v {
        out.extend_from_slice(&x.to_le_bytes());
    }
    out
}

fn decode_set(magic: &[u8; 4], buf: &[u8]) -> Option<Vec<u64>> {
    if buf.len() < 8 || &buf[0..4] != magic {
        return None;
    }
    let n = u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]) as usize;
    // Saturating: a corrupt count must be rejected, not overflow (debug builds panic).
    if buf.len() < 8usize.saturating_add(n.saturating_mul(8)) {
        return None;
    }
    let mut v = Vec::with_capacity(n);
    for i in 0..n {
        let at = 8 + i * 8;
        let mut a = [0u8; 8];
        a.copy_from_slice(&buf[at..at + 8]);
        v.push(u64::from_le_bytes(a));
    }
    v.sort_unstable();
    v.dedup();
    Some(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xsi;
    use alloc::string::String;

    /// `<root>` on line 1 … `</root>` on line 6, 48 bytes, fully indexed at stride 1.
    fn sample() -> (StructuralIndex, Vec<u8>) {
        let doc = b"<root>\n  <a>\n    <b>x</b>\n  </a>\n  <c/>\n</root>\n".to_vec();
        let ix = StructuralIndex {
            file_len: 48,
            checkpoints: alloc::vec![0, 7, 13, 26, 33, 40, 48],
            stride: 1,
            line_count: 7,
            elem_start: alloc::vec![0, 9, 17, 35],
            elem_end: alloc::vec![47, 32, 25, 39],
            elem_line: alloc::vec![1, 2, 3, 5],
            elem_parent: alloc::vec![NO_PARENT, 0, 1, 0],
            elem_name: alloc::vec![0, 1, 2, 3],
            elem_depth: alloc::vec![0, 1, 2, 1],
            names: alloc::vec![
                String::from("root"),
                String::from("a"),
                String::from("b"),
                String::from("c")
            ],
            indexed_elements: 4,
            total_elements: 4,
            total_attributes: 0,
            max_depth: 3,
            errors: alloc::vec![],
            error_count: 0,
        };
        (ix, doc)
    }

    #[test]
    fn offsets_map_to_lines() {
        let (ix, doc) = sample();
        assert_eq!(line_at(&ix, &doc, 0), 1);
        assert_eq!(line_at(&ix, &doc, 6), 1, "the newline belongs to the line it ends");
        assert_eq!(line_at(&ix, &doc, 7), 2);
        assert_eq!(line_at(&ix, &doc, 17), 3);
        assert_eq!(line_at(&ix, &doc, 35), 5);
        assert_eq!(line_at(&ix, &doc, 47), 6);
        assert_eq!(line_at(&ix, &doc, 48), 7, "past the last byte");
        assert_eq!(line_at(&ix, &doc, 10_000), 7, "clamped");
        assert_eq!(
            lines_for(&ix, &doc, &[47, 0, 17, 7]),
            alloc::vec![6, 1, 3, 2]
        );
        assert!(lines_for(&ix, &doc, &[]).is_empty());
    }

    #[test]
    fn line_offsets_are_the_inverse_of_line_at() {
        let (ix, doc) = sample();
        for line in 1..=ix.line_count {
            let off = line_offset(&ix, &doc, line).unwrap_or_else(|| panic!("line {line}"));
            assert_eq!(line_at(&ix, &doc, off), line, "round trip for line {line}");
        }
        assert_eq!(line_offset(&ix, &doc, 1), Some(0));
        assert_eq!(line_offset(&ix, &doc, 2), Some(7));
        assert_eq!(line_offset(&ix, &doc, 7), Some(48));
        assert_eq!(line_offset(&ix, &doc, 0), None, "lines are 1-based");
        assert_eq!(line_offset(&ix, &doc, 99), None, "past the end");
    }

    #[test]
    fn folds_are_the_multi_line_elements() {
        let (ix, doc) = sample();
        let f = fold_regions(&ix, &doc, 2);
        let names: Vec<&str> = f
            .iter()
            .map(|r| ix.names[r.name as usize].as_str())
            .collect();
        assert_eq!(names, alloc::vec!["root", "a"], "<b> and <c/> are single-line");
        assert_eq!(f[0].start_line, 1);
        assert_eq!(f[0].end_line, 6);
        assert_eq!(f[0].span(), 6);
        assert_eq!(f[0].end_off, 47);
        assert!(!f[0].unclosed);
        assert_eq!(f[1].start_line, 2);
        assert_eq!(f[1].end_line, 4);
        assert_eq!(f[1].start_off, 9);
        // min_lines filters by span
        assert_eq!(fold_regions(&ix, &doc, 4).len(), 1, "only <root> spans 4+ lines");
        assert!(fold_regions(&ix, &doc, 99).is_empty());
    }

    #[test]
    fn folds_can_be_restricted_to_a_line_range() {
        let (ix, doc) = sample();
        let mid = fold_regions_in(&ix, &doc, 2, 2, 5);
        assert_eq!(mid.len(), 2, "both <root> and <a> intersect lines 2..=5");
        let top = fold_regions_in(&ix, &doc, 2, 1, 1);
        assert_eq!(top.len(), 1, "only <root> starts on line 1");
        assert_eq!(top[0].id, 0);
        assert!(fold_regions_in(&ix, &doc, 2, 7, 9).is_empty());
    }

    #[test]
    fn an_unclosed_element_folds_to_the_end() {
        let (mut ix, doc) = sample();
        ix.elem_end[1] = END_UNKNOWN;
        ix.elem_end[2] = END_PENDING;
        let f = fold_regions(&ix, &doc, 2);
        assert_eq!(f.len(), 3, "<b> folds too once its end is unknown");
        assert!(!f[0].unclosed, "<root> still closes at 47");
        assert_eq!(f[0].end_off, 47);
        assert!(f[1].unclosed, "<a> has no known end");
        assert_eq!(f[1].end_off, 48, "…so it folds to the document length");
        assert_eq!(f[1].end_line, 7);
        assert!(f[2].unclosed, "END_PENDING is not an offset either");
        assert!(!is_closed(END_UNKNOWN) && !is_closed(END_PENDING));
        assert!(is_closed(47));
    }

    #[test]
    fn bracket_matching_finds_both_tags() {
        let (ix, doc) = sample();
        for off in 0u64..6 {
            let p = bracket_at(&ix, &doc, off).unwrap_or_else(|| panic!("start tag at {off}"));
            assert_eq!((p.id, p.open, p.close), (0, 0, 47), "offset {off}");
        }
        for off in 40u64..47 {
            let p = bracket_at(&ix, &doc, off).expect("end tag");
            assert_eq!((p.id, p.open, p.close), (0, 0, 47), "offset {off}");
        }
        let p = bracket_at(&ix, &doc, 9).expect("<a>");
        assert_eq!((p.id, p.open, p.close), (1, 9, 32));
        let p = bracket_at(&ix, &doc, 30).expect("</a>");
        assert_eq!((p.id, p.open, p.close), (1, 9, 32));
        let p = bracket_at(&ix, &doc, 35).expect("<c/>");
        assert_eq!((p.id, p.open, p.close), (3, 35, 39), "an empty tag matches itself");
        assert_eq!(bracket_at(&ix, &doc, 20), None, "character data matches nothing");
        assert_eq!(bracket_at(&ix, &doc, 7), None, "whitespace between elements");
        assert_eq!(bracket_at(&ix, &doc, 1000), None, "past the end");
    }

    #[test]
    fn enclosing_element() {
        let (ix, _) = sample();
        assert_eq!(enclosing(&ix, 0), Some(0));
        assert_eq!(enclosing(&ix, 20), Some(2), "inside <b>");
        assert_eq!(enclosing(&ix, 26), Some(1), "inside <a>, after </b>");
        assert_eq!(enclosing(&ix, 33), Some(0), "inside <root>, after </a>");
        assert_eq!(enclosing(&ix, 47), None, "after </root>");
    }

    #[test]
    fn tag_end_skips_quoted_angle_brackets() {
        let b = b"<a x=\">\" y='<' z=\"1\">text</a>";
        assert_eq!(tag_end(b, 0), 21);
        assert_eq!(&b[21..25], b"text");
        assert_eq!(tag_end(b"<unclosed", 0), 9, "runs off the end");
    }

    #[test]
    fn bookmarks_toggle_and_walk_with_wraparound() {
        let mut bm = Bookmarks::new();
        assert!(bm.is_empty());
        assert!(bm.toggle(10));
        assert!(bm.toggle(3));
        assert!(!bm.toggle(10), "second Ctrl+F2 clears it");
        assert_eq!(bm.lines(), &[3]);
        bm.add(7);
        bm.add(7);
        assert_eq!(bm.len(), 2);
        assert_eq!(bm.next(3), Some(7));
        assert_eq!(bm.next(7), Some(3), "wraps");
        assert_eq!(bm.next(4), Some(7));
        assert_eq!(bm.prev(7), Some(3));
        assert_eq!(bm.prev(3), Some(7), "wraps");
        assert_eq!(bm.prev(1), Some(7), "…and from before the first, wrapping to the last");
        assert!(bm.contains(7) && !bm.contains(4));
        assert!(bm.remove(7) && !bm.remove(7));
        bm.clear();
        assert!(bm.is_empty());
        assert_eq!(bm.next(1), None);
        assert_eq!(bm.prev(1), None);
        assert_eq!(Bookmarks::from_lines([5u64, 1, 5, 9]).lines(), &[1, 5, 9]);
    }

    #[test]
    fn sets_round_trip_through_their_encodings() {
        let bm = Bookmarks::from_lines([1, 2, 99, 1000]);
        assert_eq!(Bookmarks::decode(&bm.encode()), Some(bm.clone()));
        assert_eq!(Bookmarks::decode(b"nope"), None);
        assert_eq!(Bookmarks::decode(&MAGIC_BOOKMARKS), None, "truncated");
        assert_eq!(Bookmarks::decode(&bm.encode()[..9]), None, "short payload");

        let mut fs = FoldSet::new();
        fs.toggle(9);
        fs.toggle(0);
        fs.toggle(9);
        fs.toggle(47);
        assert_eq!(fs.starts(), &[0, 47]);
        assert_eq!(fs.len(), 2);
        assert!(fs.is_collapsed(47) && !fs.is_collapsed(9));
        assert_eq!(FoldSet::decode(&fs.encode()), Some(fs.clone()));
        assert_eq!(FoldSet::decode(&bm.encode()), None, "wrong magic");

        let (ix, doc) = sample();
        let collapsed = fs.collapsed_regions(&ix, &doc);
        assert_eq!(collapsed.len(), 1, "only <root> starts at 0 and is multi-line");
        assert_eq!(collapsed[0].id, 0);
        fs.clear();
        assert!(fs.collapsed_regions(&ix, &doc).is_empty());
    }

    #[test]
    fn folds_survive_the_xsi_round_trip() {
        let (ix, doc) = sample();
        let back = xsi::decode(&xsi::encode(&ix, false)).expect("decode");
        assert_eq!(fold_regions(&back, &doc, 2), fold_regions(&ix, &doc, 2));
    }

    #[test]
    fn an_empty_index_has_nothing_to_fold() {
        let ix = StructuralIndex::default();
        assert!(fold_regions(&ix, b"", 2).is_empty());
        assert_eq!(bracket_at(&ix, b"", 0), None);
        assert_eq!(enclosing(&ix, 0), None);
        assert_eq!(line_at(&ix, b"", 0), 1);
        assert!(lines_for(&ix, b"abc", &[0, 3]).iter().all(|&l| l == 1));
    }
}
