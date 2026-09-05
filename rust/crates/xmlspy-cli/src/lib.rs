//! The library half of the `xmlspy` command-line engine.
//!
//! `xmlspy` is a thin argument parser over the engine crates; everything with logic worth
//! testing lives here so that `cargo test -p xmlspy-cli` covers it without spawning a
//! process:
//!
//! * [`conformance`] — the well-formedness suite runner (`xmlspy conformance`), which reads
//!   either the vendored mini suite in `rust/conformance/mini` or an unpacked W3C
//!   `xmlconf` tree and reports a pass rate that only counts at 100 %.
//!
//! The binary target (`src/main.rs`) adds argument handling, the status lines and the exit
//! codes; see its module docs for the command reference.

#![warn(missing_docs)]

pub mod conformance;
