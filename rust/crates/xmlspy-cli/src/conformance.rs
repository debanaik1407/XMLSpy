//! The conformance runner behind `xmlspy conformance`.
//!
//! Two ways to feed it:
//!
//! * **a manifest suite** — `rust/conformance/mini/manifest.tsv`, where each row states the
//!   id, whether the document is well-formed, and (for the not-wf cases) a substring the
//!   reported diagnostic must contain. That is the suite vendored with this repository, and
//!   expecting a *message* rather than a verdict is what stops the diagnostics from
//!   regressing quietly;
//! * **a directory suite** — an unpacked W3C `xmlconf` tree, classified by the directory
//!   each document sits in (`not-wf/` must produce a diagnostic, `wf/`, `valid/` and
//!   `invalid/` must not — validity is a Phase 2 validator, not the well-formedness
//!   scanner). No manifest needed.
//!
//! Both run every document through the same path the CLI uses for `xmlspy wf`: mmap (or the
//! buffered fallback) through [`xmlspy_parallel::sequential`], one thread, deterministic.

use std::fs;
use std::path::{Path, PathBuf};

use xmlspy_core::ByteSource;
use xmlspy_parse::{ScannerConfig, StructuralIndex};

/// What a case expects of the scanner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// The document is well-formed: zero diagnostics.
    Wf,
    /// The document is not: at least one diagnostic, matching [`Case::expect`] when given.
    NotWf,
}

/// One conformance case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Case {
    /// Stable id (the manifest's first column, or the relative path).
    pub id: String,
    /// Expected verdict.
    pub status: Status,
    /// Document to scan.
    pub path: PathBuf,
    /// Substring one of the diagnostics must contain; empty means "any diagnostic".
    pub expect: String,
    /// The production or well-formedness constraint the case exercises (for the report).
    pub spec: String,
}

/// The outcome of running one case.
#[derive(Debug, Clone)]
pub struct CaseResult {
    /// The case.
    pub case: Case,
    /// Verdict.
    pub passed: bool,
    /// Diagnostics the scanner counted (not just the ones it retained).
    pub diagnostics: u64,
    /// The first retained diagnostic, rendered.
    pub first: Option<String>,
    /// Why it failed; empty when it passed.
    pub why: String,
    /// Scan time in seconds.
    pub secs: f64,
    /// Document size in bytes.
    pub bytes: u64,
}

/// The whole run.
#[derive(Debug, Clone, Default)]
pub struct Report {
    /// One entry per case, in manifest/discovery order.
    pub results: Vec<CaseResult>,
    /// Wall-clock seconds for the run.
    pub secs: f64,
    /// Where the cases came from.
    pub suite: PathBuf,
}

impl Report {
    /// Cases run.
    #[must_use]
    pub fn total(&self) -> usize {
        self.results.len()
    }

    /// Cases that passed.
    #[must_use]
    pub fn passed(&self) -> usize {
        self.results.iter().filter(|r| r.passed).count()
    }

    /// Cases that failed.
    #[must_use]
    pub fn failures(&self) -> Vec<&CaseResult> {
        self.results.iter().filter(|r| !r.passed).collect()
    }

    /// Pass rate in percent (0 when nothing ran).
    #[must_use]
    pub fn percent(&self) -> f64 {
        if self.results.is_empty() {
            0.0
        } else {
            self.passed() as f64 * 100.0 / self.results.len() as f64
        }
    }

    /// True at 100 %, which is the only passing score.
    #[must_use]
    pub fn all_passed(&self) -> bool {
        !self.results.is_empty() && self.passed() == self.results.len()
    }

    /// How many `wf` and how many `not-wf` cases ran.
    #[must_use]
    pub fn counts(&self) -> (usize, usize) {
        let wf = self
            .results
            .iter()
            .filter(|r| r.case.status == Status::Wf)
            .count();
        (wf, self.results.len() - wf)
    }
}

/// The scanner configuration the runner uses.
///
/// `max_errors` is small on purpose: a conformance case needs the first few diagnostics, not
/// 200 of them, and the cap keeps a pathological document from filling memory.
#[must_use]
pub fn runner_config(max_errors: u32) -> ScannerConfig {
    ScannerConfig {
        max_indexed: 0, // conformance is about diagnostics, not the tree
        stride: 32,
        max_errors: max_errors.max(1),
    }
}

/// Load a suite from `dir`: its manifest when it has one, otherwise a directory walk.
///
/// # Errors
/// When the directory cannot be read, a manifest row is malformed, or no case was found.
pub fn load_suite(dir: &Path) -> Result<Vec<Case>, String> {
    let manifest = dir.join("manifest.tsv");
    let cases = if manifest.is_file() {
        load_manifest(&manifest, dir)?
    } else {
        discover(dir)?
    };
    if cases.is_empty() {
        return Err(format!("no conformance cases under {}", dir.display()));
    }
    Ok(cases)
}

fn load_manifest(manifest: &Path, dir: &Path) -> Result<Vec<Case>, String> {
    let text = fs::read_to_string(manifest).map_err(|e| format!("{}: {e}", manifest.display()))?;
    let mut out = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let f: Vec<&str> = line.split('\t').collect();
        if f[0] == "id" {
            continue; // header
        }
        if f.len() < 4 {
            return Err(format!("{}:{}: expected 4 tab-separated columns", manifest.display(), n + 1));
        }
        let status = match f[1] {
            "wf" => Status::Wf,
            "not-wf" => Status::NotWf,
            other => {
                return Err(format!(
                    "{}:{}: status must be 'wf' or 'not-wf', got {other:?}",
                    manifest.display(),
                    n + 1
                ))
            }
        };
        let expect = if f[3] == "-" { String::new() } else { f[3].to_string() };
        out.push(Case {
            id: f[0].to_string(),
            status,
            path: dir.join(f[2]),
            expect,
            spec: f.get(4).copied().unwrap_or("").to_string(),
        });
    }
    Ok(out)
}

fn discover(dir: &Path) -> Result<Vec<Case>, String> {
    let mut out = Vec::new();
    walk(dir, dir, &mut out)?;
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<Case>) -> Result<(), String> {
    let rd = fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for e in rd {
        let e = e.map_err(|x| x.to_string())?;
        let p = e.path();
        if p.is_dir() {
            walk(root, &p, out)?;
            continue;
        }
        if p.extension().and_then(|x| x.to_str()) != Some("xml") {
            continue;
        }
        // The W3C suite states the expectation in the directory name.
        let comps: Vec<String> = p
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        let status = if comps.iter().any(|c| c == "not-wf") {
            Some(Status::NotWf)
        } else if comps.iter().any(|c| c == "wf" || c == "valid" || c == "invalid") {
            Some(Status::Wf)
        } else {
            None
        };
        let Some(status) = status else { continue };
        let rel = p
            .strip_prefix(root)
            .unwrap_or(&p)
            .to_string_lossy()
            .replace(std::path::MAIN_SEPARATOR, "/");
        out.push(Case {
            id: rel.clone(),
            status,
            path: p,
            expect: String::new(),
            spec: String::new(),
        });
    }
    Ok(())
}

/// Scan one document the way `xmlspy wf` does.
fn scan(path: &Path, cfg: ScannerConfig) -> Result<(StructuralIndex, u64, f64), String> {
    let mut src = xmlspy_io::open_byte_source(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let len = src.len();
    let t0 = std::time::Instant::now();
    let ix = xmlspy_parallel::sequential(&mut src, cfg).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok((ix, len, t0.elapsed().as_secs_f64()))
}

/// Run one case.
///
/// # Errors
/// Only when the document cannot be read at all — a wrong verdict is a failed case, not an
/// error, because that is the whole point of the run.
pub fn run_case(case: &Case, cfg: ScannerConfig) -> Result<CaseResult, String> {
    let (ix, bytes, secs) = scan(&case.path, cfg)?;
    let first = ix.errors.first().map(|e| format!("Ln {}, Col {}: {}", e.line, e.col, e.msg));
    let (passed, why) = match case.status {
        Status::Wf => {
            if ix.error_count == 0 {
                (true, String::new())
            } else {
                (
                    false,
                    format!(
                        "expected a well-formed document, got {} diagnostic(s): {}",
                        ix.error_count,
                        first.clone().unwrap_or_default()
                    ),
                )
            }
        }
        Status::NotWf => {
            if ix.error_count == 0 {
                (false, "expected a diagnostic, the scanner reported none".to_string())
            } else if !case.expect.is_empty()
                && !ix.errors.iter().any(|e| e.msg.contains(&case.expect))
            {
                (
                    false,
                    format!(
                        "{} diagnostic(s) but none contains {:?}: {}",
                        ix.error_count,
                        case.expect,
                        first.clone().unwrap_or_default()
                    ),
                )
            } else {
                (true, String::new())
            }
        }
    };
    Ok(CaseResult {
        case: case.clone(),
        passed,
        diagnostics: ix.error_count,
        first,
        why,
        secs,
        bytes,
    })
}

/// Run a whole suite.
///
/// # Errors
/// When the suite cannot be loaded. A failing case is reported, not returned as an error.
pub fn run(dir: &Path, cfg: ScannerConfig) -> Result<Report, String> {
    let cases = load_suite(dir)?;
    let t0 = std::time::Instant::now();
    let mut results = Vec::with_capacity(cases.len());
    for case in &cases {
        match run_case(case, cfg) {
            Ok(r) => results.push(r),
            Err(e) => results.push(CaseResult {
                case: case.clone(),
                passed: false,
                diagnostics: 0,
                first: None,
                why: format!("could not run the case: {e}"),
                secs: 0.0,
                bytes: 0,
            }),
        }
    }
    Ok(Report {
        results,
        secs: t0.elapsed().as_secs_f64(),
        suite: dir.to_path_buf(),
    })
}

/// Render a report the way the CLI prints it.
#[must_use]
pub fn render(rep: &Report, verbose: bool) -> String {
    let mut out = String::new();
    let (wf, nwf) = rep.counts();
    out.push_str(&format!(
        "conformance: {} cases ({wf} wf, {nwf} not-wf) from {}\n",
        rep.total(),
        rep.suite.display()
    ));
    for r in &rep.results {
        if r.passed && !verbose {
            continue;
        }
        let verdict = if r.passed { "pass" } else { "FAIL" };
        let name = r
            .case
            .path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| r.case.path.display().to_string());
        out.push_str(&format!(
            "  {verdict} {:<10} {name} — {} diagnostic(s)",
            r.case.id, r.diagnostics
        ));
        if !r.case.spec.is_empty() {
            out.push_str(&format!(" · {}", r.case.spec));
        }
        out.push('\n');
        if !r.passed {
            out.push_str(&format!("        {}\n", r.why));
        } else if verbose {
            if let Some(f) = &r.first {
                out.push_str(&format!("        {f}\n"));
            }
        }
    }
    out.push_str(&format!(
        "{}/{} passed ({:.1} %) in {:.3} s — {}\n",
        rep.passed(),
        rep.total(),
        rep.percent(),
        rep.secs,
        if rep.all_passed() {
            "PASS"
        } else {
            "FAIL"
        }
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn suite_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../conformance/mini")
    }

    #[test]
    fn the_vendored_suite_loads() {
        let cases = load_suite(&suite_dir()).expect("mini suite");
        assert!(cases.len() >= 40, "{} cases", cases.len());
        let (wf, nwf): (Vec<&Case>, Vec<&Case>) = cases
            .iter()
            .partition(|c| c.status == Status::Wf);
        assert!(!wf.is_empty() && !nwf.is_empty());
        for c in &cases {
            assert!(c.path.is_file(), "missing case file {}", c.path.display());
            assert!(!c.id.is_empty());
            if c.status == Status::NotWf {
                assert!(!c.expect.is_empty(), "{} expects a specific message", c.id);
            }
        }
    }

    #[test]
    fn discovery_classifies_by_directory() {
        let dir = suite_dir().join("cases");
        let cases = discover(&dir).expect("walk");
        assert!(cases.len() >= 40, "{} cases", cases.len());
        assert!(cases.iter().any(|c| c.status == Status::NotWf));
        assert!(cases.iter().any(|c| c.status == Status::Wf));
        // Every discovered case must also be in the manifest, with the same verdict.
        let manifest = load_suite(&suite_dir()).expect("manifest");
        for c in &cases {
            let name = c.path.file_name().unwrap().to_string_lossy().into_owned();
            let m = manifest
                .iter()
                .find(|m| m.path.file_name().unwrap().to_string_lossy() == name)
                .unwrap_or_else(|| panic!("{name} is not in the manifest"));
            assert_eq!(m.status, c.status, "{name}");
        }
    }

    #[test]
    fn a_case_that_reads_a_missing_file_fails_without_panicking() {
        let case = Case {
            id: "missing".to_string(),
            status: Status::NotWf,
            path: PathBuf::from("/definitely/not/here.xml"),
            expect: "x".to_string(),
            spec: String::new(),
        };
        assert!(run_case(&case, runner_config(4)).is_err());
    }
}
