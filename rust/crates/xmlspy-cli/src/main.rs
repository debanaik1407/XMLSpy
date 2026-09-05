//! `xmlspy` — the headless engine.
//!
//! ```text
//! xmlspy wf          <file> [--max-errors N] [--threads N] [--sequential] [--min-segment S]
//! xmlspy index       <file> [--out F.xsi] [--stride N] [--max-indexed N]
//!                            [--threads N] [--sequential] [--journal] [--min-segment S]
//! xmlspy info        <file.xsi>
//! xmlspy search      <file> <needle> [-i] [--max N]
//! xmlspy gen         --size 100MiB --out corpus.xml
//! xmlspy bench       <file> [--threads N] [--runs N] [--min-segment S]
//! xmlspy edit        <file> (--insert-after L | --delete-line L | --replace-line L)
//!                            [--text T] [--repeat N] [--out F] [--show]
//! xmlspy fold        <file> [--min-lines N] [--lines A-B] [--bracket OFFSET]
//!                            [--line N] [--bookmark 1,5,9]
//! xmlspy recover     <file> [--journal J] [--out F.xsi] [--threads N]
//! xmlspy conformance [--suite DIR] [--verbose] [--max-errors N]
//! ```
//!
//! Exit codes: `0` success, `1` document not well-formed / nothing found / conformance
//! below 100 %, `2` usage or I/O error.
//!
//! # How documents are read
//!
//! Every command goes through `xmlspy-io`: `mmap` where the platform has it (audited, the
//! only `unsafe` in the workspace), a buffered `read()` loop where it does not. `xmlspy wf`
//! reports which one it got. `xmlspy index` and `xmlspy bench` additionally build the index
//! with as many threads as the document and the machine allow (`--threads 1` or
//! `--sequential` pins it to one), and the parallel build is bit-identical to the sequential
//! one — `xmlspy-parallel/tests/parity.rs` is what makes that a promise rather than a hope.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use xmlspy_core::{ByteSource, CHUNK_SIZE};
use xmlspy_index::fold::{self, Bookmarks, FoldSet};
use xmlspy_index::{xsi, StructuralIndex};
use xmlspy_io::{open_byte_source, IndexCache, DEFAULT_BUDGET};
use xmlspy_parallel::{scan_bytes, BuildReport, ParallelConfig};
use xmlspy_parse::{Finder, ScannerConfig};
use xmlspy_rope::Rope;

use xmlspy_cli::conformance;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = args.first().map(String::as_str) else {
        usage();
        return ExitCode::from(2);
    };
    let rest = &args[1..];
    let r = match cmd {
        "wf" => cmd_wf(rest),
        "index" => cmd_index(rest),
        "info" => cmd_info(rest),
        "search" => cmd_search(rest),
        "gen" => cmd_gen(rest),
        "bench" => cmd_bench(rest),
        "edit" => cmd_edit(rest),
        "fold" => cmd_fold(rest),
        "recover" => cmd_recover(rest),
        "conformance" => cmd_conformance(rest),
        "-h" | "--help" | "help" => {
            usage();
            Ok(0)
        }
        "-V" | "--version" | "version" => {
            println!("xmlspy {}", env!("CARGO_PKG_VERSION"));
            Ok(0)
        }
        other => Err(format!("unknown command '{other}' (try --help)")),
    };
    match r {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(2)
        }
    }
}

fn usage() {
    eprintln!(
        "xmlspy {v} — XMLSpy-rs headless engine\n\n\
         USAGE:\n  \
           xmlspy wf          <file> [--max-errors N] [--threads N] [--sequential]\n  \
           xmlspy index       <file> [--out F.xsi] [--stride N] [--max-indexed N] [--threads N] [--journal]\n  \
           xmlspy info        <file.xsi>\n  \
           xmlspy search      <file> <needle> [-i] [--max N]\n  \
           xmlspy gen         --size 100MiB --out corpus.xml\n  \
           xmlspy bench       <file> [--threads N] [--runs N] [--min-segment S]\n  \
           xmlspy edit        <file> --insert-after L --text T [--repeat N] [--out F] [--show]\n  \
           xmlspy edit        <file> --delete-line L | --replace-line L --text T [--out F]\n  \
           xmlspy fold        <file> [--min-lines N] [--lines A-B] [--bracket OFFSET] [--line N] [--bookmark 1,5,9]\n  \
           xmlspy recover     <file> [--journal J] [--out F.xsi]\n  \
           xmlspy conformance [--suite DIR] [--verbose]\n\n\
         Shared flags: --threads N (0 = auto), --sequential, --min-segment S (e.g. 32MiB),\n  \
                       --no-index (skip the .xsi cache), --cache-dir D, --cache-budget S\n\n\
         Exit codes: 0 = ok, 1 = not well-formed / nothing found, 2 = usage or I/O error.",
        v = env!("CARGO_PKG_VERSION")
    );
}

// ---------------------------------------------------------------- helpers

fn flag<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    let i = args.iter().position(|a| a == name)?;
    args.get(i + 1).map(String::as_str)
}

fn has(args: &[String], name: &str) -> bool {
    args.iter().any(|a| a == name)
}

fn positional(args: &[String], n: usize) -> Option<&str> {
    args.iter()
        .enumerate()
        .filter(|(i, a)| {
            !a.starts_with('-')
                && !args
                    .get(i.wrapping_sub(1))
                    .is_some_and(|p| p.starts_with("--"))
        })
        .map(|(_, a)| a.as_str())
        .nth(n)
}

fn parse_size(s: &str) -> Result<u64, String> {
    let t = s.trim();
    let (num, mult) = if let Some(v) = t.strip_suffix("GiB") {
        (v, 1u64 << 30)
    } else if let Some(v) = t.strip_suffix("MiB") {
        (v, 1u64 << 20)
    } else if let Some(v) = t.strip_suffix("KiB") {
        (v, 1u64 << 10)
    } else if let Some(v) = t.strip_suffix('B') {
        (v, 1)
    } else {
        (t, 1)
    };
    num.trim()
        .parse::<f64>()
        .map(|v| (v * mult as f64) as u64)
        .map_err(|e| format!("bad size '{s}': {e}"))
}

fn human(n: u64) -> String {
    const U: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{v:.2} {}", U[i])
    }
}

/// Resident set size in bytes (Linux); `None` elsewhere.
fn rss_bytes() -> Option<u64> {
    let s = std::fs::read_to_string("/proc/self/statm").ok()?;
    let pages: u64 = s.split_whitespace().nth(1)?.parse().ok()?;
    Some(pages * 4096)
}

fn mbs(bytes: u64, secs: f64) -> f64 {
    if secs > 0.0 {
        bytes as f64 / 1e6 / secs
    } else {
        f64::INFINITY
    }
}

/// Stream a file through `f` in `CHUNK_SIZE` pieces (the `Blob.slice` loop's native twin).
fn stream<F: FnMut(&[u8], u64)>(path: &str, f: F) -> Result<u64, String> {
    xmlspy_io::stream_chunks(Path::new(path), CHUNK_SIZE, f).map_err(|e| format!("{path}: {e}"))
}

/// Build configuration from the shared flags.
fn parallel_config(args: &[String], scan: ScannerConfig) -> ParallelConfig {
    let threads = if has(args, "--sequential") {
        1
    } else {
        flag(args, "--threads")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0)
    };
    let min_segment = flag(args, "--min-segment")
        .and_then(|v| parse_size(v).ok())
        .unwrap_or(0);
    let mut cfg = ParallelConfig::new()
        .with_scan(scan)
        .with_threads(threads)
        .with_min_segment(min_segment);
    if has(args, "--journal") {
        cfg = cfg.with_journal(flag(args, "--journal-path").map(PathBuf::from));
    }
    cfg
}

fn open_cache(args: &[String]) -> Result<IndexCache, String> {
    let dir = flag(args, "--cache-dir")
        .map(PathBuf::from)
        .unwrap_or_else(IndexCache::default_dir);
    let budget = flag(args, "--cache-budget")
        .and_then(|v| parse_size(v).ok())
        .unwrap_or(DEFAULT_BUDGET);
    IndexCache::new(dir, budget).map_err(|e| e.to_string())
}

/// Build a file's index through mmap + the parallel builder (+ the `.xsi` cache unless
/// `--no-index`).
fn build(
    path: &str,
    args: &[String],
    scan: ScannerConfig,
    use_cache: bool,
) -> Result<(StructuralIndex, BuildReport), String> {
    let cfg = parallel_config(args, scan);
    let cache = if use_cache && !has(args, "--no-index") {
        Some(open_cache(args)?)
    } else {
        None
    };
    xmlspy_parallel::build_file(Path::new(path), &cfg, cache.as_ref())
        .map_err(|e| format!("{path}: {e}"))
}

fn report_errors(ix: &StructuralIndex, path: &str) {
    for e in &ix.errors {
        println!("{path}:{}:{}: {}: {}", e.line, e.col, e.severity, e.msg);
        if let Some(fix) = &e.fix {
            println!("    SmartFix: {fix}");
        }
    }
}

fn report_build(rep: &BuildReport) {
    println!("  build: {}", rep.summary());
    if rep.split_secs > 0.0 {
        println!(
            "    split {:.3} s · scan {:.3} s · merge {:.3} s",
            rep.split_secs, rep.scan_secs, rep.merge_secs
        );
    }
    if rep.reused > 0 {
        println!(
            "    reused {} segment(s) from the journal, rescanned {}",
            rep.reused, rep.rescanned
        );
    }
    if rep.journaled > 0 {
        println!("    journaled {} segment result(s)", rep.journaled);
    }
}

// ---------------------------------------------------------------- commands

fn cmd_wf(args: &[String]) -> Result<u8, String> {
    let path = positional(args, 0).ok_or("usage: xmlspy wf <file>")?;
    let max_errors = flag(args, "--max-errors")
        .and_then(|v| v.parse().ok())
        .unwrap_or(200);
    let stride = flag(args, "--stride")
        .and_then(|v| v.parse().ok())
        .unwrap_or(32);
    // Well-formedness does not need the tree: `max_indexed = 0` retains no records, which
    // is the route the benchmark report calls "wf without index".
    let scan = ScannerConfig {
        max_indexed: 0,
        stride,
        max_errors,
    };
    let (ix, rep) = build(path, args, scan, false)?;
    report_errors(&ix, path);
    if ix.error_count == 0 {
        println!(
            "{path}: well-formed — {} in {:.3} s ({:.0} MB/s), {} elements, {} attributes, {} lines, depth {}",
            human(rep.bytes), rep.total_secs, rep.mb_per_s,
            ix.total_elements, ix.total_attributes, ix.line_count, ix.max_depth
        );
        report_build(&rep);
        Ok(0)
    } else {
        println!(
            "{path}: NOT well-formed — {} error(s) ({} listed)",
            ix.error_count,
            ix.errors.len()
        );
        report_build(&rep);
        Ok(1)
    }
}

fn cmd_index(args: &[String]) -> Result<u8, String> {
    let path = positional(args, 0).ok_or("usage: xmlspy index <file> [--out F.xsi]")?;
    let stride = flag(args, "--stride")
        .and_then(|v| v.parse().ok())
        .unwrap_or(32);
    let max_indexed = flag(args, "--max-indexed")
        .and_then(|v| v.parse().ok())
        .unwrap_or(u32::MAX / 2);
    let explicit_out = flag(args, "--out").map(String::from);
    let out = explicit_out
        .clone()
        .unwrap_or_else(|| format!("{path}.xsi"));
    let scan = ScannerConfig {
        stride,
        max_indexed,
        max_errors: 200,
    };
    // An explicit `--out` means "produce this artefact", so the shared cache stays out of
    // the way (and out of the timing). Without it, `xmlspy index` uses the cache and still
    // writes the default `{path}.xsi`.
    let use_cache = explicit_out.is_none();
    let (ix, rep) = build(path, args, scan, use_cache)?;
    let buf = xsi::encode(&ix, false);
    std::fs::write(&out, &buf).map_err(|e| format!("{out}: {e}"))?;
    println!(
        "indexed {} in {:.3} s ({:.0} MB/s) → {out} ({}, {:.3} % of the document)",
        human(rep.bytes),
        rep.total_secs,
        rep.mb_per_s,
        human(buf.len() as u64),
        buf.len() as f64 / rep.bytes.max(1) as f64 * 100.0
    );
    println!(
        "  {} elements indexed / {} seen · {} checkpoints (stride {}) · {} names · depth {} · {} error(s)",
        ix.indexed_elements, ix.total_elements, ix.checkpoints.len(), ix.stride, ix.names.len(), ix.max_depth, ix.error_count
    );
    report_build(&rep);
    if let Some(rss) = rss_bytes() {
        println!("  peak RSS {}", human(rss));
    }
    Ok(u8::from(ix.error_count > 0))
}

fn cmd_info(args: &[String]) -> Result<u8, String> {
    let path = positional(args, 0).ok_or("usage: xmlspy info <file.xsi>")?;
    let buf = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
    let ix = xsi::decode(&buf).map_err(|e| e.to_string())?;
    println!(
        "{path}: .xsi v{} — {} of document",
        xsi::VERSION,
        human(ix.file_len)
    );
    println!(
        "  lines {} · stride {} · checkpoints {}",
        ix.line_count,
        ix.stride,
        ix.checkpoints.len()
    );
    println!(
        "  elements {} indexed / {} total · attributes {} · depth {}",
        ix.indexed_elements, ix.total_elements, ix.total_attributes, ix.max_depth
    );
    println!(
        "  names {} · errors {} · heap {}",
        ix.names.len(),
        ix.error_count,
        human(ix.heap_bytes() as u64)
    );
    for (i, name) in ix.names.iter().take(10).enumerate() {
        println!("    name[{i}] = {name}");
    }
    // The cache side of the story, when this .xsi is one the engine wrote.
    if let Some(dir) = flag(args, "--cache-dir").map(PathBuf::from) {
        if let Ok(cache) = IndexCache::new(dir.clone(), DEFAULT_BUDGET) {
            let st = cache.stats();
            println!(
                "  cache {}: {} entries, {} of {}",
                dir.display(),
                st.entries,
                human(st.bytes),
                human(st.budget)
            );
        }
    }
    Ok(0)
}

fn cmd_search(args: &[String]) -> Result<u8, String> {
    let path = positional(args, 0).ok_or("usage: xmlspy search <file> <needle>")?;
    let needle = positional(args, 1).ok_or("usage: xmlspy search <file> <needle>")?;
    let ci = has(args, "-i") || has(args, "--ignore-case");
    let max = flag(args, "--max")
        .and_then(|v| v.parse().ok())
        .unwrap_or(1000);
    let mut f = Finder::new(needle.as_bytes(), ci, max);
    let t0 = Instant::now();
    let total = stream(path, |buf, off| f.feed(buf, off))?;
    f.finish();
    let secs = t0.elapsed().as_secs_f64();
    for h in f.hits() {
        println!("{path}:{}:{}: offset {}", h.line, h.col, h.offset);
    }
    println!(
        "{} hit(s) in {} — {secs:.3} s ({:.0} MB/s streamed)",
        f.total(),
        human(total),
        mbs(total, secs)
    );
    Ok(u8::from(f.total() == 0))
}

fn cmd_gen(args: &[String]) -> Result<u8, String> {
    let size =
        parse_size(flag(args, "--size").ok_or("usage: xmlspy gen --size 100MiB --out f.xml")?)?;
    let out = flag(args, "--out").ok_or("usage: xmlspy gen --size 100MiB --out f.xml")?;
    let f = File::create(out).map_err(|e| format!("{out}: {e}"))?;
    let mut w = BufWriter::with_capacity(1 << 20, f);
    let t0 = Instant::now();

    const CITIES: [&str; 6] = [
        "Vienna",
        "Boston",
        "Mill Valley",
        "Zürich",
        "São Paulo",
        "Tōkyō",
    ];
    const PRODUCTS: [&str; 6] = [
        "Lawnmower",
        "Baby Monitor",
        "Router",
        "Keyboard",
        "Webcam",
        "Desk Lamp",
    ];
    let header = b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!-- deterministic corpus generated by xmlspy gen -->\n<PurchaseOrders xmlns=\"urn:xmlspy:orders\">\n";
    let footer = b"</PurchaseOrders>\n";
    w.write_all(header).map_err(|e| e.to_string())?;
    let mut written = header.len() as u64;
    let mut i: u64 = 0;
    while written + footer.len() as u64 + 512 < size {
        let city = CITIES[(i % CITIES.len() as u64) as usize];
        let product = PRODUCTS[(i % PRODUCTS.len() as u64) as usize];
        let rec = format!(
            "  <PurchaseOrder id=\"{i}\" date=\"2026-09-05\">\n    <Address type=\"Ship\"><Name>Customer {i}</Name><City>{city}</City><Zip>{:05}</Zip></Address>\n    <Items><Item PartNumber=\"P-{:06}\"><ProductName>{product}</ProductName><Quantity>{}</Quantity><USPrice>{}.99</USPrice></Item></Items>\n  </PurchaseOrder>\n",
            i % 99999,
            i % 999999,
            i % 9 + 1,
            i % 997
        );
        w.write_all(rec.as_bytes()).map_err(|e| e.to_string())?;
        written += rec.len() as u64;
        i += 1;
    }
    w.write_all(footer).map_err(|e| e.to_string())?;
    written += footer.len() as u64;
    w.flush().map_err(|e| e.to_string())?;
    let secs = t0.elapsed().as_secs_f64();
    println!(
        "wrote {out}: {} ({i} PurchaseOrder records) in {secs:.2} s ({:.0} MB/s)",
        human(written),
        mbs(written, secs)
    );
    Ok(0)
}

fn cmd_bench(args: &[String]) -> Result<u8, String> {
    let path = positional(args, 0).ok_or("usage: xmlspy bench <file>")?;
    let runs = flag(args, "--runs")
        .and_then(|v| v.parse().ok())
        .unwrap_or(3u32)
        .clamp(1, 20);
    let threads = flag(args, "--threads")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0usize);
    // Defaults are the shipped ones, so these numbers stay comparable with the reports in
    // bench/reports/. `--max-indexed 2147483647` measures the uncapped index instead.
    let idx_cfg = ScannerConfig {
        stride: flag(args, "--stride")
            .and_then(|v| v.parse().ok())
            .unwrap_or(ScannerConfig::default().stride),
        max_indexed: flag(args, "--max-indexed")
            .and_then(|v| v.parse().ok())
            .unwrap_or(ScannerConfig::default().max_indexed),
        ..ScannerConfig::default()
    };
    println!("benchmark: {path}");

    // Route 1: well-formedness with no index retained — the throughput gate's number.
    let wf_cfg = ScannerConfig {
        max_indexed: 0,
        ..idx_cfg
    };
    let mut wf_best = f64::MAX;
    let mut total_bytes = 0u64;
    let mut backend = String::from("?");
    for run in 1..=runs {
        let (_, rep) = build(path, &["--sequential".to_string()], wf_cfg, false)?;
        total_bytes = rep.bytes;
        backend = rep.note.clone();
        println!(
            "  wf   run {run}: {:.3} s · {:.0} MB/s",
            rep.total_secs, rep.mb_per_s
        );
        wf_best = wf_best.min(rep.total_secs);
    }
    let wf_mbs = mbs(total_bytes, wf_best);

    // Route 2: the full index, sequential and then with the requested thread count.
    let mut ix_out = None;
    let mut idx_best = f64::MAX;
    for run in 1..=runs {
        let (ix, rep) = build(path, &["--sequential".to_string()], idx_cfg, false)?;
        println!(
            "  idx  run {run}: {:.3} s · {:.0} MB/s (1 thread)",
            rep.total_secs, rep.mb_per_s
        );
        idx_best = idx_best.min(rep.total_secs);
        ix_out = Some(ix);
    }
    let idx_mbs = mbs(total_bytes, idx_best);

    let mut par_args: Vec<String> = vec!["--threads".to_string(), threads.to_string()];
    if let Some(v) = flag(args, "--min-segment") {
        par_args.push("--min-segment".to_string());
        par_args.push(v.to_string());
    }
    let (pix, prep) = build(path, &par_args, idx_cfg, false)?;
    let par_mbs = prep.mb_per_s;
    println!(
        "  idx  parallel: {:.3} s · {:.0} MB/s ({} segment(s){})",
        prep.total_secs,
        par_mbs,
        prep.segments,
        if prep.parallel {
            format!(", split {:.3} s + scan {:.3} s + merge {:.3} s", prep.split_secs, prep.scan_secs, prep.merge_secs)
        } else {
            format!(", {}", prep.note)
        }
    );
    if prep.parallel {
        // The parallel build promises a bit-identical index; the benchmark checks it here
        // too, so a regression shows up in the report and not only in `cargo test`.
        if let Some(seq_ix) = &ix_out {
            if &pix != seq_ix {
                println!("  WARNING: the parallel index differs from the sequential one");
            }
        }
    }

    let ix = ix_out.unwrap_or_default();
    let buf = xsi::encode(&ix, false);
    println!("  backend: {backend}");
    println!(
        "  best: wf {:.3} s · {:.0} MB/s | index {:.3} s · {:.0} MB/s single-thread",
        wf_best, wf_mbs, idx_best, idx_mbs
    );
    println!(
        "  document {} · index {} ({:.3} %)",
        human(total_bytes),
        human(buf.len() as u64),
        buf.len() as f64 / total_bytes.max(1) as f64 * 100.0
    );
    if let Some(rss) = rss_bytes() {
        println!(
            "  RSS {} (gate: < 512 MiB) — {}",
            human(rss),
            if rss < 512 << 20 { "PASS" } else { "FAIL" }
        );
    }
    println!(
        "  gate index >= 500 MB/s single-thread — {} ({:.0} MB/s)",
        if idx_mbs >= 500.0 { "PASS" } else { "FAIL" },
        idx_mbs
    );
    println!(
        "  gate index >= 1.2 GB/s at 8 threads — {} ({:.0} MB/s on {} segment(s), {} thread(s){})",
        if par_mbs >= 1200.0 && prep.threads >= 8 {
            "PASS"
        } else {
            "FAIL"
        },
        par_mbs,
        prep.segments,
        prep.threads,
        if prep.threads < 8 {
            ", pass --threads 8 --min-segment 1MiB to measure the gate"
        } else {
            ""
        }
    );
    Ok(0)
}

/// The rope edit buffer: edits cost one piece each, saving streams the untouched original.
fn cmd_edit(args: &[String]) -> Result<u8, String> {
    let path = positional(args, 0).ok_or(
        "usage: xmlspy edit <file> (--insert-after L | --delete-line L | --replace-line L) [--text T]",
    )?;
    let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
    let original_len = bytes.len() as u64;
    let mut rope = Rope::new(bytes);
    let text = flag(args, "--text").unwrap_or("");
    let repeat = flag(args, "--repeat")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(1)
        .clamp(1, 1_000_000);

    let insert_after = flag(args, "--insert-after").and_then(|v| v.parse::<usize>().ok());
    let delete_line = flag(args, "--delete-line").and_then(|v| v.parse::<usize>().ok());
    let replace_line = flag(args, "--replace-line").and_then(|v| v.parse::<usize>().ok());
    if insert_after.is_none() && delete_line.is_none() && replace_line.is_none() {
        return Err("nothing to do: pass --insert-after, --delete-line or --replace-line".into());
    }

    let t0 = Instant::now();
    let mut applied = 0usize;
    if let Some(l) = insert_after {
        for k in 0..repeat {
            rope.insert_line_after(l.saturating_sub(1) + k, text.as_bytes());
            applied += 1;
        }
    }
    if let Some(l) = delete_line {
        for _ in 0..repeat {
            // Repeated deletes walk down the document: each one removes what is now line L.
            rope.delete_line(l.saturating_sub(1));
            applied += 1;
        }
    }
    if let Some(l) = replace_line {
        let at = l.saturating_sub(1);
        let r = rope
            .line_range(at)
            .ok_or_else(|| format!("line {l} is past the end ({} lines)", rope.line_count()))?;
        rope.replace(r, text.as_bytes());
        applied += 1;
    }
    let edit_secs = t0.elapsed().as_secs_f64();
    let st = rope.stats();

    println!(
        "{path}: {} edit(s) in {:.6} s — {} → {} ({}), {} piece(s)",
        applied,
        edit_secs,
        human(original_len),
        human(rope.len() as u64),
        if rope.len() as u64 >= original_len { "+" } else { "-" },
        st.pieces
    );
    println!(
        "  {:.6} % of the document is still the untouched original · {} original run(s), longest {} · add buffer {}",
        rope.unchanged_ratio() * 100.0,
        st.original_runs,
        human(st.longest_original_run as u64),
        human(st.add_bytes as u64)
    );
    if rope.unchanged_ratio() > 0.999_999 {
        println!("  a 3-byte edit did not rewrite the document (gate: unchanged_ratio > 0.999999)");
    }

    if has(args, "--show") {
        let at = insert_after
            .or(replace_line)
            .or(delete_line)
            .unwrap_or(1)
            .saturating_sub(1);
        for l in at..(at + 5).min(rope.line_count()) {
            if let Some(line) = rope.line(l) {
                println!("  {:>6} | {}", l + 1, String::from_utf8_lossy(&line));
            }
        }
    }

    if let Some(out) = flag(args, "--out") {
        let f = File::create(out).map_err(|e| format!("{out}: {e}"))?;
        let mut w = BufWriter::with_capacity(1 << 20, f);
        let mut written = 0u64;
        let mut parts = 0u64;
        let t1 = Instant::now();
        rope.try_each_chunk(|c| -> Result<(), String> {
            w.write_all(c).map_err(|e| e.to_string())?;
            written += c.len() as u64;
            parts += 1;
            Ok(())
        })?;
        w.flush().map_err(|e| e.to_string())?;
        let secs = t1.elapsed().as_secs_f64();
        println!(
            "  saved {} in {parts} write(s), {secs:.3} s ({:.0} MB/s) → {out}",
            human(written),
            mbs(written, secs)
        );
    }
    Ok(0)
}

/// Folding, bracket matching and bookmarks, all answered from the index.
fn cmd_fold(args: &[String]) -> Result<u8, String> {
    let path = positional(args, 0).ok_or("usage: xmlspy fold <file> [--bracket OFFSET]")?;
    // Folding needs the bytes (to turn offsets into lines) as well as the index, so this
    // maps the document rather than reading it: on a 10 GiB file the copy would be the
    // whole cost. Where mmap is unavailable it falls back to one read.
    let src = open_byte_source(Path::new(path)).map_err(|e| format!("{path}: {e}"))?;
    let backend = src.kind();
    let owned;
    let bytes: &[u8] = match src.as_slice() {
        Some(b) => b,
        None => {
            owned = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
            &owned
        }
    };
    let stride = flag(args, "--stride")
        .and_then(|v| v.parse().ok())
        .unwrap_or(32);
    let max_indexed = flag(args, "--max-indexed")
        .and_then(|v| v.parse().ok())
        .unwrap_or(u32::MAX / 2);
    let t0 = Instant::now();
    let ix = scan_bytes(
        bytes,
        ScannerConfig {
            max_indexed,
            stride,
            max_errors: 200,
        },
    );
    let scan_secs = t0.elapsed().as_secs_f64();
    println!(
        "{path}: {} · {} lines · {} elements indexed ({backend}, {scan_secs:.3} s)",
        human(bytes.len() as u64),
        ix.line_count,
        ix.indexed_elements
    );

    if let Some(off) = flag(args, "--bracket").and_then(|v| v.parse::<u64>().ok()) {
        match fold::bracket_at(&ix, bytes, off) {
            Some(p) => {
                let name = ix.name_of(p.id as usize).unwrap_or("?");
                let open_line = fold::line_at(&ix, bytes, p.open);
                let close_line = fold::line_at(&ix, bytes, p.close.saturating_sub(1));
                println!(
                    "  offset {off} matches <{name}> — open {} (line {open_line}), close {} (line {close_line}){}",
                    p.open,
                    p.close,
                    if p.unclosed { ", unclosed" } else { "" }
                );
            }
            None => println!("  offset {off} is not on a tag (no bracket to match)"),
        }
        return Ok(0);
    }

    if let Some(line) = flag(args, "--line").and_then(|v| v.parse::<u64>().ok()) {
        // `--line` is 1-based, like every line number a human reads.
        match fold::line_offset(&ix, bytes, line) {
            Some(from) => {
                let next = fold::line_offset(&ix, bytes, line + 1)
                    .unwrap_or(bytes.len() as u64);
                println!("  line {line} is bytes {from}..{next}");
                match fold::enclosing(&ix, from) {
                    Some(id) => {
                        let i = id as usize;
                        let name = ix.name_of(i).unwrap_or("?");
                        let depth = ix.elem_depth.get(i).copied().unwrap_or(0);
                        let start = ix.elem_start.get(i).copied().unwrap_or(0);
                        let end = ix.elem_end.get(i).copied().unwrap_or(0);
                        let line = ix.elem_line.get(i).copied().unwrap_or(0);
                        println!(
                            "  enclosing element <{name}> (id {id}, depth {depth}): {start}..{end}, line {line}"
                        );
                        let kids = ix.children_of(id as usize);
                        println!("  {} child(ren)", kids.len());
                    }
                    None => println!("  not inside any element"),
                }
            }
            None => println!("  line {line} is past the end ({})", ix.line_count),
        }
    }

    if let Some(list) = flag(args, "--bookmark") {
        let lines: Vec<u64> = list
            .split(',')
            .filter_map(|v| v.trim().parse::<u64>().ok())
            .collect();
        let bm = Bookmarks::from_lines(lines);
        println!(
            "  {} bookmark(s): {:?}",
            bm.len(),
            bm.lines().iter().copied().collect::<Vec<u64>>()
        );
        if let Some(&first) = bm.lines().first() {
            println!(
                "  F2 from line {first} → {:?}, Shift+F2 → {:?}",
                bm.next(first),
                bm.prev(first)
            );
        }
        println!("  session encoding: {} bytes", bm.encode().len());
    }

    let min_lines = flag(args, "--min-lines")
        .and_then(|v| v.parse().ok())
        .unwrap_or(2u64);
    let (from_line, to_line) = match flag(args, "--lines") {
        Some(spec) => {
            let (a, b) = spec.split_once('-').unwrap_or((spec, ""));
            (
                a.parse::<u64>().unwrap_or(1),
                b.parse::<u64>().unwrap_or(u64::MAX),
            )
        }
        None => (1, u64::MAX),
    };
    let t1 = Instant::now();
    let regions = fold::fold_regions_in(&ix, bytes, min_lines, from_line, to_line);
    let fold_secs = t1.elapsed().as_secs_f64();
    println!(
        "  {} fold region(s) spanning {} line(s) or more (lines {from_line}..={}) in {fold_secs:.3} s",
        regions.len(),
        min_lines,
        if to_line == u64::MAX {
            ix.line_count.to_string()
        } else {
            to_line.to_string()
        }
    );
    for r in regions.iter().take(20) {
        let name = ix.name_of(r.id as usize).unwrap_or("?");
        println!(
            "    lines {:>6}..{:<6} <{name}> ({}..{}){}",
            r.start_line,
            r.end_line,
            r.start_off,
            r.end_off,
            if r.unclosed { " unclosed" } else { "" }
        );
    }
    if regions.len() > 20 {
        println!("    … {} more", regions.len() - 20);
    }
    // "Collapse all" then "expand all" is what the editor does on open, and what a session
    // file has to round-trip.
    let mut set = FoldSet::new();
    for r in &regions {
        set.collapse(r);
    }
    let encoded = set.encode();
    let back = FoldSet::decode(&encoded).unwrap_or_default();
    println!(
        "  collapse-all: {} region(s), {} bytes of session state, round-trip {}",
        set.len(),
        encoded.len(),
        if back == set { "ok" } else { "MISMATCH" }
    );
    Ok(u8::from(back != set))
}

/// Finish an interrupted build from its write-ahead journal.
fn cmd_recover(args: &[String]) -> Result<u8, String> {
    let path = positional(args, 0).ok_or("usage: xmlspy recover <file>")?;
    let mut cfg = parallel_config(
        args,
        ScannerConfig {
            stride: flag(args, "--stride")
                .and_then(|v| v.parse().ok())
                .unwrap_or(32),
            ..ScannerConfig::default()
        },
    );
    if let Some(j) = flag(args, "--journal") {
        cfg = cfg.with_journal(Some(PathBuf::from(j)));
    }
    let jpath = cfg
        .journal_path
        .clone()
        .unwrap_or_else(|| xmlspy_io::journal_path_for(Path::new(path)));
    // Say what is there even when it cannot be used: a stale journal is information.
    match xmlspy_io::recover(&jpath) {
        Ok(rec) => println!(
            "journal {}: {} segment(s), {} entries, committed {:?}{}",
            jpath.display(),
            rec.segments.len(),
            rec.entries_read,
            rec.committed,
            rec.reason
                .as_ref()
                .map(|r| format!(", stopped: {r}"))
                .unwrap_or_default()
        ),
        Err(e) => println!("journal {}: not usable ({e})", jpath.display()),
    }
    match xmlspy_parallel::resume_file(Path::new(path), &cfg).map_err(|e| format!("{path}: {e}"))? {
        Some((ix, rep)) => {
            println!(
                "recovered {} — {} element(s) indexed / {} seen, {} error(s)",
                human(rep.bytes),
                ix.indexed_elements,
                ix.total_elements,
                ix.error_count
            );
            report_build(&rep);
            report_errors(&ix, path);
            if let Some(out) = flag(args, "--out") {
                let buf = xsi::encode(&ix, false);
                std::fs::write(out, &buf).map_err(|e| format!("{out}: {e}"))?;
                println!("wrote {out} ({})", human(buf.len() as u64));
            }
            Ok(u8::from(ix.error_count > 0))
        }
        None => {
            println!(
                "{path}: nothing to resume (no journal, or it no longer matches the document)"
            );
            Ok(1)
        }
    }
}

fn cmd_conformance(args: &[String]) -> Result<u8, String> {
    let dir = flag(args, "--suite")
        .map(PathBuf::from)
        .unwrap_or_else(default_suite);
    if !dir.is_dir() {
        return Err(format!(
            "{} is not a directory (pass --suite DIR; the vendored suite lives in rust/conformance/mini)",
            dir.display()
        ));
    }
    let max_errors = flag(args, "--max-errors")
        .and_then(|v| v.parse().ok())
        .unwrap_or(16);
    let rep = conformance::run(&dir, conformance::runner_config(max_errors))?;
    print!("{}", conformance::render(&rep, has(args, "--verbose")));
    Ok(u8::from(!rep.all_passed()))
}

/// Where the vendored mini suite is, from the usual places a developer or CI runs this.
fn default_suite() -> PathBuf {
    for cand in [
        "rust/conformance/mini",
        "conformance/mini",
        "../conformance/mini",
        "../../conformance/mini",
    ] {
        let p = PathBuf::from(cand);
        if p.is_dir() {
            return p;
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(root) = exe.parent().and_then(|p| p.parent()) {
            for up in ["../../..", "../../../..", "../../../../.."] {
                let p = root.join(up).join("rust/conformance/mini");
                if p.is_dir() {
                    return p;
                }
            }
        }
    }
    PathBuf::from("rust/conformance/mini")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_parsing() {
        assert_eq!(parse_size("1024").unwrap(), 1024);
        assert_eq!(parse_size("2KiB").unwrap(), 2048);
        assert_eq!(parse_size("1.5MiB").unwrap(), 1_572_864);
        assert_eq!(parse_size("10GiB").unwrap(), 10 << 30);
        assert!(parse_size("banana").is_err());
    }

    #[test]
    fn human_sizes() {
        assert_eq!(human(512), "512 B");
        assert_eq!(human(2048), "2.00 KiB");
        assert_eq!(human(3 << 30), "3.00 GiB");
    }

    #[test]
    fn arg_helpers() {
        let a: Vec<String> = ["file.xml", "--out", "x.xsi", "-i"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(positional(&a, 0), Some("file.xml"));
        assert_eq!(flag(&a, "--out"), Some("x.xsi"));
        assert!(has(&a, "-i"));
        assert!(!has(&a, "--nope"));
    }

    #[test]
    fn throughput_helper() {
        assert_eq!(mbs(1_000_000, 1.0), 1.0);
        assert_eq!(mbs(1_000_000, 0.0), f64::INFINITY);
    }

    #[test]
    fn parallel_config_reads_the_shared_flags() {
        let a: Vec<String> = ["--threads", "4", "--min-segment", "1MiB", "--journal"]
            .iter()
            .map(String::from)
            .collect();
        let c = parallel_config(&a, ScannerConfig::default());
        assert_eq!(c.threads, 4);
        assert_eq!(c.min_segment_bytes, 1 << 20);
        assert!(c.journal);
        assert_eq!(c.min_segment(), 1 << 20);

        let seq: Vec<String> = ["--sequential", "--threads", "8"]
            .iter()
            .map(String::from)
            .collect();
        assert_eq!(parallel_config(&seq, ScannerConfig::default()).threads, 1);

        let plain: Vec<String> = Vec::new();
        let c = parallel_config(&plain, ScannerConfig::default());
        assert_eq!(c.threads, 0, "0 = automatic");
        assert_eq!(c.min_segment(), xmlspy_parallel::DEFAULT_MIN_SEGMENT_BYTES);
        assert!(!c.journal);
    }

    #[test]
    fn the_vendored_conformance_suite_is_where_the_cli_looks() {
        // Run from the crate directory in tests, so the relative candidate is ../../..
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../conformance/mini");
        assert!(dir.is_dir(), "{} must exist", dir.display());
        assert!(dir.join("manifest.tsv").is_file());
    }
}
