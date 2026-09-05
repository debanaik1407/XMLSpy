//! Single-pass XML scanning, indexing and searching for multi-GB documents.
//!
//! * [`scanner::Scanner`] — resumable well-formedness scanner + structural indexer.
//! * [`scanner::BoundaryState`] — the resumable state at a byte boundary, captured with
//!   [`scanner::Scanner::boundary`] and restored with [`scanner::Scanner::resume`]. This is
//!   what lets `xmlspy-parallel` cut a document into segments and scan them on separate
//!   threads without the cut changing a single byte of the resulting index.
//! * [`search::Finder`] — chunk-resumable literal search with line/column tracking.
//! * [`classify`] — SWAR byte classification used by the hot loop.
//!
//! The crate is `no_std` (+`alloc`) and has no dependencies outside the workspace, so the
//! same code runs in the browser (`wasm32-unknown-unknown`), in the CLI and in the server.

#![no_std]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

extern crate alloc;

#[cfg(test)]
extern crate std;

pub mod classify;
pub mod scanner;
pub mod search;

pub use scanner::{
    BoundaryState, Progress, Scanner, ScannerConfig, St, END_PENDING, MAX_DEPTH0_CLOSES,
};
pub use search::{Finder, Hit};
pub use xmlspy_core::{Severity, WfError};
pub use xmlspy_index::{StructuralIndex, END_UNKNOWN, NO_PARENT};
