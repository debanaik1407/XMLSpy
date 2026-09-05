//! Merging per-segment indexes into one that is **bit-identical** to a single-pass scan.
//!
//! Each segment was scanned by its own [`Scanner`], resumed from a boundary the split pass
//! captured, so a segment's records already carry absolute offsets, absolute line numbers
//! and absolute depths. What the segments do *not* know is each other's existence, and
//! that is exactly three things:
//!
//! 1. **the element budget is global.** A scanner retains a record when
//!    `(elem_count < max_indexed || depth <= 1) && elem_count < 2 * max_indexed`, counting
//!    from the start of the *document*. A segment counts from zero, so it keeps more than
//!    its share. That is fine, and it is the whole trick: the segment's set is a **superset**
//!    of the sequential one, so the merge can re-apply the global rule and get the same
//!    answer. The proof is below, because "it's a superset" is the only thing standing
//!    between this crate and a silently truncated index.
//! 2. **index ids are global.** `elem_parent` holds an index slot, and slots are numbered
//!    over the whole document. A segment numbers from zero, so the merge renumbers and
//!    remaps parents through the same table.
//! 3. **the top-level element spans every segment.** A cut is only made at depth 1, so the
//!    only element that can straddle a cut is the depth-0 one. Its start lives in an early
//!    segment and its end offset — which that segment cannot know — comes from the split
//!    pass ([`SplitPlan::depth0_closes`]).
//!
//! Name ids need no renumbering in practice: the split pass interned every element name in
//! the document, in first-appearance order, which is the order a sequential scan interns
//! them, so [`MergeInput::base_names`] already *is* the final table. The merge still
//! re-interns by string (cheap: distinct element names are few), so a segment that meets a
//! name the pass did not — impossible today, but not by construction — lands in the table
//! instead of pointing outside it.
//!
//! # The superset proof
//!
//! Notation for one segment: `B` = slots the merge already handed out before this segment
//! (= the sequential `elem_count` at the segment start), `budget` = `max_indexed`,
//! `stop` = `2 * budget`. For a record `r`, `L_r` = slots the *segment* handed out before
//! `r`, `G_r = B + M_r` = slots the *merge* handed out before `r`, `D`/`S` = the number of
//! deep (`depth > 1`) / shallow (`depth <= 1`) records retained, suffixed `w` for the
//! segment and `m` for the merge.
//!
//! * The merge retains every shallow record the segment retained, up to the point where
//!   `G` reaches `stop`: a shallow record is retained by both whenever the counter is below
//!   `stop`, and `G_r >= stop` implies `M_r >= stop - B`, after which sequential retains
//!   nothing either — so nothing later can diverge. Hence `S_m >= S_w` before any
//!   divergence, and `L_r = D_w + S_w`, `M_r = D_m + S_m`.
//! * A deep record is retained by the segment only while `L < budget`, so `D_w <= budget`.
//! * The merge retains deep records only while `G < budget`, so `D_m = min(budget - B, #deep)`
//!   when `B < budget` and `D_m = 0` when `B >= budget`. Therefore `B + D_m >= min(budget, B + #deep)`.
//! * Suppose sequential retains `r` but the segment dropped it. Dropping means
//!   `L_r >= stop` (it cannot be the depth clause: sequential retained `r`, and if `r` is
//!   deep then `G_r < budget <= stop` while `L_r >= stop`); retaining means `G_r < stop`.
//!   With `S_m >= S_w`:
//!   `stop > G_r = B + D_m + S_m >= B + D_m + S_w = B + D_m + L_w - D_w >= B + D_m + stop - D_w`,
//!   which gives `D_w > B + D_m >= min(budget, B + #deep)`. Since `D_w <= budget` and
//!   `D_w <= #deep`, that is impossible in both branches (`B >= budget` gives
//!   `D_w > B >= budget`; `B < budget` with enough deep records gives `D_w > budget`;
//!   with fewer deep records gives `D_w > #deep`). Contradiction. ∎
//!
//! So the merge never has to guess: re-applying the global rule to the segments' records
//! reproduces the sequential index exactly, and `tests/parity.rs` asserts it over the whole
//! corpus set at every thread count and every budget from 0 upwards.

use xmlspy_index::{StructuralIndex, END_UNKNOWN, NO_PARENT};
use xmlspy_parse::ScannerConfig;

/// One scanned segment, plus what the merge needs to place it in the document.
#[derive(Debug, Clone)]
pub struct SegmentIndex {
    /// What the segment's scanner produced (records, checkpoints, diagnostics, counters).
    pub ix: StructuralIndex,
    /// Element depth the segment was seeded with: `0` for the first segment (a fresh
    /// scanner), `1` for every segment that starts at a cut.
    pub seed_depth: usize,
    /// True for the segment that reaches EOF. Only that one ran `Scanner::finish`, so only
    /// its diagnostics contain the end-of-file errors and only its `line_count` is final.
    pub is_last: bool,
}

/// Everything [`merge`] needs.
pub struct MergeInput {
    /// Scanner configuration the segments were scanned with (the budget is re-applied here).
    pub cfg: ScannerConfig,
    /// Document length in bytes.
    pub total: u64,
    /// Segment results, in document order.
    pub segments: Vec<SegmentIndex>,
    /// Offsets just past the `>` that closed each depth-0 element, ascending.
    pub depth0_closes: Vec<u64>,
    /// Name table from the split pass (empty when there was no split pass).
    pub base_names: Vec<String>,
}

/// Intern `s` in `names`, moving it in when it is new.
fn intern(names: &mut Vec<String>, s: String) -> u32 {
    if let Some(i) = names.iter().position(|n| *n == s) {
        return i as u32;
    }
    names.push(s);
    (names.len() - 1) as u32
}

/// Merge segment indexes into the index a single sequential scan would have produced.
pub fn merge(input: MergeInput) -> StructuralIndex {
    let MergeInput {
        cfg,
        total,
        segments,
        depth0_closes,
        base_names,
    } = input;
    let budget = cfg.max_indexed as usize;
    let stop = budget.saturating_mul(2);

    let mut out = StructuralIndex {
        file_len: total,
        // `Scanner::new` and `Scanner::resume` both clamp the stride, so the merged index
        // has to report the clamped value too or it would differ from a sequential scan.
        stride: cfg.stride.max(1),
        ..Default::default()
    };
    out.names = base_names;

    let mut kept: usize = 0; // the sequential `elem_count`
    let mut last_depth0: i32 = NO_PARENT; // slot of the open depth-0 element, if retained
    let mut open_depth0: Vec<i32> = Vec::new();
    let mut line_count: u64 = 1;

    for seg in segments {
        let mut six = seg.ix;
        // Checkpoints are absolute offsets and every newline belongs to exactly one
        // segment, so concatenation reproduces the sequential table.
        out.checkpoints.append(&mut six.checkpoints);
        out.total_elements += six.total_elements;
        out.total_attributes += six.total_attributes;
        if six.max_depth > out.max_depth {
            out.max_depth = six.max_depth;
        }
        out.error_count += six.error_count;
        line_count = six.line_count; // the last segment's value wins, and it is the final one
        // Diagnostics: document order across segments, capped exactly like the scanner.
        for e in six.errors.drain(..) {
            if out.errors.len() < cfg.max_errors as usize {
                out.errors.push(e);
            }
        }
        // Name ids: local table -> global table.
        let mut name_map: Vec<u32> = Vec::with_capacity(six.names.len());
        for n in six.names.drain(..) {
            name_map.push(intern(&mut out.names, n));
        }

        // Records. Move the arrays out so the segment's memory is released as we go.
        let n = six.elem_start.len();
        let starts = std::mem::take(&mut six.elem_start);
        let ends = std::mem::take(&mut six.elem_end);
        let lines = std::mem::take(&mut six.elem_line);
        let parents = std::mem::take(&mut six.elem_parent);
        let name_ids = std::mem::take(&mut six.elem_name);
        let depths = std::mem::take(&mut six.elem_depth);
        let mut id_map: Vec<i32> = vec![NO_PARENT; n];

        for i in 0..n {
            let depth = depths[i] as usize;
            let keep = (kept < budget || depth <= 1) && kept < stop;
            if !keep {
                if depth == 0 {
                    // The sequential scanner would have put -1 in its stack slot here, so
                    // children of this element get no parent either.
                    last_depth0 = NO_PARENT;
                }
                continue;
            }
            let gid = kept as i32;
            kept += 1;
            id_map[i] = gid;

            let mut parent = parents[i];
            if parent >= 0 {
                parent = match id_map.get(parent as usize) {
                    Some(p) => *p,
                    None => NO_PARENT,
                };
            } else if seg.seed_depth > 0 && depth == seg.seed_depth {
                // The segment's stack was seeded with a slot it does not own: the parent is
                // the depth-0 element that was already open when the segment started.
                parent = last_depth0;
            }

            out.elem_start.push(starts[i]);
            out.elem_end.push(ends[i]);
            out.elem_line.push(lines[i]);
            out.elem_parent.push(parent);
            out.elem_name.push(*name_map.get(name_ids[i] as usize).unwrap_or(&0));
            out.elem_depth.push(depths[i]);
            if depth == 0 {
                last_depth0 = gid;
                open_depth0.push(gid);
            }
        }
    }

    // The top-level element's end offset: it was closed in a later segment, which had no
    // slot to write it into. `depth0_closes` came from the same scanner pass that chose the
    // cuts, so the offsets are exactly the ones the sequential scan would have written.
    let mut ci = 0usize;
    for &gid in &open_depth0 {
        let g = gid as usize;
        if out.elem_end[g] != END_UNKNOWN {
            continue;
        }
        let start = out.elem_start[g];
        while ci < depth0_closes.len() && depth0_closes[ci] <= start {
            ci += 1;
        }
        if ci < depth0_closes.len() {
            out.elem_end[g] = depth0_closes[ci];
            ci += 1;
        }
    }
    // Anything still open at EOF is closed at `total`, exactly like `Scanner::finish`.
    for &gid in &open_depth0 {
        let g = gid as usize;
        if out.elem_end[g] == END_UNKNOWN {
            out.elem_end[g] = total;
        }
    }

    out.indexed_elements = kept as u32;
    out.line_count = line_count;
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use xmlspy_parse::{Scanner, WfError};

    fn cfg(max_indexed: u32) -> ScannerConfig {
        ScannerConfig {
            max_indexed,
            stride: 8,
            max_errors: 100,
        }
    }

    /// The reference: one sequential scan.
    fn seq(bytes: &[u8], c: ScannerConfig) -> StructuralIndex {
        Scanner::scan_all(c, bytes)
    }

    /// The same document as two segments cut at `at`, merged by hand. This is the merge
    /// logic in miniature — no threads, no split pass — so a regression here points at
    /// `merge` itself rather than at the scheduler.
    fn two_segments(src: &str, c: ScannerConfig, at: usize) -> StructuralIndex {
        let bytes = src.as_bytes();
        let mut a = Scanner::new(c);
        a.feed(&bytes[..at], 0);
        let b = a.boundary();
        let first = a.index_snapshot();

        let mut tail = Scanner::resume(c, b.clone());
        tail.feed(&bytes[at..], at as u64);
        tail.finish(bytes.len() as u64);
        let second = tail.into_index();

        // The top-level element is open at the cut (depth 1), so segment one left its end
        // as END_UNKNOWN and the merge needs the offset just past `</root>`'s `>`.
        let root_close = src.find("</root>").unwrap() as u64 + "</root>".len() as u64;
        let closes = if b.depth == 1 { vec![root_close] } else { Vec::new() };

        merge(MergeInput {
            cfg: c,
            total: bytes.len() as u64,
            segments: vec![
                SegmentIndex {
                    ix: first,
                    seed_depth: 0,
                    is_last: false,
                },
                SegmentIndex {
                    ix: second,
                    seed_depth: b.depth,
                    is_last: true,
                },
            ],
            depth0_closes: closes,
            base_names: Vec::new(),
        })
    }

    #[test]
    fn merging_one_segment_is_the_identity() {
        let src = "<root>\n  <a x=\"1\"><b/></a>\n  <c/>\n</root>\n";
        let bytes = src.as_bytes();
        for budget in [0u32, 1, 2, 5, 1000] {
            let c = cfg(budget);
            let want = seq(bytes, c);
            let mut only = Scanner::new(c);
            only.feed(bytes, 0);
            only.finish(bytes.len() as u64);
            let got = merge(MergeInput {
                cfg: c,
                total: bytes.len() as u64,
                segments: vec![SegmentIndex {
                    ix: only.into_index(),
                    seed_depth: 0,
                    is_last: true,
                }],
                depth0_closes: Vec::new(),
                base_names: Vec::new(),
            });
            assert_eq!(got, want, "budget {budget}");
        }
    }

    #[test]
    fn two_segments_reproduce_the_sequential_index() {
        let mut src = String::from("<root>\n");
        for i in 0..80 {
            src.push_str(&format!("  <row id=\"{i}\"><cell>{i}</cell></row>\n"));
        }
        src.push_str("</root>\n");
        let at = src.find("<row id=\"40\"").unwrap();
        for budget in [0u32, 1, 3, 17, 41, 81, 161, 100_000] {
            let c = cfg(budget);
            let want = seq(src.as_bytes(), c);
            let got = two_segments(&src, c, at);
            assert_eq!(got.indexed_elements, want.indexed_elements, "budget {budget}");
            assert_eq!(got.elem_start, want.elem_start, "budget {budget}");
            assert_eq!(got.elem_depth, want.elem_depth, "budget {budget}");
            assert_eq!(got.elem_parent, want.elem_parent, "budget {budget}");
            assert_eq!(got.elem_name, want.elem_name, "budget {budget}");
            assert_eq!(got.names, want.names, "budget {budget}");
            assert_eq!(got.total_elements, want.total_elements);
            assert_eq!(got.total_attributes, want.total_attributes);
            assert_eq!(got.max_depth, want.max_depth);
            assert_eq!(got.checkpoints, want.checkpoints, "budget {budget}");
            assert_eq!(got.errors, want.errors, "budget {budget}");
            assert_eq!(got.line_count, want.line_count);
            assert_eq!(got.file_len, want.file_len);
        }
    }

    #[test]
    fn an_empty_merge_is_an_empty_index() {
        let got = merge(MergeInput {
            cfg: cfg(100),
            total: 0,
            segments: Vec::new(),
            depth0_closes: Vec::new(),
            base_names: Vec::new(),
        });
        assert_eq!(got.indexed_elements, 0);
        assert_eq!(got.line_count, 1);
        assert_eq!(got.file_len, 0);
        assert!(got.errors.is_empty());
    }

    #[test]
    fn errors_are_concatenated_in_document_order_and_capped() {
        let c = ScannerConfig {
            max_indexed: 1000,
            stride: 8,
            max_errors: 3,
        };
        let mk = |msgs: &[&str], count: u64| {
            let mut ix = StructuralIndex::default();
            for (i, m) in msgs.iter().enumerate() {
                ix.errors.push(WfError::error(i as u64, 1, i as u64 + 1, *m));
            }
            ix.error_count = count;
            ix.line_count = 1;
            ix
        };
        let got = merge(MergeInput {
            cfg: c,
            total: 100,
            segments: vec![
                SegmentIndex {
                    ix: mk(&["a", "b"], 2),
                    seed_depth: 0,
                    is_last: false,
                },
                SegmentIndex {
                    ix: mk(&["c", "d"], 2),
                    seed_depth: 1,
                    is_last: true,
                },
            ],
            depth0_closes: Vec::new(),
            base_names: Vec::new(),
        });
        assert_eq!(got.error_count, 4, "every diagnostic is counted");
        assert_eq!(got.errors.len(), 3, "…but only max_errors are kept");
        let msgs: Vec<&str> = got.errors.iter().map(|e| e.msg.as_str()).collect();
        assert_eq!(msgs, vec!["a", "b", "c"]);
    }
}
