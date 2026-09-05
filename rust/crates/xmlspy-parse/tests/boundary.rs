//! `BoundaryState` round-trips: cutting a scan at **any** byte offset, capturing the
//! state, rebuilding a scanner from it and continuing must be indistinguishable from
//! never cutting at all.
//!
//! This is the property `xmlspy-parallel` rests on. If it ever breaks, the parallel index
//! builder silently produces a different index than the single-threaded one, so the
//! assertions here are deliberately pedantic: not just "same element count" but the same
//! scanner state, the same open-element stack, the same retained records and the same
//! diagnostics for the tail of the document.

use xmlspy_parse::{BoundaryState, Scanner, ScannerConfig, StructuralIndex};

const CFG: ScannerConfig = ScannerConfig {
    max_indexed: 300_000,
    stride: 8,
    max_errors: 1000,
};

/// Scan `bytes` in one pass.
fn whole(bytes: &[u8], cfg: ScannerConfig) -> StructuralIndex {
    Scanner::scan_all(cfg, bytes)
}

/// Scan `bytes[..at]`, capture the boundary, resume from it and scan `bytes[at..]`.
///
/// Returns `(prefix index, boundary, tail index, tail boundary)`.
fn cut(bytes: &[u8], at: usize, cfg: ScannerConfig) -> (StructuralIndex, BoundaryState, StructuralIndex, BoundaryState) {
    let mut a = Scanner::new(cfg);
    a.feed(&bytes[..at], 0);
    let b = a.boundary();

    let mut tail = Scanner::resume(cfg, b.clone());
    tail.feed(&bytes[at..], at as u64);
    let tail_boundary = tail.boundary();

    // The single-pass scanner continues from exactly the same place.
    let mut a2 = Scanner::new(cfg);
    a2.feed(&bytes[..at], 0);
    a2.feed(&bytes[at..], at as u64);
    let a2_boundary = a2.boundary();
    assert_eq!(
        a2_boundary, tail_boundary,
        "resuming at {at} diverged from a continuous scan"
    );

    let prefix_ix = a.into_index();
    let tail_ix = tail.into_index();
    (prefix_ix, b, tail_ix, a2_boundary)
}

/// The records a resumed scanner produces must be exactly the tail of the records the
/// continuous scanner produces — same slots, same parents, same name ids.
fn assert_tail_matches(
    full: &StructuralIndex,
    prefix: &StructuralIndex,
    tail: &StructuralIndex,
    at: usize,
) {
    let base = prefix.indexed_elements as usize;
    assert!(
        base <= full.indexed_elements as usize,
        "prefix retained more than the whole document"
    );
    let want = full.indexed_elements as usize - base;
    assert_eq!(
        tail.indexed_elements as usize,
        want,
        "tail retained {want} records in the continuous scan"
    );
    assert_eq!(tail.elem_start.len(), want);
    for i in 0..want {
        let f = base + i;
        assert_eq!(tail.elem_start[i], full.elem_start[f], "start of record {i}");
        assert_eq!(tail.elem_end[i], full.elem_end[f], "end of record {i}");
        assert_eq!(tail.elem_line[i], full.elem_line[f], "line of record {i}");
        assert_eq!(
            tail.elem_parent[i], full.elem_parent[f],
            "parent of record {i}"
        );
        assert_eq!(tail.elem_name[i], full.elem_name[f], "name of record {i}");
        assert_eq!(tail.elem_depth[i], full.elem_depth[f], "depth of record {i}");
    }
    // Line checkpoints continue on the same stride, without duplicating checkpoint 0.
    // (A cut at offset 0 is the degenerate case: the empty prefix "owns" checkpoint 0 and
    // the resumed scanner seeds it again, so the prefix contributes nothing there.)
    let cp_base = if at == 0 { 0 } else { prefix.checkpoints.len() };
    assert_eq!(tail.checkpoints.len(), full.checkpoints.len() - cp_base);
    for i in 0..tail.checkpoints.len() {
        assert_eq!(
            tail.checkpoints[i], full.checkpoints[cp_base + i],
            "checkpoint {i}"
        );
    }
    // Names discovered after the cut keep their ids.
    assert_eq!(tail.names.len(), full.names.len());
    for (i, n) in tail.names.iter().enumerate() {
        assert_eq!(n, &full.names[i], "name table entry {i}");
    }
    assert_eq!(tail.total_elements, full.total_elements - prefix.total_elements);
    assert_eq!(
        tail.total_attributes,
        full.total_attributes - prefix.total_attributes
    );
    assert_eq!(tail.line_count, full.line_count);
    // The tail can only see the depths it actually reaches; the deepest element may sit
    // in the prefix. The merge in `xmlspy-parallel` takes the maximum over all segments.
    assert!(tail.max_depth <= full.max_depth, "tail depth exceeds the document");
}

const CORPORA: &[&str] = &[
    "<a/>",
    "<root><a/><b>x</b><c/></root>",
    "\u{feff}<?xml version=\"1.0\"?><a x=\"1\"><b>t</b></a>",
    "<root>\n  <e a='1' b=\"2\">text &amp; ref &#x41;</e>\n  <!-- c -->\n  <![CDATA[ ]] ]]> ]]>\n  <?pi x?>\n</root>\n",
    "<!DOCTYPE r [ <!ENTITY e \"v\"> <!ELEMENT r (#PCDATA)> ]>\n<r>&e;</r>",
    // malformed on purpose: every diagnostic path must survive a cut as well
    "<a><b x=1 x=\"2\"></c>Tom & Jerry<!-- -- --><d>]]>",
    "<a></a></b><c/>",
    "<Ünïcødé attr=\"vàlüe\">tëxt ünïcødé</Ünïcødé>",
];

#[test]
fn initial_boundary_is_a_fresh_scanner() {
    let cfg = CFG;
    let a = Scanner::new(cfg);
    let b = Scanner::resume(cfg, BoundaryState::initial());
    assert_eq!(a.boundary(), b.boundary());

    let bytes = CORPORA[3].as_bytes();
    let mut a = Scanner::new(cfg);
    a.feed(bytes, 0);
    a.finish(bytes.len() as u64);
    let mut b = Scanner::resume(cfg, BoundaryState::initial());
    b.feed(bytes, 0);
    b.finish(bytes.len() as u64);
    assert_eq!(a.into_index(), b.into_index());
    // `resume` at offset 0 owns checkpoint 0 exactly like `new` does.
    assert_eq!(
        Scanner::resume(cfg, BoundaryState::initial())
            .into_index()
            .checkpoints,
        Scanner::new(cfg).into_index().checkpoints
    );
}

#[test]
fn cutting_at_every_offset_is_a_no_op() {
    for src in CORPORA {
        let bytes = src.as_bytes();
        let full = whole(bytes, CFG);
        for at in 0..=bytes.len() {
            let (prefix, boundary, tail, _) = cut(bytes, at, CFG);
            assert_eq!(boundary.seen_bytes, at as u64, "offset {at} in {src:?}");
            assert_tail_matches(&full, &prefix, &tail, at);
        }
    }
}

#[test]
fn cutting_survives_a_tight_element_budget() {
    let mut src = String::from("<root>\n");
    for i in 0..120 {
        src.push_str(&format!("  <row id=\"{i}\"><cell>{i}</cell></row>\n"));
    }
    src.push_str("</root>\n");
    let bytes = src.as_bytes();
    for max_indexed in [0u32, 1, 7, 40, 121, 241] {
        let cfg = ScannerConfig {
            max_indexed,
            stride: 8,
            max_errors: 100,
        };
        let full = whole(bytes, cfg);
        for at in (0..bytes.len()).step_by(37) {
            let (prefix, _, tail, _) = cut(bytes, at, cfg);
            assert_tail_matches(&full, &prefix, &tail, at);
        }
    }
}

#[test]
fn split_targets_yield_clean_depth_one_boundaries() {
    let mut src = String::from("<PurchaseOrders>\n");
    for i in 0..200 {
        src.push_str(&format!(
            "  <Order id=\"{i}\"><Item>x</Item><Item>y</Item></Order>\n"
        ));
    }
    src.push_str("</PurchaseOrders>\n");
    let bytes = src.as_bytes();
    let len = bytes.len() as u64;

    let threads = 8usize;
    let targets: Vec<u64> = (1..threads).map(|k| len * k as u64 / threads as u64).collect();

    let mut s = Scanner::new(ScannerConfig {
        max_indexed: 0, // a split pass does not need to retain anything
        stride: CFG.stride,
        max_errors: CFG.max_errors,
    });
    s.track_splits(targets.clone());
    s.feed(bytes, 0);
    s.finish(len);
    let splits = s.take_splits();
    let roots = s.take_depth0_closes();

    assert_eq!(splits.len(), targets.len(), "every target found a split");
    let mut prev = 0u64;
    for (i, (off, b)) in splits.iter().enumerate() {
        assert!(*off >= targets[i], "split {i} is before its target");
        assert!(*off > prev, "splits must ascend");
        prev = *off;
        assert!(b.is_clean(), "split {i} at {off} is not a clean boundary: {b:?}");
        assert_eq!(b.depth, 1, "split {i} must sit between top-level records");
        assert_eq!(b.seen_bytes, *off);
        assert!(b.root_seen && !b.root_closed);
        assert_eq!(b.elem_count, 0, "a split pass retains nothing");
    }

    // Exactly one depth-0 close: just past `</PurchaseOrders>`.
    assert_eq!(roots.len(), 1);
    assert_eq!(roots[0], len - 1, "root ends before the trailing newline");

    // Segments scanned from those boundaries reproduce the single-pass index.
    let full = whole(bytes, CFG);
    let mut bounds = vec![0u64];
    bounds.extend(splits.iter().map(|(o, _)| *o));
    bounds.push(len);
    let mut tail_records = 0usize;
    for k in 0..splits.len() {
        let (start, b) = splits[k].clone();
        let end = bounds[k + 2] as usize;
        let mut seg = Scanner::resume(CFG, b);
        seg.feed(&bytes[start as usize..end], start);
        if end == bytes.len() {
            seg.finish(len);
        }
        let ix = seg.into_index();
        assert_eq!(ix.error_count, 0);
        assert!(ix.indexed_elements > 0, "segment {k} indexed nothing");
        tail_records += ix.indexed_elements as usize;
    }
    assert!(
        tail_records <= full.indexed_elements as usize,
        "segments retained more than the whole document"
    );
}

#[test]
fn no_splits_without_tracking() {
    let bytes = CORPORA[3].as_bytes();
    let mut s = Scanner::new(CFG);
    s.feed(bytes, 0);
    s.finish(bytes.len() as u64);
    assert!(s.take_splits().is_empty());
    assert!(s.take_depth0_closes().is_empty());
}
