//! Parallel index building for the XMLSpy-rs engine.
//!
//! # The promise
//!
//! A parallel build produces **exactly** the [`StructuralIndex`] a single sequential scan
//! of the same bytes produces — same records, same ids, same parents, same diagnostics,
//! same checkpoints, same counters. Not "equivalent for practical purposes": byte for
//! byte. [`tests/parity.rs`](https://github.com/debanaik1407/XMLSpy) asserts it over every
//! corpus in the workspace at every thread count from 1 to 8 and every element budget from
//! 0 to 100 000, including malformed and truncated documents. Anything less would make the
//! index depend on the machine that built it, and a cached `.xsi` would then be a lie.
//!
//! # How
//!
//! 1. **Split** ([`split`]). One sequential pass of the real [`Scanner`] with
//!    `max_indexed == 0` — no records, no diagnostic strings, just the state machine —
//!    captures a [`BoundaryState`] at the first depth-1 element close at or after each
//!    target offset. A cut is legal only where the scanner is between tokens with exactly
//!    one element open, so no element except the top-level one can span a cut.
//! 2. **Scan** ([`scan_segment`]). One thread per segment, each resumed from its boundary,
//!    so it starts with the sequential scanner's exact state: depth, open-element stack,
//!    line number, line start, quote/CDATA/DOCTYPE sub-state, the works. Offsets and line
//!    numbers are absolute, so nothing has to be shifted afterwards. Every segment but the
//!    last yields [`Scanner::index_snapshot`] — the last one alone runs
//!    [`Scanner::finish`], which is what keeps end-of-file diagnostics from being reported
//!    eight times.
//! 3. **Merge** ([`merge`]). Renumber ids, remap parents, re-apply the *global* element
//!    budget, concatenate checkpoints and diagnostics, and patch the top-level element's
//!    end offset from the split pass. [`merge`] carries the proof that a segment's records
//!    are a superset of the sequential ones, which is what makes step 3 exact rather than
//!    hopeful.
//!
//! Results stream back over a channel, so the write-ahead journal is appended **as segments
//! finish** and a crash mid-build loses only the work in flight ([`resume_file`]).
//!
//! # What it costs, honestly
//!
//! The split pass is sequential. If it costs a fraction `c` of a full scan, the best
//! achievable speed-up on `N` threads is `1 / (c + 1/N)` — so this is *not* linear
//! scaling, and on a 2-vCPU machine it will not beat the sequential path by much. `c` is
//! measured and reported in [`BuildReport::split_secs`] rather than hand-waved; the
//! benchmark report in `rust/bench/` records it next to the throughput gates.
//!
//! # Requirements
//!
//! Threads share the document as a `&[u8]`, which is why the parallel path needs
//! [`ByteSource::as_slice`] — `mmap` on native, an in-memory buffer in tests and in WASM.
//! A streamed source (no `mmap` available) is scanned sequentially and the report says so:
//! copying gigabytes into memory to parallelise the copy would be a poor trade.
//!
//! # No dependencies
//!
//! `std::thread::scope` and `std::sync::mpsc` only. The workspace has no external crates,
//! so there is no rayon here; the scoped-thread version needs no global pool and no
//! `'static` bounds, which is what lets a worker borrow the document slice directly.

#![warn(missing_docs)]

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::Instant;

use xmlspy_core::{ByteSource, SourceError, CHUNK_SIZE};
use xmlspy_index::{xsi, StructuralIndex};
use xmlspy_io::{
    journal_path_for, open_byte_source, Fingerprint, IndexCache, Journal, JournalHeader,
};
use xmlspy_parse::{BoundaryState, Scanner, ScannerConfig};

pub mod merge;
pub mod split;

pub use merge::{MergeInput, SegmentIndex};
pub use split::SplitPlan;

/// Bytes a worker reads per pass over its slice. Smaller than [`CHUNK_SIZE`] because
/// `N` workers read at once and the allocation is per worker.
pub const WORKER_CHUNK: usize = 4 << 20;

/// Segments below this are not worth a thread (default for [`ParallelConfig`]).
pub const DEFAULT_MIN_SEGMENT_BYTES: u64 = 32 << 20;

/// Ceiling on threads, however many the machine claims to have.
pub const MAX_THREADS: usize = 64;

/// How to build an index.
#[derive(Debug, Clone)]
pub struct ParallelConfig {
    /// Scanner configuration given to every segment (and to the split pass, which lowers
    /// `max_indexed` to 0 itself).
    pub scan: ScannerConfig,
    /// Threads requested. `0` means "as many as this machine offers".
    pub threads: usize,
    /// Minimum segment size in bytes. `0` means [`DEFAULT_MIN_SEGMENT_BYTES`]. Set to `1`
    /// to force a parallel build of a small document (benchmarks do this).
    pub min_segment_bytes: u64,
    /// Journal segment results to disk as they arrive, so an interrupted build can be
    /// finished by [`resume_file`]. Only `build_file` has a path to journal next to.
    pub journal: bool,
    /// Journal location; `None` means [`journal_path_for`].
    pub journal_path: Option<PathBuf>,
}

impl Default for ParallelConfig {
    fn default() -> Self {
        Self {
            scan: ScannerConfig::default(),
            threads: 0,
            min_segment_bytes: 0,
            journal: false,
            journal_path: None,
        }
    }
}

impl ParallelConfig {
    /// Default configuration: automatic thread count, journaling off.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the scanner configuration.
    #[must_use]
    pub fn with_scan(mut self, scan: ScannerConfig) -> Self {
        self.scan = scan;
        self
    }

    /// Ask for `n` threads (`0` = automatic).
    #[must_use]
    pub fn with_threads(mut self, n: usize) -> Self {
        self.threads = n;
        self
    }

    /// Smallest segment worth a thread.
    #[must_use]
    pub fn with_min_segment(mut self, bytes: u64) -> Self {
        self.min_segment_bytes = bytes;
        self
    }

    /// Journal the build (`path = None` puts it next to the document).
    #[must_use]
    pub fn with_journal(mut self, path: Option<PathBuf>) -> Self {
        self.journal = true;
        self.journal_path = path;
        self
    }

    /// The effective minimum segment size.
    #[must_use]
    pub fn min_segment(&self) -> u64 {
        if self.min_segment_bytes == 0 {
            DEFAULT_MIN_SEGMENT_BYTES
        } else {
            self.min_segment_bytes
        }
    }

    /// Threads actually worth spending on a document of `len` bytes.
    ///
    /// An explicit request is honoured (benchmarks need to over-subscribe a small machine)
    /// but never beyond [`MAX_THREADS`]; the automatic count is
    /// `available_parallelism()`. Either way a document only gets as many threads as it has
    /// [`min_segment`](Self::min_segment) sized pieces.
    #[must_use]
    pub fn thread_budget(&self, len: u64) -> usize {
        let cpus = thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);
        let want = if self.threads == 0 { cpus } else { self.threads };
        let want = want.clamp(1, MAX_THREADS);
        let min_seg = self.min_segment().max(1);
        let by_size = (len / min_seg) as usize;
        want.min(by_size).max(1)
    }
}

/// What a build did, for the status line and the benchmark report.
#[derive(Debug, Clone, Default)]
pub struct BuildReport {
    /// Document length in bytes.
    pub bytes: u64,
    /// Threads that actually scanned a segment (1 for a sequential build).
    pub threads: usize,
    /// Segments the document was cut into.
    pub segments: usize,
    /// True when more than one thread scanned.
    pub parallel: bool,
    /// Split pass, seconds (0 when there was none).
    pub split_secs: f64,
    /// Segment scanning, seconds (wall clock, so it shrinks with threads).
    pub scan_secs: f64,
    /// Merge, seconds.
    pub merge_secs: f64,
    /// Whole build, seconds.
    pub total_secs: f64,
    /// `bytes / total_secs`, in MB/s (1 MB = 10^6 bytes, matching `xmlspy bench`).
    pub mb_per_s: f64,
    /// Segment results written to the journal.
    pub journaled: u32,
    /// Segments reused from a journal by [`resume_file`].
    pub reused: usize,
    /// Segments still scanned by [`resume_file`].
    pub rescanned: usize,
    /// Why the build took the shape it did (backend, fallback reason, cache hit).
    pub note: String,
}

impl BuildReport {
    /// One-line summary for the CLI.
    #[must_use]
    pub fn summary(&self) -> String {
        let mode = if self.parallel {
            format!("{} threads / {} segments", self.threads, self.segments)
        } else {
            "sequential".to_string()
        };
        let extra = if self.note.is_empty() {
            String::new()
        } else {
            format!(" ({})", self.note)
        };
        format!(
            "{:.1} MB in {:.3}s = {:.0} MB/s [{}]{extra}",
            self.bytes as f64 / 1e6,
            self.total_secs,
            self.mb_per_s,
            mode
        )
    }
}

/// Scan `bytes[start..end]` as one segment.
///
/// `seed` is the [`BoundaryState`] at `start` (`None` for the first segment, which starts
/// a fresh scanner). `finish` must be true only for the segment that reaches the end of the
/// document: it is the one that reports end-of-file diagnostics, and `total` is the
/// document length it reports them against.
///
/// Every segment gets the same [`ScannerConfig`], including the element budget. A segment
/// therefore counts retained records from zero and keeps *more* than its share — which is
/// exactly what [`merge`] expects; see the superset proof there.
pub fn scan_segment(
    cfg: ScannerConfig,
    bytes: &[u8],
    start: u64,
    end: u64,
    seed: Option<BoundaryState>,
    finish: bool,
    total: u64,
) -> StructuralIndex {
    let mut s = match seed {
        Some(b) => Scanner::resume(cfg, b),
        None => Scanner::new(cfg),
    };
    let mut off = start.min(end).min(bytes.len() as u64) as usize;
    let end = end.min(bytes.len() as u64) as usize;
    while off < end {
        let stop = (off + WORKER_CHUNK).min(end);
        let d = &bytes[off..stop];
        if d.is_empty() {
            break;
        }
        s.feed(d, off as u64);
        off = stop;
    }
    if finish {
        s.finish(total);
        s.into_index()
    } else {
        s.index_snapshot()
    }
}

/// Scan a whole in-memory document with one thread. The reference implementation the
/// parallel path is checked against.
#[must_use]
pub fn scan_bytes(bytes: &[u8], cfg: ScannerConfig) -> StructuralIndex {
    scan_segment(cfg, bytes, 0, bytes.len() as u64, None, true, bytes.len() as u64)
}

/// Stream a [`ByteSource`] through one scanner.
///
/// This is what a document that cannot be mapped falls back to, and what the browser's
/// worker does with a `Blob`: `chunk()` may return fewer bytes than asked for, and the
/// scanner is resumable at every byte, so the loop needs no alignment or lookahead.
///
/// # Errors
/// When the source fails a read.
pub fn sequential(
    src: &mut dyn ByteSource,
    cfg: ScannerConfig,
) -> Result<StructuralIndex, SourceError> {
    let total = src.len();
    let mut s = Scanner::new(cfg);
    let mut off = 0u64;
    while off < total {
        let want = (total - off).min(CHUNK_SIZE as u64) as usize;
        let d = src.chunk(off, want)?;
        let n = d.len() as u64;
        if n == 0 {
            break;
        }
        s.feed(d, off);
        off += n;
    }
    s.finish(total);
    Ok(s.into_index())
}

/// Build the index of an in-memory document, in parallel when that pays.
#[must_use]
pub fn build_bytes(bytes: &[u8], cfg: &ParallelConfig) -> (StructuralIndex, BuildReport) {
    run(bytes, cfg, None)
}

/// Build the index of a [`ByteSource`].
///
/// Uses the parallel path when the source is random-access ([`ByteSource::as_slice`]) and
/// the document is big enough to split; otherwise streams it sequentially and says so in
/// [`BuildReport::note`].
#[must_use]
pub fn build(
    src: &mut dyn ByteSource,
    cfg: &ParallelConfig,
) -> Result<(StructuralIndex, BuildReport), SourceError> {
    let total = src.len();
    if let Some(bytes) = src.as_slice() {
        return Ok(run(bytes, cfg, None));
    }
    let t0 = Instant::now();
    let ix = sequential(src, cfg.scan)?;
    let mut rep = BuildReport {
        bytes: total,
        threads: 1,
        segments: 1,
        note: "source is not random-access (no mmap): streamed one thread".to_string(),
        ..Default::default()
    };
    close_out(&mut rep, t0);
    Ok((ix, rep))
}

/// Build the index of a file, with mmap, the on-disk index cache and an optional journal.
///
/// `cache` is consulted first and updated on success; pass `None` to always scan. With
/// [`ParallelConfig::journal`], segment results are journaled as they arrive so
/// [`resume_file`] can finish an interrupted build.
///
/// # Errors
/// When the file cannot be opened or fingerprinted.
pub fn build_file(
    path: &Path,
    cfg: &ParallelConfig,
    cache: Option<&IndexCache>,
) -> std::io::Result<(StructuralIndex, BuildReport)> {
    let t0 = Instant::now();
    let mut src = open_byte_source(path)?;
    let fp = Fingerprint::of(path)?;
    let backend = src.kind();

    if let Some(c) = cache {
        if let Some(buf) = c.get(&fp) {
            if let Ok(ix) = xsi::decode(&buf) {
                let mut rep = BuildReport {
                    bytes: ix.file_len,
                    // `build_file` prefixes the backend, so this stays short.
                    note: "index cache hit".to_string(),
                    ..Default::default()
                };
                rep.segments = 1;
                rep.threads = 0;
                close_out(&mut rep, t0);
                return Ok((ix, rep));
            }
            // A cache entry that does not decode is worse than no entry.
            let _ = c.remove(&fp.key());
        }
    }

    if let Some(bytes) = src.as_slice() {
        let spec = if cfg.journal {
            Some(JournalSpec {
                path: cfg
                    .journal_path
                    .clone()
                    .unwrap_or_else(|| journal_path_for(path)),
                fp: fp.clone(),
            })
        } else {
            None
        };
        let (ix, mut rep) = run(bytes, cfg, spec);
        if rep.note.is_empty() {
            rep.note = backend.to_string();
        } else {
            rep.note = format!("{backend}: {}", rep.note);
        }
        if let Some(c) = cache {
            let buf = xsi::encode(&ix, false);
            match c.put(&fp, &buf) {
                Ok(_) => {}
                Err(e) => rep.note = format!("{} (cache write failed: {e})", rep.note),
            }
        }
        return Ok((ix, rep));
    }

    // No mmap: the segments cannot share the document, so stream it once.
    let total = src.len();
    let t0 = Instant::now();
    let ix = sequential(&mut src, cfg.scan)?;
    let mut rep = BuildReport {
        bytes: total,
        threads: 1,
        segments: 1,
        note: format!("{backend}: not random-access, streamed one thread"),
        ..Default::default()
    };
    close_out(&mut rep, t0);
    if let Some(c) = cache {
        let buf = xsi::encode(&ix, false);
        let _ = c.put(&fp, &buf);
    }
    Ok((ix, rep))
}

/// Finish a build that was interrupted, using its journal.
///
/// Returns `Ok(None)` when there is nothing to resume: no journal, a journal for a
/// different document (the fingerprint no longer matches), a source that cannot be mapped,
/// or cuts that no longer land where the journal says they did. In every one of those
/// cases the caller should just run [`build_file`].
///
/// When it does resume, the split pass is re-run (it is cheap: no records, no diagnostics)
/// to reproduce the [`BoundaryState`] at each recorded cut — the journal stores offsets,
/// not scanner state — and only the segments that never finished are scanned.
///
/// # Errors
/// When the document cannot be opened or fingerprinted.
pub fn resume_file(
    path: &Path,
    cfg: &ParallelConfig,
) -> std::io::Result<Option<(StructuralIndex, BuildReport)>> {
    let jpath = cfg
        .journal_path
        .clone()
        .unwrap_or_else(|| journal_path_for(path));
    let rec = match xmlspy_io::recover(&jpath) {
        Ok(r) => r,
        Err(_) => return Ok(None),
    };
    if rec.segments.is_empty() {
        return Ok(None);
    }
    let src = open_byte_source(path)?;
    let fp = Fingerprint::of(path)?;
    if rec.header.source_len != src.len() || !rec.header.source.same_content(&fp) {
        return Ok(None); // the file changed under the journal
    }
    let bytes = match src.as_slice() {
        Some(b) => b,
        None => return Ok(None),
    };

    let scan_cfg = ScannerConfig {
        max_indexed: rec.header.max_indexed,
        stride: rec.header.stride,
        max_errors: rec.header.max_errors,
    };
    let n = rec.header.splits.len() + 1;
    let t0 = Instant::now();
    let mut plan = split::plan_at(bytes, scan_cfg, rec.header.splits.clone());
    let offsets: Vec<u64> = plan.splits.iter().map(|(o, _)| *o).collect();
    if offsets != rec.header.splits || plan.segments() != n {
        return Ok(None); // not the same document shape; do not guess
    }
    let split_secs = plan.secs;

    let mut bounds: Vec<(u64, u64)> = Vec::with_capacity(n);
    let mut prev = 0u64;
    for (off, _) in &plan.splits {
        bounds.push((prev, *off));
        prev = *off;
    }
    bounds.push((prev, bytes.len() as u64));

    // Whatever the journal has, decoded; the rest gets scanned now.
    let mut results: Vec<Option<StructuralIndex>> = (0..n).map(|_| None).collect();
    let mut reused = 0usize;
    for seg in &rec.segments {
        let i = seg.idx as usize;
        if i < n && results[i].is_none() {
            if let Ok(ix) = xsi::decode(&seg.xsi) {
                results[i] = Some(ix);
                reused += 1;
            }
        }
    }

    let mut todo: Vec<usize> = (0..n).filter(|i| results[*i].is_none()).collect();
    if !todo.is_empty() {
        let threads = todo.len().clamp(1, MAX_THREADS);
        let (tx, rx) = mpsc::channel::<(usize, StructuralIndex)>();
        let total = bytes.len() as u64;
        // Scan the missing segments in parallel among themselves.
        thread::scope(|sc| {
            let mut per_thread: Vec<Vec<usize>> = vec![Vec::new(); threads];
            for (k, i) in todo.drain(..).enumerate() {
                per_thread[k % threads].push(i);
            }
            for group in per_thread {
                if group.is_empty() {
                    continue;
                }
                let tx = tx.clone();
                let mut seeds: Vec<(usize, Option<BoundaryState>)> = Vec::with_capacity(group.len());
                for &i in &group {
                    let seed = if i == 0 {
                        None
                    } else {
                        Some(plan.splits[i - 1].1.clone())
                    };
                    seeds.push((i, seed));
                }
                sc.spawn(move || {
                    for (i, seed) in seeds {
                        let (a, b) = bounds[i];
                        let ix = scan_segment(scan_cfg, bytes, a, b, seed, i + 1 == n, total);
                        let _ = tx.send((i, ix));
                    }
                });
            }
            drop(tx);
            for (i, ix) in rx.iter() {
                results[i] = Some(ix);
            }
        });
    }
    let scan_secs = t0.elapsed().as_secs_f64() - split_secs;

    let merge_t = Instant::now();
    let segments: Vec<SegmentIndex> = results
        .into_iter()
        .enumerate()
        .map(|(i, ix)| SegmentIndex {
            ix: ix.unwrap_or_default(),
            seed_depth: if i == 0 { 0 } else { 1 },
            is_last: i + 1 == n,
        })
        .collect();
    let ix = merge::merge(MergeInput {
        cfg: scan_cfg,
        total: bytes.len() as u64,
        segments,
        depth0_closes: std::mem::take(&mut plan.depth0_closes),
        base_names: std::mem::take(&mut plan.names),
    });
    let merge_secs = merge_t.elapsed().as_secs_f64();

    let _ = std::fs::remove_file(&jpath); // the build is complete; the log has served its purpose

    let mut rep = BuildReport {
        bytes: bytes.len() as u64,
        threads: (n - reused).max(1),
        segments: n,
        parallel: n - reused > 1,
        split_secs,
        scan_secs: scan_secs.max(0.0),
        merge_secs,
        reused,
        rescanned: n - reused,
        note: format!("resumed {reused}/{n} segments from {}", jpath.display()),
        ..Default::default()
    };
    close_out(&mut rep, t0);
    Ok(Some((ix, rep)))
}

/// Where to journal, and the fingerprint that ties the journal to a document.
struct JournalSpec {
    path: PathBuf,
    fp: Fingerprint,
}

/// The pipeline: split, scan in parallel, merge. Falls back to one sequential scan
/// whenever splitting does not pay.
fn run(
    bytes: &[u8],
    cfg: &ParallelConfig,
    spec: Option<JournalSpec>,
) -> (StructuralIndex, BuildReport) {
    let total = bytes.len() as u64;
    let t0 = Instant::now();
    let threads = cfg.thread_budget(total);
    let mut rep = BuildReport {
        bytes: total,
        threads: 1,
        segments: 1,
        ..Default::default()
    };

    if threads < 2 {
        let why = if cfg.threads == 1 {
            "one thread requested"
        } else {
            "document smaller than two segments"
        };
        let ix = scan_bytes(bytes, cfg.scan);
        rep.note = why.to_string();
        close_out(&mut rep, t0);
        return (ix, rep);
    }

    let min_seg = cfg.min_segment();
    let mut plan = split::plan(bytes, cfg.scan, threads, min_seg);
    rep.split_secs = plan.secs;
    if plan.is_single() {
        let ix = scan_bytes(bytes, cfg.scan);
        rep.note = plan.note.to_string();
        close_out(&mut rep, t0);
        return (ix, rep);
    }

    let mut bounds: Vec<(u64, u64)> = Vec::with_capacity(plan.segments());
    let mut prev = 0u64;
    for (off, _) in &plan.splits {
        bounds.push((prev, *off));
        prev = *off;
    }
    bounds.push((prev, total));
    let n = bounds.len();
    rep.segments = n;
    rep.threads = n;
    rep.parallel = true;

    // The journal is opened before any thread starts: a build that dies in the split pass
    // leaves nothing behind, and one that dies mid-scan leaves whatever finished.
    let mut journal: Option<Journal> = None;
    if let Some(spec) = &spec {
        let header = JournalHeader {
            stride: cfg.scan.stride,
            max_indexed: cfg.scan.max_indexed,
            max_errors: cfg.scan.max_errors,
            threads: n as u32,
            source_len: total,
            source: spec.fp.clone(),
            splits: plan.splits.iter().map(|(o, _)| *o).collect(),
        };
        match Journal::create(&spec.path, &header, false) {
            Ok(j) => journal = Some(j),
            Err(e) => rep.note = format!("journal disabled: {e}"),
        }
    }

    let scan_t = Instant::now();
    let mut results: Vec<Option<StructuralIndex>> = (0..n).map(|_| None).collect();
    let (tx, rx) = mpsc::channel::<(usize, StructuralIndex)>();
    thread::scope(|sc| {
        for (i, &(a, b)) in bounds.iter().enumerate() {
            let tx = tx.clone();
            let scan_cfg = cfg.scan;
            let seed = if i == 0 {
                None
            } else {
                Some(plan.splits[i - 1].1.clone())
            };
            let is_last = i + 1 == n;
            sc.spawn(move || {
                let ix = scan_segment(scan_cfg, bytes, a, b, seed, is_last, total);
                // A full channel cannot happen (capacity is unbounded), and a dead
                // receiver would mean the build was abandoned anyway.
                let _ = tx.send((i, ix));
            });
        }
        drop(tx); // the loop below ends when every worker has sent its result
        for (i, ix) in rx.iter() {
            if let Some(j) = journal.as_mut() {
                let buf = xsi::encode(&ix, i + 1 != n);
                match j.append_segment(i as u32, &buf) {
                    Ok(()) => rep.journaled += 1,
                    Err(e) => {
                        if rep.note.is_empty() {
                            rep.note = format!("journal write failed: {e}");
                        }
                        journal = None; // stop trying; the build itself is unaffected
                    }
                }
            }
            results[i] = Some(ix);
        }
    });
    rep.scan_secs = scan_t.elapsed().as_secs_f64();

    if let Some(mut j) = journal {
        let path = j.path().to_path_buf();
        if j.commit(total).is_ok() {
            // Committed and merged: the log has done its job. A crash between here and the
            // merge is covered by the index cache, not by the journal.
            let _ = std::fs::remove_file(&path);
        }
    }

    let merge_t = Instant::now();
    let segments: Vec<SegmentIndex> = results
        .into_iter()
        .enumerate()
        .map(|(i, ix)| SegmentIndex {
            ix: ix.unwrap_or_default(),
            seed_depth: if i == 0 { 0 } else { 1 },
            is_last: i + 1 == n,
        })
        .collect();
    let ix = merge::merge(MergeInput {
        cfg: cfg.scan,
        total,
        segments,
        depth0_closes: std::mem::take(&mut plan.depth0_closes),
        base_names: std::mem::take(&mut plan.names),
    });
    rep.merge_secs = merge_t.elapsed().as_secs_f64();
    close_out(&mut rep, t0);
    (ix, rep)
}

/// Fill in the derived timing fields of a report.
fn close_out(rep: &mut BuildReport, t0: Instant) {
    rep.total_secs = t0.elapsed().as_secs_f64();
    rep.mb_per_s = if rep.total_secs > 0.0 {
        rep.bytes as f64 / rep.total_secs / 1e6
    } else {
        0.0
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    fn corpus(records: usize) -> String {
        let mut s = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<PurchaseOrders>\n");
        for i in 0..records {
            s.push_str(&format!(
                "  <Order id=\"{i}\" total=\"{i}.00\"><Item sku=\"A{i}\">widget</Item><Ship><City>Pune</City></Ship></Order>\n"
            ));
        }
        s.push_str("</PurchaseOrders>\n");
        s
    }

    #[test]
    fn thread_budget_respects_size_and_ceiling() {
        let cfg = ParallelConfig::new().with_threads(8).with_min_segment(1 << 20);
        assert_eq!(cfg.thread_budget(0), 1);
        assert_eq!(cfg.thread_budget(1 << 20), 1, "one segment");
        assert_eq!(cfg.thread_budget(4 << 20), 4, "four segments");
        assert_eq!(cfg.thread_budget(1 << 30), 8, "capped by the request");

        let auto = ParallelConfig::new().with_min_segment(1 << 20);
        let cpus = thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
        assert_eq!(auto.thread_budget(1 << 30), cpus.min(MAX_THREADS));
        assert_eq!(ParallelConfig::new().with_threads(4096).thread_budget(u64::MAX), MAX_THREADS);
    }

    #[test]
    fn small_documents_stay_sequential() {
        let src = corpus(10);
        let cfg = ParallelConfig::new().with_threads(4);
        let (ix, rep) = build_bytes(src.as_bytes(), &cfg);
        assert!(!rep.parallel);
        assert_eq!(rep.segments, 1);
        assert_eq!(ix, scan_bytes(src.as_bytes(), cfg.scan));
    }

    #[test]
    fn forced_split_matches_the_sequential_index() {
        // `min_segment = 1` forces the split pass on a document far below the default
        // threshold, which is how the parity suite exercises the whole pipeline cheaply.
        let src = corpus(400);
        let cfg = ParallelConfig::new()
            .with_threads(4)
            .with_min_segment(1)
            .with_scan(ScannerConfig {
                max_indexed: 50,
                stride: 8,
                max_errors: 64,
            });
        let (par, rep) = build_bytes(src.as_bytes(), &cfg);
        let seq = scan_bytes(src.as_bytes(), cfg.scan);
        assert!(rep.parallel, "{}", rep.note);
        assert_eq!(rep.segments, 4);
        assert_eq!(par, seq);
    }

    #[test]
    fn a_document_with_nothing_to_split_says_so() {
        let src = "<root><child>text</child></root>";
        let cfg = ParallelConfig::new().with_threads(4).with_min_segment(1);
        let (ix, rep) = build_bytes(src.as_bytes(), &cfg);
        assert!(!rep.parallel);
        assert!(!rep.note.is_empty(), "the report must explain the fallback");
        assert_eq!(ix, scan_bytes(src.as_bytes(), cfg.scan));
    }

    #[test]
    fn truncated_documents_still_merge_exactly() {
        let full = corpus(200);
        for cut in [0usize, 1, 40, 512, 4096, full.len() - 20, full.len()] {
            let bytes = &full.as_bytes()[..cut.min(full.len())];
            let cfg = ParallelConfig::new().with_threads(3).with_min_segment(1);
            let (par, _) = build_bytes(bytes, &cfg);
            let seq = scan_bytes(bytes, cfg.scan);
            assert_eq!(par, seq, "truncated at {cut}");
        }
    }

    #[test]
    fn streaming_a_source_gives_the_same_index() {
        let src = corpus(200);
        let bytes = src.as_bytes();
        let cfg = ScannerConfig {
            max_indexed: 1000,
            stride: 16,
            max_errors: 32,
        };
        let mut vs = xmlspy_core::VecSource::new(bytes.to_vec());
        let streamed = sequential(&mut vs, cfg).unwrap();
        assert_eq!(streamed, scan_bytes(bytes, cfg));
    }
}
