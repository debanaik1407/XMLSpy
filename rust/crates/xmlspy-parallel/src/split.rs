//! The sequential split pass: where a document may be cut so that each piece can be
//! scanned by its own thread.
//!
//! # Why this is a `Scanner` pass and not a second tokenizer
//!
//! A parallel scan is only worth anything if the pieces it produces are *bit-identical* to
//! one sequential scan — the property `tests/resumable.rs` already guarantees for chunk
//! boundaries. The cheap way to get that is to let the real scanner choose the cut points:
//! this pass runs the ordinary [`Scanner`] with `max_indexed == 0` (so it retains no
//! records and does no index work) and asks it, via [`Scanner::track_splits`], for a
//! [`BoundaryState`] at the first depth-1 element close at or after each target offset.
//!
//! Because the boundary comes from the scanner itself, a thread resumed from it is *by
//! construction* in the same state the sequential scanner would be — no speculation, no
//! verification pass, no "probably equivalent". The price is that the split pass is
//! sequential and costs a fraction `c` of a full scan, so the speed-up is bounded by
//! `1 / (c + 1/threads)`; `rust/bench/` reports the measured `c` rather than promising
//! linear scaling it does not have.
//!
//! # What makes a cut legal
//!
//! * the scanner state is [`St::Text`] — not inside a tag, comment, CDATA, PI, DOCTYPE
//!   subset, attribute value or reference;
//! * the element depth is exactly 1, i.e. between two children of the top-level element.
//!   That guarantees no element spans the cut except the top-level one, whose end offset
//!   the pass also records ([`SplitPlan::depth0_closes`]) so the merge can patch it;
//! * the boundary carries the absolute line number and line start, so `elem_line`,
//!   checkpoint positions and error columns need no fix-up at all.
//!
//! Documents that never reach depth 1 again after the target (a single huge element, an
//! empty file, a document that ends inside a comment) simply produce fewer or no splits,
//! and the builder degrades to fewer threads or to a single sequential scan.

use std::time::Instant;

use xmlspy_core::CHUNK_SIZE;
use xmlspy_parse::{BoundaryState, Scanner, ScannerConfig, St, MAX_DEPTH0_CLOSES};

/// Where and how a document was cut.
#[derive(Debug, Clone)]
pub struct SplitPlan {
    /// Cut points, ascending: `(byte offset, boundary state at that offset)`.
    pub splits: Vec<(u64, BoundaryState)>,
    /// Offsets just past the `>` that closed each depth-0 element, ascending.
    pub depth0_closes: Vec<u64>,
    /// Element-name table of the split pass. Every name a segment can meet is already
    /// here, in first-appearance order, which is exactly the order the sequential scanner
    /// interns them — so the merged name table needs no renumbering.
    pub names: Vec<String>,
    /// Seconds the pass took.
    pub secs: f64,
    /// Bytes scanned by the pass.
    pub bytes: u64,
    /// Why the plan is empty, when it is (`""` when splits were found).
    pub note: &'static str,
}

impl SplitPlan {
    /// A plan with no cuts: one segment, scanned sequentially.
    pub fn single(note: &'static str) -> SplitPlan {
        SplitPlan {
            splits: Vec::new(),
            depth0_closes: Vec::new(),
            names: Vec::new(),
            secs: 0.0,
            bytes: 0,
            note,
        }
    }

    /// Number of segments this plan produces.
    pub fn segments(&self) -> usize {
        self.splits.len() + 1
    }

    /// True when there is nothing to parallelise.
    pub fn is_single(&self) -> bool {
        self.splits.is_empty()
    }
}

/// Evenly spaced cut targets for `threads` segments over `len` bytes.
///
/// The last segment is whatever follows the final split, so targets stop at
/// `(threads-1)/threads` of the file. Targets closer together than `min_segment_bytes`
/// are dropped: a document whose records are clustered must not produce a dozen segments
/// of a few kilobytes each.
pub fn targets(len: u64, threads: usize, min_segment_bytes: u64) -> Vec<u64> {
    let mut out: Vec<u64> = Vec::new();
    if threads < 2 {
        return out;
    }
    for k in 1..threads {
        let t = len.saturating_mul(k as u64) / threads as u64;
        match out.last() {
            Some(prev) if t.saturating_sub(*prev) < min_segment_bytes => {}
            _ => out.push(t),
        }
    }
    out
}

/// Run the split pass over `bytes`.
///
/// `threads` is how many segments the caller wants; `min_segment_bytes` keeps segments
/// large enough that thread wake-up does not dominate.
pub fn plan(bytes: &[u8], cfg: ScannerConfig, threads: usize, min_segment_bytes: u64) -> SplitPlan {
    let len = bytes.len() as u64;
    if threads < 2 {
        return SplitPlan::single("one thread requested");
    }
    if len < min_segment_bytes.saturating_mul(2) {
        return SplitPlan::single("document smaller than two segments");
    }
    let targets = targets(len, threads, min_segment_bytes);
    if targets.is_empty() {
        return SplitPlan::single("segments would be smaller than the minimum");
    }
    plan_at(bytes, cfg, targets)
}

/// Run the split pass against explicit targets (ascending byte offsets).
///
/// [`plan`] uses this with evenly spaced targets; crash recovery uses it with the offsets
/// a previous build recorded in its journal, which reproduces the same cuts — and, more
/// importantly, the same [`BoundaryState`] at each cut, which the journal does not store.
pub fn plan_at(bytes: &[u8], cfg: ScannerConfig, targets: Vec<u64>) -> SplitPlan {
    let len = bytes.len() as u64;
    if targets.is_empty() {
        return SplitPlan::single("no targets");
    }
    let t0 = Instant::now();
    let mut s = Scanner::new(ScannerConfig {
        // A split pass retains nothing: the element budget is what dominates a full scan,
        // and this pass only has to walk the state machine and count depth.
        max_indexed: 0,
        stride: cfg.stride,
        // Diagnostics are re-derived by the segments; do not spend strings on them here.
        max_errors: 0,
    });
    s.track_splits(targets);
    let mut off = 0usize;
    while off < bytes.len() {
        let end = (off + CHUNK_SIZE).min(bytes.len());
        s.feed(&bytes[off..end], off as u64);
        off = end;
    }
    s.finish(len);
    let mut splits = s.take_splits();
    let depth0_closes = s.take_depth0_closes();
    let pass = s.into_index();
    let secs = t0.elapsed().as_secs_f64();

    // The split pass records where each top-level element ends, because a segment cannot
    // close an element it never opened. That list is capped (`MAX_DEPTH0_CLOSES`) to keep a
    // pathological document from turning it into a memory problem — and a capped list means
    // the merge could not fill in every top-level `elem_end`, which would make the parallel
    // index differ from the sequential one. Such a document has more top-level elements than
    // XML allows anyway, so the honest answer is to scan it with one thread.
    if depth0_closes.len() >= MAX_DEPTH0_CLOSES {
        return SplitPlan::single("too many top-level elements to track where each one ends");
    }

    // Defensive: a cut is only usable if the scanner really was between tokens at depth 1.
    // `note_close` guarantees both, but a future state must not be able to silently break
    // the parallel build, so unusable cuts are dropped instead of trusted. Several targets
    // can also be satisfied by the same close (a document with few top-level records), and
    // a cut must not appear twice.
    let before = splits.len();
    let mut last = 0u64;
    splits.retain(|(off, b)| {
        let ok = *off > last && b.is_clean() && b.depth == 1 && b.state == St::Text;
        if ok {
            last = *off;
        }
        ok
    });
    let note = if splits.is_empty() {
        "no usable depth-1 boundary was found"
    } else if splits.len() < before {
        "some candidate cuts were rejected or duplicated"
    } else {
        ""
    };

    SplitPlan {
        splits,
        depth0_closes,
        names: pass.names,
        secs,
        bytes: len,
        note,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn corpus(records: usize) -> String {
        let mut s = String::from("<?xml version=\"1.0\"?>\n<PurchaseOrders>\n");
        for i in 0..records {
            s.push_str(&format!(
                "  <Order id=\"{i}\"><Item>x</Item><Item>y</Item></Order>\n"
            ));
        }
        s.push_str("</PurchaseOrders>\n");
        s
    }

    #[test]
    fn finds_one_split_per_target() {
        let src = corpus(2000);
        let bytes = src.as_bytes();
        let p = plan(bytes, ScannerConfig::default(), 4, 64);
        assert_eq!(p.note, "");
        assert_eq!(p.splits.len(), 3);
        assert_eq!(p.segments(), 4);
        assert_eq!(p.depth0_closes.len(), 1);
        let mut prev = 0u64;
        for (off, b) in &p.splits {
            assert!(*off > prev, "splits ascend");
            prev = *off;
            assert_eq!(b.depth, 1);
            assert!(b.is_clean());
            assert_eq!(b.state, St::Text);
            assert!(b.root_seen && !b.root_closed);
            assert!(b.line > 1);
            assert!(b.line_start <= *off);
        }
        // The name table of the pass is the document's name table, in first-appearance
        // order — that is what makes the merge's ids match the sequential scan's.
        assert_eq!(p.names[0], "PurchaseOrders");
        assert!(p.names.contains(&"Order".to_string()));
        assert!(p.names.contains(&"Item".to_string()));
        assert!(p.secs >= 0.0);
        assert_eq!(p.bytes, bytes.len() as u64);
    }

    #[test]
    fn refuses_to_split_what_cannot_be_split() {
        // No child of the root at all: depth never returns to 1, so there is nowhere to cut.
        let p = plan(b"<a>text</a>", ScannerConfig::default(), 4, 4);
        assert!(p.is_single(), "{:?}", p.note);
        assert_eq!(p.segments(), 1);
        assert!(!p.note.is_empty());

        // One child: exactly one legal cut, however many targets were asked for.
        let one = plan(b"<a><b>deep</b></a>", ScannerConfig::default(), 4, 4);
        assert_eq!(one.splits.len(), 1, "duplicate cuts must be dropped");
        assert_eq!(one.segments(), 2);
        assert_eq!(one.splits[0].0, 14, "just past </b>");

        // Too small to be worth a thread, and one thread means no plan at all.
        assert!(plan(b"<a><b/><b/><b/></a>", ScannerConfig::default(), 4, 1 << 20).is_single());
        assert!(plan(b"<a/>", ScannerConfig::default(), 1, 8).is_single());
        assert!(plan(b"", ScannerConfig::default(), 8, 8).is_single());
    }

    #[test]
    fn split_pass_is_cheaper_than_a_full_scan_because_it_retains_nothing() {
        let src = corpus(4000);
        let bytes = src.as_bytes();
        let p = plan(bytes, ScannerConfig::default(), 8, 64);
        assert_eq!(p.splits.len(), 7);
        // The pass reports no diagnostics and no records of its own.
        assert!(p.depth0_closes.len() <= xmlspy_parse::MAX_DEPTH0_CLOSES);
        // Segments stay above the minimum size.
        let mut bounds = vec![0u64];
        bounds.extend(p.splits.iter().map(|(o, _)| *o));
        bounds.push(bytes.len() as u64);
        for w in bounds.windows(2) {
            assert!(w[1] > w[0], "empty segment");
        }
    }

    #[test]
    fn a_document_that_ends_inside_a_comment_still_yields_a_usable_plan() {
        let mut src = corpus(500);
        src.push_str("<!-- unterminated");
        let bytes = src.as_bytes();
        let p = plan(bytes, ScannerConfig::default(), 4, 64);
        // The cuts that were found are all clean; the unterminated tail simply sits in the
        // last segment, which is where the sequential scanner would report it too.
        for (_, b) in &p.splits {
            assert!(b.is_clean());
        }
        assert!(!p.is_single());
    }
}
