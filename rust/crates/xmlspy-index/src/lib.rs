//! The structural index: what the engine keeps in memory (or on disk) instead of a DOM.
//!
//! For every element the index stores its byte range, line, parent, interned name and
//! depth; on top of that a *sparse* line-checkpoint table (every `stride`-th line start)
//! makes "give me lines N..M" an O(1) seek plus one block decode, which is what allows a
//! 10 GiB document to be scrolled without ever loading it.
//!
//! [`xsi`] serialises the whole structure into a flat, little-endian, mmap-able buffer
//! (`.xsi`) with an explicit section table, so the browser can build zero-copy typed-array
//! views over WASM memory and the native engine can `mmap` a cached index straight from disk.

#![no_std]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

extern crate alloc;

#[cfg(test)]
extern crate std;

pub mod xsi;

use alloc::string::String;
use alloc::vec::Vec;
use xmlspy_core::WfError;

/// Sentinel stored in `elem_end` while an element's end offset is still unknown.
pub const END_UNKNOWN: u64 = u64::MAX;

/// Sentinel stored in `elem_parent` for root elements.
pub const NO_PARENT: i32 = -1;

/// A sparse structural index over one XML document.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StructuralIndex {
    /// Byte length of the region this index covers.
    pub file_len: u64,
    /// Every `stride`-th line start offset (`checkpoints[k]` = start of line `k * stride`).
    pub checkpoints: Vec<u64>,
    /// Checkpoint stride in lines.
    pub stride: u32,
    /// Number of lines seen (1-based count).
    pub line_count: u64,

    /// Byte offset of each indexed element's `<`.
    pub elem_start: Vec<u64>,
    /// Byte offset just past each indexed element's end tag ([`END_UNKNOWN`] if unclosed).
    pub elem_end: Vec<u64>,
    /// 1-based line of each indexed element.
    pub elem_line: Vec<u64>,
    /// Index of the parent element, or [`NO_PARENT`].
    pub elem_parent: Vec<i32>,
    /// Index into [`StructuralIndex::names`].
    pub elem_name: Vec<u32>,
    /// Nesting depth (0 = root).
    pub elem_depth: Vec<u16>,

    /// Interned element names.
    pub names: Vec<String>,

    /// Elements actually stored (may be lower than `total_elements` because of the budget).
    pub indexed_elements: u32,
    /// Elements seen by the scanner.
    pub total_elements: u64,
    /// Attributes seen by the scanner.
    pub total_attributes: u64,
    /// Deepest nesting level seen.
    pub max_depth: u32,

    /// Diagnostics retained (capped by the scanner's `max_errors`).
    pub errors: Vec<WfError>,
    /// Diagnostics *seen* (may exceed `errors.len()`).
    pub error_count: u64,
}

impl StructuralIndex {
    /// Name of element `id`, if it is in range.
    pub fn name_of(&self, id: usize) -> Option<&str> {
        let n = *self.elem_name.get(id)? as usize;
        self.names.get(n).map(String::as_str)
    }

    /// Children of `id` (linear scan bounded by the index budget, mirrors the browser model).
    pub fn children_of(&self, id: usize) -> Vec<u32> {
        let mut out = Vec::new();
        for (i, p) in self.elem_parent.iter().enumerate().skip(id + 1) {
            if *p == id as i32 {
                out.push(i as u32);
            }
        }
        out
    }

    /// Start offset of line `line` (0-based) if a checkpoint covers it exactly,
    /// otherwise the nearest preceding checkpoint and how many lines to skip.
    pub fn seek_line(&self, line: u64) -> Option<(u64, u64)> {
        if self.stride == 0 {
            return None;
        }
        let block = line / u64::from(self.stride);
        let off = *self.checkpoints.get(block as usize)?;
        Some((off, line - block * u64::from(self.stride)))
    }

    /// Rough resident size of the index in bytes (what the status bar reports).
    pub fn heap_bytes(&self) -> usize {
        self.checkpoints.len() * 8
            + self.elem_start.len() * 8
            + self.elem_end.len() * 8
            + self.elem_line.len() * 8
            + self.elem_parent.len() * 4
            + self.elem_name.len() * 4
            + self.elem_depth.len() * 2
            + self.names.iter().map(|n| n.len() + 16).sum::<usize>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    fn sample() -> StructuralIndex {
        StructuralIndex {
            file_len: 100,
            checkpoints: alloc::vec![0, 40, 80],
            stride: 2,
            line_count: 6,
            elem_start: alloc::vec![0, 10, 20],
            elem_end: alloc::vec![100, 18, 30],
            elem_line: alloc::vec![1, 2, 3],
            elem_parent: alloc::vec![NO_PARENT, 0, 0],
            elem_name: alloc::vec![0, 1, 1],
            elem_depth: alloc::vec![0, 1, 1],
            names: alloc::vec!["root".to_string(), "child".to_string()],
            indexed_elements: 3,
            total_elements: 3,
            total_attributes: 1,
            max_depth: 2,
            errors: Vec::new(),
            error_count: 0,
        }
    }

    #[test]
    fn navigation_helpers() {
        let ix = sample();
        assert_eq!(ix.name_of(2), Some("child"));
        assert_eq!(ix.children_of(0), alloc::vec![1, 2]);
        assert_eq!(ix.seek_line(5), Some((80, 1)));
        assert!(ix.heap_bytes() > 0);
    }
}
