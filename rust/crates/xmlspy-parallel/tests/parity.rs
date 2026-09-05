//! The parallel build must produce **exactly** the sequential index.
//!
//! Not "the same records in a different order", not "the same tree": the same
//! [`StructuralIndex`], field for field, including diagnostics with their SmartFix strings,
//! line checkpoints, counters and the name table. If that ever stops holding, a cached
//! `.xsi` stops describing the document and the tree view depends on how many cores the
//! machine had — so this file is deliberately obsessive: every corpus in the workspace
//! (well-formed, malformed, truncated, deep, entity-riddled, CRLF, BOM, multi-root) crossed
//! with thread counts 1–8, element budgets 0–100 000 and line-checkpoint strides 1–32.
//!
//! It also pins the parts of the pipeline that are easy to get subtly wrong:
//!
//! * every cut the split pass emits is a legal boundary (state `Text`, depth 1, ascending);
//! * segments scanned one at a time on the calling thread merge to the same index, which
//!   separates the merge from the scheduler when something fails;
//! * a source that dribbles bytes out 7 at a time (short reads) streams to the same index;
//! * an interrupted build resumes from its journal and still produces the sequential index;
//! * a journal that no longer matches the file is ignored rather than trusted.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use xmlspy_core::{ByteSource, SourceError};
use xmlspy_index::{xsi, StructuralIndex};
use xmlspy_io::{journal_path_for, Fingerprint, IndexCache, Journal, JournalHeader};
use xmlspy_parallel::merge::{merge, MergeInput, SegmentIndex};
use xmlspy_parallel::split::{self, SplitPlan};
use xmlspy_parallel::{
    build_bytes, build_file, resume_file, scan_bytes, scan_segment, sequential, ParallelConfig,
};
use xmlspy_parse::{Scanner, ScannerConfig, St};

// ---------------------------------------------------------------- corpora

fn corpus(records: usize) -> String {
    let mut s = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!-- a generated purchase-order dump -->\n\
         <PurchaseOrders xmlns=\"urn:po\" xmlns:x=\"urn:x\">\n",
    );
    for i in 0..records {
        s.push_str(&format!(
            "  <Order id=\"{i}\" total=\"{i}.50\" currency=\"INR\">\n    \
             <Item sku=\"A-{i}\">Widget &amp; Co</Item>\n    \
             <Ship><City>Pune</City><Zip x:role=\"pin\">411001</Zip></Ship>\n    \
             <Notes><![CDATA[raw <text> & more]]></Notes>\n  \
             </Order>\n"
        ));
    }
    s.push_str("</PurchaseOrders>\n");
    s
}

fn deep(levels: usize) -> String {
    let mut s = String::from("<?xml version=\"1.0\"?>\n");
    for i in 0..levels {
        s.push_str(&format!("  <d{i} a=\"{i}\">\n"));
    }
    s.push_str("text &amp; more\n");
    for i in (0..levels).rev() {
        s.push_str(&format!("  </d{i}>\n"));
    }
    s
}

fn many_roots(n: usize) -> String {
    // More top-level elements than XML allows, and more than the split pass is willing to
    // track the ends of: the builder must notice and refuse to parallelise.
    let mut s = String::new();
    for i in 0..n {
        s.push_str(&format!("<r{i}><c/>{i}</r{i}>\n"));
    }
    s
}

fn drop_last(s: &str) -> Vec<u8> {
    s.as_bytes()[..s.len() - 1].to_vec()
}

fn cut_at(s: &str, frac: f64) -> Vec<u8> {
    let at = ((s.len() as f64) * frac) as usize;
    s.as_bytes()[..at.min(s.len())].to_vec()
}

fn replace_all(s: &str, from: &str, to: &str) -> Vec<u8> {
    s.replace(from, to).into_bytes()
}

fn corpora() -> Vec<(&'static str, Vec<u8>)> {
    let c60 = corpus(60);
    let c200 = corpus(200);
    let c600 = corpus(600);
    let c2000 = corpus(2000);
    vec![
        ("empty", Vec::new()),
        ("whitespace-only", b"   \n\t\n ".to_vec()),
        ("just-a-declaration", b"<?xml version=\"1.0\"?>\n".to_vec()),
        ("prolog-and-comment", b"<?xml version=\"1.0\"?>\n<!-- nothing else -->\n".to_vec()),
        ("empty-root", b"<a/>".to_vec()),
        ("empty-root-with-newline", b"<a/>\n".to_vec()),
        ("text-root", b"<a>text</a>".to_vec()),
        (
            "bom-prefixed",
            [b"\xef\xbb\xbf".to_vec(), c60.clone().into_bytes()].concat(),
        ),
        ("records-1", corpus(1).into_bytes()),
        ("records-4", corpus(4).into_bytes()),
        ("records-60", c60.into_bytes()),
        ("records-200", c200.clone().into_bytes()),
        ("records-600", c600.into_bytes()),
        ("records-2000", c2000.into_bytes()),
        ("deep-120", deep(120).into_bytes()),
        ("deep-2-unbalanced", b"<a><b><c>x</b></a>".to_vec()),
        ("mismatched-tags", b"<a><b></a></b></a>".to_vec()),
        ("stray-end-tag", b"<a>x</a></b>".to_vec()),
        ("multi-root", b"<a>1</a><b>2</b><c>3</c>".to_vec()),
        ("multi-root-nested", b"<a><x/></a><b><y/></b><c><z/></c>".to_vec()),
        ("many-roots-4200", many_roots(4200).into_bytes()),
        ("truncated-55pct", cut_at(&c200, 0.55)),
        ("truncated-99pct", cut_at(&c200, 0.99)),
        ("minus-one-byte", drop_last(&c200)),
        ("truncated-mid-cdata", [cut_at(&c200, 0.3), b"]]".to_vec()].concat()),
        (
            "unterminated-comment",
            [c200.into_bytes(), b"<!-- never closed".to_vec()].concat(),
        ),
        ("unterminated-attribute", b"<a x=\"1><b/></a>".to_vec()),
        ("unterminated-cdata", b"<a><![CDATA[abc</a>".to_vec()),
        ("bad-references", b"<a>&foo; &#xZZ; &#; & </a>".to_vec()),
        ("bare-ampersand-lt", b"<a>a & b < c</a>".to_vec()),
        ("cdata-end-in-text", b"<a>]]>x</a>".to_vec()),
        ("comment-with-dashes", b"<a><!-- x -- y --></a>".to_vec()),
        (
            "doctype-internal-subset",
            b"<!DOCTYPE r [ <!ENTITY e \"v\"> <!ELEMENT r (#PCDATA)> ]>\n<r>&e;</r>\n".to_vec(),
        ),
        ("pis-and-comments", b"<?xml version=\"1.0\"?>\n<?pi data?>\n<!--c-->\n<r><?inner?>t</r>\n<?tail?>\n".to_vec()),
        ("duplicate-attributes", b"<r a=\"1\" a=\"2\" b='3'><c a=\"x\" a=\"y\"/></r>".to_vec()),
        ("bad-name-chars", b"<a><b c$d=\"1\"/><e f/></a>".to_vec()),
        ("unquoted-attribute", b"<a x=1 y=\"2\"/></a>".to_vec()),
        ("lt-in-attribute", b"<a x=\"<\"/>".to_vec()),
        ("no-root-text", b"just text, no markup at all\n".to_vec()),
        ("crlf", replace_all(&corpus(80), "\n", "\r\n")),
        ("one-long-line", replace_all(&corpus(80), "\n", "")),
        (
            "utf8-text",
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<r><n>नाम</n><e>Ünïcødé — 日本語 🎉</e></r>\n"
                .as_bytes()
                .to_vec(),
        ),
        ("mixed-quotes", b"<r a='single' b=\"double\"><c d=\"it's\" e='say \"hi\"'/></r>".to_vec()),
        ("nested-same-name", b"<a><a><a><a/></a></a></a>".to_vec()),
        ("self-closing-run", b"<r/>".repeat(400)),
    ]
}

// ---------------------------------------------------------------- helpers

fn assert_same(got: &StructuralIndex, want: &StructuralIndex, ctx: &str) {
    assert_eq!(got.file_len, want.file_len, "{ctx}: file_len");
    assert_eq!(got.stride, want.stride, "{ctx}: stride");
    assert_eq!(got.line_count, want.line_count, "{ctx}: line_count");
    assert_eq!(got.checkpoints, want.checkpoints, "{ctx}: checkpoints");
    assert_eq!(got.names, want.names, "{ctx}: names");
    assert_eq!(got.indexed_elements, want.indexed_elements, "{ctx}: indexed_elements");
    assert_eq!(got.total_elements, want.total_elements, "{ctx}: total_elements");
    assert_eq!(got.total_attributes, want.total_attributes, "{ctx}: total_attributes");
    assert_eq!(got.max_depth, want.max_depth, "{ctx}: max_depth");
    assert_eq!(got.elem_start, want.elem_start, "{ctx}: elem_start");
    assert_eq!(got.elem_end, want.elem_end, "{ctx}: elem_end");
    assert_eq!(got.elem_line, want.elem_line, "{ctx}: elem_line");
    assert_eq!(got.elem_parent, want.elem_parent, "{ctx}: elem_parent");
    assert_eq!(got.elem_name, want.elem_name, "{ctx}: elem_name");
    assert_eq!(got.elem_depth, want.elem_depth, "{ctx}: elem_depth");
    assert_eq!(got.error_count, want.error_count, "{ctx}: error_count");
    assert_eq!(got.errors, want.errors, "{ctx}: errors");
    assert_eq!(got, want, "{ctx}: whole index");
}

fn cfg(budget: u32, stride: u32) -> ScannerConfig {
    ScannerConfig {
        max_indexed: budget,
        stride,
        max_errors: 64,
    }
}

/// A `ParallelConfig` that forces the split pass even on a small document.
fn forced(threads: usize, c: ScannerConfig) -> ParallelConfig {
    ParallelConfig::new()
        .with_threads(threads)
        .with_min_segment(1)
        .with_scan(c)
}

static TMP: AtomicUsize = AtomicUsize::new(0);

/// A scratch directory that deletes itself.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> TempDir {
        let n = TMP.fetch_add(1, Ordering::SeqCst);
        let mut p = std::env::temp_dir();
        p.push(format!("xmlspy-parity-{}-{n}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).expect("create temp dir");
        TempDir(p)
    }
    fn path(&self) -> &Path {
        &self.0
    }
    fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// A [`ByteSource`] that hands out at most `max` bytes per call and cannot be mapped, so it
/// exercises the streaming fallback and short reads at the same time.
struct Dribble {
    data: Vec<u8>,
    max: usize,
}

impl ByteSource for Dribble {
    fn len(&self) -> u64 {
        self.data.len() as u64
    }
    fn chunk(&mut self, offset: u64, len: usize) -> Result<&[u8], SourceError> {
        let o = offset as usize;
        if o > self.data.len() {
            return Err(SourceError::OutOfBounds {
                offset,
                len: self.data.len() as u64,
            });
        }
        let e = (o + len.min(self.max)).min(self.data.len());
        Ok(&self.data[o..e])
    }
}

/// Segment bounds for a plan, the way the builder computes them.
fn bounds_of(plan: &SplitPlan, total: u64) -> Vec<(u64, u64)> {
    let mut out = Vec::with_capacity(plan.segments());
    let mut prev = 0u64;
    for (off, _) in &plan.splits {
        out.push((prev, *off));
        prev = *off;
    }
    out.push((prev, total));
    out
}

// ---------------------------------------------------------------- the matrix

#[test]
fn parallel_equals_sequential_on_every_corpus() {
    for (name, bytes) in corpora() {
        let big = bytes.len() > 40_000;
        let strides: &[u32] = if big { &[8] } else { &[1, 8, 32] };
        let budgets: &[u32] = if big {
            &[1, 64, 100_000]
        } else {
            &[0, 1, 7, 64, 1000, 100_000]
        };
        for &stride in strides {
            for &budget in budgets {
                for threads in [1usize, 2, 3, 4, 8] {
                    let c = cfg(budget, stride);
                    let want = scan_bytes(&bytes, c);
                    let (got, rep) = build_bytes(&bytes, &forced(threads, c));
                    let ctx = format!("{name} threads={threads} budget={budget} stride={stride}");
                    assert_same(&got, &want, &ctx);
                    // The report must describe what actually happened.
                    assert_eq!(rep.bytes, bytes.len() as u64, "{ctx}: report bytes");
                    if rep.parallel {
                        // A document may offer fewer legal cuts than there are threads
                        // (targets past the last top-level close simply go unsatisfied).
                        assert!(rep.segments >= 2, "{ctx}: parallel with one segment");
                        assert!(rep.segments <= threads, "{ctx}: more segments than threads");
                        assert_eq!(rep.threads, rep.segments, "{ctx}: report threads");
                    } else {
                        assert_eq!(rep.segments, 1, "{ctx}: sequential report");
                        assert_eq!(rep.threads, 1, "{ctx}: sequential report");
                        assert!(!rep.note.is_empty(), "{ctx}: a fallback needs a reason");
                    }
                }
            }
        }
    }
}

#[test]
fn the_parallel_path_is_actually_taken() {
    let bytes = corpus(600).into_bytes();
    for threads in [2usize, 3, 4, 8] {
        let c = cfg(1000, 8);
        let (ix, rep) = build_bytes(&bytes, &forced(threads, c));
        assert!(rep.parallel, "threads={threads}: {}", rep.note);
        assert_eq!(rep.segments, threads);
        assert_eq!(rep.threads, threads);
        assert!(rep.split_secs > 0.0, "the split pass must have run");
        assert!(rep.merge_secs >= 0.0);
        assert!(!ix.elem_start.is_empty());
        assert_same(&ix, &scan_bytes(&bytes, c), &format!("threads={threads}"));
    }
}

#[test]
fn every_cut_is_a_legal_boundary() {
    let bytes = corpus(500).into_bytes();
    let plan = split::plan(&bytes, cfg(1000, 8), 8, 1);
    assert_eq!(plan.segments(), 8, "{}", plan.note);
    let mut prev = 0u64;
    for (off, b) in &plan.splits {
        assert!(*off > prev, "cuts must ascend and be distinct");
        assert!(*off <= bytes.len() as u64, "cut inside the document");
        prev = *off;
        assert_eq!(b.depth, 1, "only between children of the top-level element");
        assert_eq!(b.state, St::Text, "only between tokens");
        assert!(b.is_clean(), "no half-finished token may be carried across");
        assert_eq!(b.seen_bytes, *off, "the segment resumes exactly at the cut");
        assert_eq!(b.stack_idx.len(), 1, "one element open");
        assert_eq!(b.stack_idx[0], -1, "…and the split pass retained no records");
        assert!(!b.finished);
    }
    // The split pass interns every element name in first-appearance order, which is the
    // order the sequential scanner interns them: that is why the merge needs no renumbering.
    let seq = scan_bytes(&bytes, cfg(1000, 8));
    assert_eq!(plan.names, seq.names, "name table of the split pass");
    assert!(!plan.depth0_closes.is_empty(), "the root must close somewhere");
    assert!(plan.depth0_closes.windows(2).all(|w| w[0] < w[1]), "ascending");
    assert_eq!(plan.bytes, bytes.len() as u64);
}

#[test]
fn segments_scanned_on_one_thread_merge_to_the_same_index() {
    // Same pipeline, no threads: if this passes and the matrix fails, the bug is in the
    // scheduler; if this fails, it is in the split pass or the merge.
    let bytes = corpus(500).into_bytes();
    let total = bytes.len() as u64;
    for threads in [2usize, 4, 8] {
        let c = cfg(97, 8);
        let mut plan = split::plan(&bytes, c, threads, 1);
        assert_eq!(plan.segments(), threads);
        let bounds = bounds_of(&plan, total);
        let segments: Vec<SegmentIndex> = bounds
            .iter()
            .enumerate()
            .map(|(i, &(a, b))| {
                let seed = if i == 0 {
                    None
                } else {
                    Some(plan.splits[i - 1].1.clone())
                };
                SegmentIndex {
                    ix: scan_segment(c, &bytes, a, b, seed, i + 1 == bounds.len(), total),
                    seed_depth: if i == 0 { 0 } else { 1 },
                    is_last: i + 1 == bounds.len(),
                }
            })
            .collect();
        let got = merge(MergeInput {
            cfg: c,
            total,
            segments,
            depth0_closes: std::mem::take(&mut plan.depth0_closes),
            base_names: std::mem::take(&mut plan.names),
        });
        assert_same(&got, &scan_bytes(&bytes, c), &format!("single-threaded, {threads} segments"));
    }
}

#[test]
fn a_document_with_too_many_roots_falls_back() {
    // More top-level elements than the split pass will track the ends of. Parallelising
    // would leave some `elem_end` unfilled, so the builder must decline — and still be right.
    let bytes = many_roots(4200).into_bytes();
    let c = cfg(100_000, 8);
    let (ix, rep) = build_bytes(&bytes, &forced(4, c));
    assert!(!rep.parallel, "{}", rep.note);
    assert!(rep.note.contains("top-level"), "{}", rep.note);
    assert_same(&ix, &scan_bytes(&bytes, c), "many roots");
    // Three roots is fine: the ends are trackable, so it may parallelise.
    let small = b"<a><x/></a><b><y/></b><c><z/></c>".to_vec();
    let (ix, _) = build_bytes(&small, &forced(2, c));
    assert_same(&ix, &scan_bytes(&small, c), "three roots");
}

#[test]
fn one_thread_is_always_sequential() {
    let bytes = corpus(300).into_bytes();
    let c = cfg(1000, 8);
    let (ix, rep) = build_bytes(&bytes, &forced(1, c));
    assert!(!rep.parallel);
    assert_eq!(rep.segments, 1);
    assert_eq!(rep.threads, 1);
    assert_same(&ix, &scan_bytes(&bytes, c), "one thread");

    // An explicit request is honoured even on a small machine…
    let cpus = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    let (ix, rep) = build_bytes(&bytes, &forced(8, c));
    assert_eq!(rep.segments, 8, "cpus={cpus}");
    assert_same(&ix, &scan_bytes(&bytes, c), "eight threads");

    // …and the automatic count never exceeds the machine or the document size.
    let auto = ParallelConfig::new().with_scan(c);
    assert!(auto.thread_budget(bytes.len() as u64) <= cpus.max(1));
    assert_eq!(auto.thread_budget(0), 1);
}

// ---------------------------------------------------------------- streaming

#[test]
fn short_reads_and_unmappable_sources_still_match() {
    let bytes = corpus(120).into_bytes();
    for max in [1usize, 3, 7, 64, 4096, 1 << 20] {
        let mut d = Dribble {
            data: bytes.clone(),
            max,
        };
        let got = sequential(&mut d, cfg(1000, 8)).expect("streaming scan");
        assert_same(&got, &scan_bytes(&bytes, cfg(1000, 8)), &format!("dribble {max}"));
    }
    // An unmappable source cannot be shared between threads, so `build` streams it and says so.
    let mut d = Dribble {
        data: bytes.clone(),
        max: 512,
    };
    let (got, rep) = xmlspy_parallel::build(&mut d, &forced(8, cfg(1000, 8))).expect("build");
    assert!(!rep.parallel);
    assert!(rep.note.contains("random-access"), "{}", rep.note);
    assert_same(&got, &scan_bytes(&bytes, cfg(1000, 8)), "unmappable source");

    // An in-memory slice source *is* random access, so it takes the parallel path.
    let mut s = xmlspy_core::SliceSource::new(&bytes);
    let (got, rep) = xmlspy_parallel::build(&mut s, &forced(4, cfg(1000, 8))).expect("build");
    assert!(rep.parallel, "{}", rep.note);
    assert_same(&got, &scan_bytes(&bytes, cfg(1000, 8)), "slice source");
}

#[test]
fn a_scanner_resumed_at_every_offset_agrees_with_one_pass() {
    // The property the split pass relies on, checked at byte level on a small document:
    // resuming anywhere the scanner calls a boundary reproduces the sequential index.
    let src = "<r a=\"1\">\n  <c>text &amp; more</c>\n  <c/><d><!--x--><![CDATA[y]]></d>\n</r>\n";
    let bytes = src.as_bytes();
    let c = cfg(1000, 4);
    let want = scan_bytes(bytes, c);
    let plan = split::plan(bytes, c, 3, 1);
    assert!(!plan.is_single(), "{}", plan.note);
    let total = bytes.len() as u64;
    let bounds = bounds_of(&plan, total);
    let mut segments = Vec::new();
    for (i, &(a, b)) in bounds.iter().enumerate() {
        let seed = if i == 0 {
            None
        } else {
            Some(plan.splits[i - 1].1.clone())
        };
        segments.push(SegmentIndex {
            ix: scan_segment(c, bytes, a, b, seed, i + 1 == bounds.len(), total),
            seed_depth: if i == 0 { 0 } else { 1 },
            is_last: i + 1 == bounds.len(),
        });
    }
    let got = merge(MergeInput {
        cfg: c,
        total,
        segments,
        depth0_closes: plan.depth0_closes.clone(),
        base_names: plan.names.clone(),
    });
    assert_same(&got, &want, "resumed at the plan's boundaries");
}

// ---------------------------------------------------------------- files, cache, journal

#[test]
fn build_file_maps_the_document_and_caches_the_index() {
    let dir = TempDir::new("file");
    let path = dir.join("orders.xml");
    let bytes = corpus(400).into_bytes();
    fs::write(&path, &bytes).expect("write");

    let cache = IndexCache::new(dir.join("cache"), 8 << 20).expect("cache dir");
    let c = cfg(5000, 8);
    let pcfg = forced(4, c);

    let (first, rep1) = build_file(&path, &pcfg, Some(&cache)).expect("build_file");
    assert_same(&first, &scan_bytes(&bytes, c), "from file");
    assert!(
        rep1.note.contains("mmap") || rep1.note.contains("buffered"),
        "the report should name the backend: {}",
        rep1.note
    );
    assert!(rep1.parallel || !rep1.note.is_empty());

    // Second open: the cache answers, and the answer is the same index.
    let (second, rep2) = build_file(&path, &pcfg, Some(&cache)).expect("build_file cached");
    assert!(rep2.note.contains("cache hit"), "{}", rep2.note);
    assert_eq!(first, second, "cache round-trip through .xsi must be lossless");

    // A different document must not hit the same entry.
    let other = dir.join("other.xml");
    fs::write(&other, corpus(5).as_bytes()).expect("write other");
    let (third, rep3) = build_file(&other, &pcfg, Some(&cache)).expect("build_file other");
    assert!(!rep3.note.contains("cache hit"), "{}", rep3.note);
    assert_same(&third, &scan_bytes(corpus(5).as_bytes(), c), "other document");
    assert!(cache.stats().entries >= 2, "{:?}", cache.stats());

    // No cache: still the same index.
    let (fourth, _) = build_file(&path, &pcfg, None).expect("build_file uncached");
    assert_same(&fourth, &scan_bytes(&bytes, c), "uncached");
}

#[test]
fn a_journaled_build_cleans_up_after_itself() {
    let dir = TempDir::new("journal-ok");
    let path = dir.join("doc.xml");
    let bytes = corpus(300).into_bytes();
    fs::write(&path, &bytes).expect("write");

    let c = cfg(2000, 8);
    let pcfg = forced(4, c).with_journal(None);
    let (ix, rep) = build_file(&path, &pcfg, None).expect("journaled build");
    assert_same(&ix, &scan_bytes(&bytes, c), "journaled build");
    assert!(rep.parallel, "{}", rep.note);
    assert_eq!(rep.journaled, rep.segments as u32, "every segment was logged");
    assert!(!journal_path_for(&path).exists(), "a committed build removes its log");
    assert!(resume_file(&path, &pcfg).expect("resume").is_none(), "nothing left to resume");
}

#[test]
fn an_interrupted_build_resumes_from_its_journal() {
    let dir = TempDir::new("resume");
    let path = dir.join("doc.xml");
    let bytes = corpus(600).into_bytes();
    fs::write(&path, &bytes).expect("write");

    let c = cfg(500, 8);
    let pcfg = forced(4, c);
    let plan = split::plan(&bytes, c, 4, 1);
    assert_eq!(plan.segments(), 4, "{}", plan.note);
    let bounds = bounds_of(&plan, bytes.len() as u64);

    // Write a journal that stops after two segments: exactly what a killed process leaves.
    let jpath = journal_path_for(&path);
    let header = JournalHeader {
        stride: c.stride,
        max_indexed: c.max_indexed,
        max_errors: c.max_errors,
        threads: 4,
        source_len: bytes.len() as u64,
        source: Fingerprint::of(&path).expect("fingerprint"),
        splits: plan.splits.iter().map(|(o, _)| *o).collect(),
    };
    let mut j = Journal::create(&jpath, &header, false).expect("journal");
    for i in 0..2usize {
        let (a, b) = bounds[i];
        let seed = if i == 0 {
            None
        } else {
            Some(plan.splits[i - 1].1.clone())
        };
        let ix = scan_segment(c, &bytes, a, b, seed, false, bytes.len() as u64);
        j.append_segment(i as u32, &xsi::encode(&ix, true))
            .expect("append segment");
    }
    drop(j); // no commit: the build "died" here
    assert!(jpath.exists());

    let (ix, rep) = resume_file(&path, &pcfg)
        .expect("resume_file")
        .expect("a journal with two segments must be resumable");
    assert_eq!(rep.reused, 2, "{}", rep.note);
    assert_eq!(rep.rescanned, 2, "{}", rep.note);
    assert_eq!(rep.segments, 4);
    assert_same(&ix, &scan_bytes(&bytes, c), "resumed build");
    assert!(!jpath.exists(), "a finished resume removes the log");

    // With every segment in the journal, nothing is scanned at all.
    let mut j = Journal::create(&jpath, &header, false).expect("journal");
    for i in 0..4usize {
        let (a, b) = bounds[i];
        let seed = if i == 0 {
            None
        } else {
            Some(plan.splits[i - 1].1.clone())
        };
        let ix = scan_segment(c, &bytes, a, b, seed, i == 3, bytes.len() as u64);
        j.append_segment(i as u32, &xsi::encode(&ix, i != 3))
            .expect("append segment");
    }
    drop(j);
    let (ix, rep) = resume_file(&path, &pcfg).expect("resume_file").expect("resumable");
    assert_eq!(rep.reused, 4, "{}", rep.note);
    assert_eq!(rep.rescanned, 0, "{}", rep.note);
    assert_same(&ix, &scan_bytes(&bytes, c), "fully journaled build");
}

#[test]
fn a_journal_that_does_not_match_is_ignored() {
    let dir = TempDir::new("stale");
    let path = dir.join("doc.xml");
    let bytes = corpus(300).into_bytes();
    fs::write(&path, &bytes).expect("write");

    // No journal at all.
    let pcfg = forced(4, cfg(500, 8));
    assert!(resume_file(&path, &pcfg).expect("resume_file").is_none());

    let plan = split::plan(&bytes, pcfg.scan, 4, 1);
    let jpath = journal_path_for(&path);
    let header = JournalHeader {
        stride: pcfg.scan.stride,
        max_indexed: pcfg.scan.max_indexed,
        max_errors: pcfg.scan.max_errors,
        threads: 4,
        source_len: bytes.len() as u64,
        source: Fingerprint::of(&path).expect("fingerprint"),
        splits: plan.splits.iter().map(|(o, _)| *o).collect(),
    };
    let bounds = bounds_of(&plan, bytes.len() as u64);
    let (a, b) = bounds[0];
    let seg0 = scan_segment(pcfg.scan, &bytes, a, b, None, false, bytes.len() as u64);
    let encoded = xsi::encode(&seg0, true);

    // A journal whose fingerprint belongs to a different document.
    let mut j = Journal::create(&jpath, &header, false).expect("journal");
    j.append_segment(0, &encoded).expect("append");
    drop(j);
    fs::write(&path, corpus(301).as_bytes()).expect("rewrite");
    assert!(
        resume_file(&path, &pcfg).expect("resume_file").is_none(),
        "a changed document must not reuse a journal"
    );
    let _ = fs::remove_file(&jpath);

    // A journal whose recorded cuts no longer land where the document says they should.
    fs::write(&path, &bytes).expect("restore");
    let mut bad = header.clone();
    bad.splits = bad.splits.iter().map(|o| o + 3).collect();
    let mut j = Journal::create(&jpath, &bad, false).expect("journal");
    j.append_segment(0, &encoded).expect("append");
    drop(j);
    assert!(
        resume_file(&path, &pcfg).expect("resume_file").is_none(),
        "cuts that moved must not be trusted"
    );

    // A truncated journal file is not an error either: there is simply nothing to resume.
    fs::write(&jpath, b"XSJ1").expect("stub");
    assert!(resume_file(&path, &pcfg).expect("resume_file").is_none());
}

// ---------------------------------------------------------------- sanity of the reference itself

#[test]
fn scan_bytes_agrees_with_the_scanner_api() {
    let bytes = corpus(50).into_bytes();
    let c = cfg(1000, 8);
    let mut s = Scanner::new(c);
    s.feed(&bytes, 0);
    s.finish(bytes.len() as u64);
    assert_eq!(scan_bytes(&bytes, c), s.into_index());
    assert_eq!(scan_bytes(&bytes, c), Scanner::scan_all(c, &bytes));
    // A segment scan of the whole document is the same thing again.
    assert_eq!(
        scan_bytes(&bytes, c),
        scan_segment(c, &bytes, 0, bytes.len() as u64, None, true, bytes.len() as u64)
    );
}

#[test]
fn an_empty_document_is_empty_every_way() {
    let c = cfg(1000, 8);
    let want = scan_bytes(b"", c);
    assert_eq!(want.indexed_elements, 0);
    assert_eq!(want.line_count, 1);
    assert_eq!(want.checkpoints, vec![0]);
    assert!(!want.errors.is_empty(), "an empty document is not well-formed");
    for threads in [1usize, 2, 8] {
        let (got, rep) = build_bytes(b"", &forced(threads, c));
        assert!(!rep.parallel);
        assert_same(&got, &want, &format!("empty, {threads} threads"));
    }
}
