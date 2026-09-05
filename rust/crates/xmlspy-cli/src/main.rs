//! `xmlspy` — the headless engine.
//!
//! ```text
//! xmlspy wf     <file> [--max-errors N]          well-formedness, RaptorXML-style exit codes
//! xmlspy index  <file> [--out F.xsi] [--stride N] [--max-indexed N]
//! xmlspy info   <file.xsi>                       inspect a cached index
//! xmlspy search <file> <needle> [-i] [--max N]
//! xmlspy gen    --size 100MiB --out corpus.xml   deterministic synthetic corpus
//! xmlspy bench  <file>                           MB/s, index size, RSS
//! ```
//!
//! Exit codes: `0` success, `1` document not well-formed, `2` usage/IO error.

use std::fs::File;
use std::io::{BufWriter, Read, Write};
use std::process::ExitCode;
use std::time::Instant;

use xmlspy_core::CHUNK_SIZE;
use xmlspy_index::{xsi, StructuralIndex};
use xmlspy_parse::{Finder, Scanner, ScannerConfig};

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
        "xmlspy {} — XMLSpy-rs headless engine\n\n\
         USAGE:\n  \
           xmlspy wf     <file> [--max-errors N]\n  \
           xmlspy index  <file> [--out F.xsi] [--stride N] [--max-indexed N]\n  \
           xmlspy info   <file.xsi>\n  \
           xmlspy search <file> <needle> [-i] [--max N]\n  \
           xmlspy gen    --size 100MiB --out corpus.xml\n  \
           xmlspy bench  <file>\n\n\
         Exit codes: 0 = ok, 1 = not well-formed, 2 = usage/IO error.",
        env!("CARGO_PKG_VERSION")
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

/// Stream a file through `f` in 8 MiB chunks. This is the `ByteSource` the browser
/// implements with `Blob.slice`; native builds can swap in `mmap` behind a feature flag.
fn stream<F: FnMut(&[u8], u64)>(path: &str, mut f: F) -> Result<u64, String> {
    let mut file = File::open(path).map_err(|e| format!("{path}: {e}"))?;
    let mut buf = vec![0u8; CHUNK_SIZE];
    let mut off = 0u64;
    loop {
        let n = file.read(&mut buf).map_err(|e| format!("{path}: {e}"))?;
        if n == 0 {
            break;
        }
        f(&buf[..n], off);
        off += n as u64;
    }
    Ok(off)
}

fn scan_file(path: &str, cfg: ScannerConfig) -> Result<(StructuralIndex, f64, u64), String> {
    let mut s = Scanner::new(cfg);
    let t0 = Instant::now();
    let total = stream(path, |buf, off| s.feed(buf, off))?;
    s.finish(total);
    let secs = t0.elapsed().as_secs_f64();
    Ok((s.into_index(), secs, total))
}

fn report_errors(ix: &StructuralIndex, path: &str) {
    for e in &ix.errors {
        println!("{path}:{}:{}: {}: {}", e.line, e.col, e.severity, e.msg);
        if let Some(fix) = &e.fix {
            println!("    SmartFix: {fix}");
        }
    }
}

// ---------------------------------------------------------------- commands

fn cmd_wf(args: &[String]) -> Result<u8, String> {
    let path = positional(args, 0).ok_or("usage: xmlspy wf <file>")?;
    let max_errors = flag(args, "--max-errors")
        .and_then(|v| v.parse().ok())
        .unwrap_or(200);
    let (ix, secs, total) = scan_file(
        path,
        ScannerConfig {
            max_errors,
            ..Default::default()
        },
    )?;
    report_errors(&ix, path);
    let mbs = if secs > 0.0 {
        total as f64 / 1e6 / secs
    } else {
        f64::INFINITY
    };
    if ix.error_count == 0 {
        println!(
            "{path}: well-formed — {} in {secs:.3} s ({mbs:.0} MB/s), {} elements, {} attributes, {} lines, depth {}",
            human(total), ix.total_elements, ix.total_attributes, ix.line_count, ix.max_depth
        );
        Ok(0)
    } else {
        println!(
            "{path}: NOT well-formed — {} error(s) ({} listed)",
            ix.error_count,
            ix.errors.len()
        );
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
    let out = flag(args, "--out")
        .map(String::from)
        .unwrap_or_else(|| format!("{path}.xsi"));
    let (ix, secs, total) = scan_file(
        path,
        ScannerConfig {
            stride,
            max_indexed,
            max_errors: 200,
        },
    )?;
    let buf = xsi::encode(&ix, false);
    std::fs::write(&out, &buf).map_err(|e| format!("{out}: {e}"))?;
    let mbs = if secs > 0.0 {
        total as f64 / 1e6 / secs
    } else {
        f64::INFINITY
    };
    println!(
        "indexed {} in {secs:.3} s ({mbs:.0} MB/s) → {out} ({}, {:.3} % of the document)",
        human(total),
        human(buf.len() as u64),
        buf.len() as f64 / total.max(1) as f64 * 100.0
    );
    println!(
        "  {} elements indexed / {} seen · {} checkpoints (stride {}) · {} names · depth {} · {} error(s)",
        ix.indexed_elements, ix.total_elements, ix.checkpoints.len(), ix.stride, ix.names.len(), ix.max_depth, ix.error_count
    );
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
    let mbs = if secs > 0.0 {
        total as f64 / 1e6 / secs
    } else {
        f64::INFINITY
    };
    println!(
        "{} hit(s) in {} — {secs:.3} s ({mbs:.0} MB/s streamed)",
        f.total(),
        human(total)
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
        written as f64 / 1e6 / secs.max(1e-9)
    );
    Ok(0)
}

fn cmd_bench(args: &[String]) -> Result<u8, String> {
    let path = positional(args, 0).ok_or("usage: xmlspy bench <file>")?;
    println!("benchmark: {path}");
    let mut best = f64::MAX;
    let mut ix_out = None;
    let mut total_bytes = 0;
    for run in 1..=3 {
        let (ix, secs, total) = scan_file(path, ScannerConfig::default())?;
        let mbs = total as f64 / 1e6 / secs.max(1e-9);
        println!("  run {run}: {secs:.3} s · {mbs:.0} MB/s");
        best = best.min(secs);
        total_bytes = total;
        ix_out = Some(ix);
    }
    let ix = ix_out.unwrap();
    let buf = xsi::encode(&ix, false);
    let mbs = total_bytes as f64 / 1e6 / best.max(1e-9);
    println!("  best: {best:.3} s · {mbs:.0} MB/s single-thread");
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
        "  gate index >= 500 MB/s single-thread — {}",
        if mbs >= 500.0 {
            "PASS"
        } else {
            "FAIL (see bench/reports)"
        }
    );
    Ok(0)
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
}
