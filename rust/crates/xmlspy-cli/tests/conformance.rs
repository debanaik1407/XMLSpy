//! The vendored mini conformance suite as a test gate.
//!
//! `cargo test -p xmlspy-cli` runs the same documents `xmlspy conformance` reports on, so a
//! diagnostic that regresses fails the build instead of quietly changing the report. The
//! suite itself lives in `rust/conformance/mini` and is documented there; the real W3C
//! `xmlconf` tree is not vendored (see that README) and is run with `--suite DIR`.

use std::fs;
use std::path::PathBuf;

use xmlspy_cli::conformance::{self, runner_config, Case, Status};

fn suite() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../conformance/mini")
}

#[test]
fn the_mini_suite_passes_completely() {
    let dir = suite();
    assert!(dir.is_dir(), "{} must exist", dir.display());
    let rep = conformance::run(&dir, runner_config(16)).expect("run the suite");
    let rendered = conformance::render(&rep, true);
    assert!(rep.total() >= 40, "expected the vendored cases, got {}", rep.total());
    assert!(rep.all_passed(), "conformance below 100 %:\n{rendered}");
    assert_eq!(rep.percent(), 100.0);
    let (wf, nwf) = rep.counts();
    assert!(wf >= 10 && nwf >= 25, "{wf} wf / {nwf} not-wf cases");
}

#[test]
fn a_failing_case_says_why() {
    let dir = suite();
    let cases = conformance::load_suite(&dir).expect("load");
    // Ask a well-formed document to be not-wf: the runner must fail it with a reason.
    let mut wrong = cases
        .iter()
        .find(|c| c.status == Status::Wf)
        .expect("a wf case")
        .clone();
    wrong.status = Status::NotWf;
    wrong.expect = "this message does not exist".to_string();
    let r = conformance::run_case(&wrong, runner_config(16)).expect("run");
    assert!(!r.passed);
    assert!(!r.why.is_empty(), "a failure must explain itself");
    assert_eq!(r.diagnostics, 0);

    // And the other way round: a not-wf document claimed to be well-formed.
    let mut wrong2 = cases
        .iter()
        .find(|c| c.status == Status::NotWf)
        .expect("a not-wf case")
        .clone();
    wrong2.status = Status::Wf;
    let r2 = conformance::run_case(&wrong2, runner_config(16)).expect("run");
    assert!(!r2.passed);
    assert!(r2.diagnostics > 0);
    assert!(r2.why.contains("well-formed"), "{}", r2.why);
}

#[test]
fn the_runner_scans_a_file_it_was_given() {
    let mut dir = std::env::temp_dir();
    dir.push(format!("xmlspy-conf-{}", std::process::id()));
    fs::create_dir_all(&dir).expect("temp dir");
    let good = dir.join("good.xml");
    let bad = dir.join("bad.xml");
    fs::write(&good, b"<a><b/></a>").expect("write");
    // Truncated at a depth-1 open element: the scanner's end-of-file diagnostic is the one
    // `expect` below pins, so this case cannot pass by reporting some *other* error.
    fs::write(&bad, b"<a><b>").expect("write");

    let cfg = runner_config(8);
    let ok = conformance::run_case(
        &Case {
            id: "good".to_string(),
            status: Status::Wf,
            path: good.clone(),
            expect: String::new(),
            spec: "[1] document".to_string(),
        },
        cfg,
    )
    .expect("run");
    assert!(ok.passed, "{}", ok.why);
    assert_eq!(ok.bytes, 11, "<a><b/></a>");

    let bad_case = conformance::run_case(
        &Case {
            id: "bad".to_string(),
            status: Status::NotWf,
            path: bad.clone(),
            expect: "element(s) not closed".to_string(),
            spec: "WFC: Element Type Match".to_string(),
        },
        cfg,
    )
    .expect("run");
    assert!(bad_case.passed, "{}", bad_case.why);
    assert!(bad_case.diagnostics > 0);
    assert!(bad_case.first.is_some(), "the first diagnostic is reported");

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_manifest_is_required_to_name_expectations() {
    // Directory discovery (the W3C-suite mode) classifies by path and expects any
    // diagnostic; the manifest is what pins the message.
    let cases = conformance::load_suite(&suite().join("cases")).expect("discover");
    assert!(cases.iter().all(|c| c.expect.is_empty()));
    assert!(cases.iter().any(|c| c.status == Status::NotWf));
    assert!(cases.iter().any(|c| c.status == Status::Wf));

    let manifest = conformance::load_suite(&suite()).expect("manifest");
    assert!(manifest
        .iter()
        .filter(|c| c.status == Status::NotWf)
        .all(|c| !c.expect.is_empty()));
    assert_eq!(manifest.len(), cases.len(), "same cases, two ways in");
}
