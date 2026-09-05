//! `.xsi` — the flat, little-endian, mmap-able serialisation of a [`StructuralIndex`].
//!
//! ```text
//! offset  size  field
//! 0       4     magic "XSI1"
//! 4       2     u16  version (1)
//! 6       2     u16  flags (bit0 = index is partial/resumable)
//! 8       4     u32  stride (lines per checkpoint)
//! 12      4     u32  indexed_elements
//! 16      8     u64  file_len (bytes covered by this index)
//! 24      8     u64  line_count
//! 32      8     u64  total_elements
//! 40      8     u64  total_attributes
//! 48      4     u32  max_depth
//! 52      4     u32  checkpoint_count
//! 56      4     u32  name_count
//! 60      4     u32  error_count (seen; the errors section may hold fewer)
//! 64      4     u32  off_checkpoints   ─┐  all offsets are absolute, 8-byte aligned
//! 68      4     u32  off_elem_start     │
//! 72      4     u32  off_elem_end       │
//! 76      4     u32  off_elem_line      │  section table
//! 80      4     u32  off_elem_parent    │
//! 84      4     u32  off_elem_name      │
//! 88      4     u32  off_elem_depth     │
//! 92      4     u32  off_names          │
//! 96      4     u32  off_errors        ─┘
//! 100     4     u32  total_len
//! 104     8     ---  padding to 112 (keeps every u64 section 8-byte aligned)
//! ```
//!
//! Sections are plain arrays (`u64`/`i32`/`u32`/`u16`), so a consumer can build
//! `Float64Array`/`Int32Array`/`Uint16Array` views directly over the buffer.
//! `names` is `u32 len + UTF-8` repeated; `errors` is `u32 count` followed by
//! `u64 offset, u64 line, u32 col, u8 severity, u8 has_fix, u32 msg_len, msg, u32 fix_len, fix`.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;
use xmlspy_core::{Severity, WfError};

use crate::StructuralIndex;

/// Magic bytes at the start of every `.xsi` buffer.
pub const MAGIC: [u8; 4] = *b"XSI1";
/// Format version understood by this build.
pub const VERSION: u16 = 1;
/// Size of the fixed header, including padding.
pub const HEADER_LEN: usize = 112;
/// Flag: the index only covers a prefix of the document (cancelled / progressive open).
pub const FLAG_PARTIAL: u16 = 1;

/// Errors returned by [`decode`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum XsiError {
    /// Buffer does not start with [`MAGIC`].
    BadMagic,
    /// Version is newer than this build understands.
    UnsupportedVersion(u16),
    /// A section offset or length points outside the buffer.
    Truncated {
        /// Section that failed to decode.
        section: &'static str,
    },
    /// A name or message was not valid UTF-8.
    BadUtf8 {
        /// Section that failed to decode.
        section: &'static str,
    },
}

impl fmt::Display for XsiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            XsiError::BadMagic => f.write_str(".xsi: bad magic"),
            XsiError::UnsupportedVersion(v) => write!(f, ".xsi: unsupported version {v}"),
            XsiError::Truncated { section } => write!(f, ".xsi: truncated section '{section}'"),
            XsiError::BadUtf8 { section } => write!(f, ".xsi: invalid UTF-8 in '{section}'"),
        }
    }
}

const fn align8(n: usize) -> usize {
    (n + 7) & !7
}

struct Writer {
    buf: Vec<u8>,
}

impl Writer {
    fn new(cap: usize) -> Self {
        let mut buf = Vec::with_capacity(cap);
        buf.resize(HEADER_LEN, 0);
        Self { buf }
    }
    fn pad(&mut self) -> u32 {
        let want = align8(self.buf.len());
        self.buf.resize(want, 0);
        want as u32
    }
    fn u16(&mut self, v: u16) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    fn i32(&mut self, v: i32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    fn bytes(&mut self, v: &[u8]) {
        self.buf.extend_from_slice(v);
    }
    fn put_u16(&mut self, at: usize, v: u16) {
        self.buf[at..at + 2].copy_from_slice(&v.to_le_bytes());
    }
    fn put_u32(&mut self, at: usize, v: u32) {
        self.buf[at..at + 4].copy_from_slice(&v.to_le_bytes());
    }
    fn put_u64(&mut self, at: usize, v: u64) {
        self.buf[at..at + 8].copy_from_slice(&v.to_le_bytes());
    }
}

/// Serialise an index into a `.xsi` buffer.
pub fn encode(ix: &StructuralIndex, partial: bool) -> Vec<u8> {
    let n = ix.indexed_elements as usize;
    let est =
        HEADER_LEN + ix.checkpoints.len() * 8 + n * 34 + ix.names.len() * 24 + ix.errors.len() * 96;
    let mut w = Writer::new(est);

    let off_checkpoints = w.pad();
    for c in &ix.checkpoints {
        w.u64(*c);
    }
    let off_elem_start = w.pad();
    for v in ix.elem_start.iter().take(n) {
        w.u64(*v);
    }
    let off_elem_end = w.pad();
    for v in ix.elem_end.iter().take(n) {
        w.u64(*v);
    }
    let off_elem_line = w.pad();
    for v in ix.elem_line.iter().take(n) {
        w.u64(*v);
    }
    let off_elem_parent = w.pad();
    for v in ix.elem_parent.iter().take(n) {
        w.i32(*v);
    }
    let off_elem_name = w.pad();
    for v in ix.elem_name.iter().take(n) {
        w.u32(*v);
    }
    let off_elem_depth = w.pad();
    for v in ix.elem_depth.iter().take(n) {
        w.u16(*v);
    }
    let off_names = w.pad();
    for name in &ix.names {
        w.u32(name.len() as u32);
        w.bytes(name.as_bytes());
    }
    let off_errors = w.pad();
    w.u32(ix.errors.len() as u32);
    for e in &ix.errors {
        w.u64(e.offset);
        w.u64(e.line);
        w.u32(e.col as u32);
        w.bytes(&[e.severity.as_u8(), u8::from(e.fix.is_some())]);
        w.u32(e.msg.len() as u32);
        w.bytes(e.msg.as_bytes());
        match &e.fix {
            Some(f) => {
                w.u32(f.len() as u32);
                w.bytes(f.as_bytes());
            }
            None => w.u32(0),
        }
    }
    let total_len = w.buf.len() as u32;

    w.buf[0..4].copy_from_slice(&MAGIC);
    w.put_u16(4, VERSION);
    w.put_u16(6, if partial { FLAG_PARTIAL } else { 0 });
    w.put_u32(8, ix.stride);
    w.put_u32(12, ix.indexed_elements);
    w.put_u64(16, ix.file_len);
    w.put_u64(24, ix.line_count);
    w.put_u64(32, ix.total_elements);
    w.put_u64(40, ix.total_attributes);
    w.put_u32(48, ix.max_depth);
    w.put_u32(52, ix.checkpoints.len() as u32);
    w.put_u32(56, ix.names.len() as u32);
    w.put_u32(60, ix.error_count as u32);
    w.put_u32(64, off_checkpoints);
    w.put_u32(68, off_elem_start);
    w.put_u32(72, off_elem_end);
    w.put_u32(76, off_elem_line);
    w.put_u32(80, off_elem_parent);
    w.put_u32(84, off_elem_name);
    w.put_u32(88, off_elem_depth);
    w.put_u32(92, off_names);
    w.put_u32(96, off_errors);
    w.put_u32(100, total_len);
    w.buf
}

struct Reader<'a> {
    b: &'a [u8],
    p: usize,
}

impl<'a> Reader<'a> {
    fn u16(&mut self, s: &'static str) -> Result<u16, XsiError> {
        let e = self.p + 2;
        let v = self
            .b
            .get(self.p..e)
            .ok_or(XsiError::Truncated { section: s })?;
        self.p = e;
        Ok(u16::from_le_bytes([v[0], v[1]]))
    }
    fn u32(&mut self, s: &'static str) -> Result<u32, XsiError> {
        let e = self.p + 4;
        let v = self
            .b
            .get(self.p..e)
            .ok_or(XsiError::Truncated { section: s })?;
        self.p = e;
        Ok(u32::from_le_bytes([v[0], v[1], v[2], v[3]]))
    }
    fn i32(&mut self, s: &'static str) -> Result<i32, XsiError> {
        Ok(self.u32(s)? as i32)
    }
    fn u64(&mut self, s: &'static str) -> Result<u64, XsiError> {
        let e = self.p + 8;
        let v = self
            .b
            .get(self.p..e)
            .ok_or(XsiError::Truncated { section: s })?;
        self.p = e;
        let mut a = [0u8; 8];
        a.copy_from_slice(v);
        Ok(u64::from_le_bytes(a))
    }
    fn u8(&mut self, s: &'static str) -> Result<u8, XsiError> {
        let v = *self
            .b
            .get(self.p)
            .ok_or(XsiError::Truncated { section: s })?;
        self.p += 1;
        Ok(v)
    }
    fn string(&mut self, len: usize, s: &'static str) -> Result<String, XsiError> {
        let e = self.p + len;
        let v = self
            .b
            .get(self.p..e)
            .ok_or(XsiError::Truncated { section: s })?;
        self.p = e;
        core::str::from_utf8(v)
            .map(String::from)
            .map_err(|_| XsiError::BadUtf8 { section: s })
    }
}

/// Parse a `.xsi` buffer back into a [`StructuralIndex`].
pub fn decode(buf: &[u8]) -> Result<StructuralIndex, XsiError> {
    if buf.len() < HEADER_LEN {
        return Err(XsiError::Truncated { section: "header" });
    }
    if buf[0..4] != MAGIC {
        return Err(XsiError::BadMagic);
    }
    let mut h = Reader { b: buf, p: 4 };
    let version = h.u16("header")?;
    if version != VERSION {
        return Err(XsiError::UnsupportedVersion(version));
    }
    let _flags = h.u16("header")?;
    let stride = h.u32("header")?;
    let indexed = h.u32("header")? as usize;
    let file_len = h.u64("header")?;
    let line_count = h.u64("header")?;
    let total_elements = h.u64("header")?;
    let total_attributes = h.u64("header")?;
    let max_depth = h.u32("header")?;
    let checkpoint_count = h.u32("header")? as usize;
    let name_count = h.u32("header")? as usize;
    let error_count = u64::from(h.u32("header")?);
    let off_checkpoints = h.u32("header")? as usize;
    let off_elem_start = h.u32("header")? as usize;
    let off_elem_end = h.u32("header")? as usize;
    let off_elem_line = h.u32("header")? as usize;
    let off_elem_parent = h.u32("header")? as usize;
    let off_elem_name = h.u32("header")? as usize;
    let off_elem_depth = h.u32("header")? as usize;
    let off_names = h.u32("header")? as usize;
    let off_errors = h.u32("header")? as usize;

    let mut ix = StructuralIndex {
        file_len,
        stride,
        line_count,
        indexed_elements: indexed as u32,
        total_elements,
        total_attributes,
        max_depth,
        error_count,
        ..Default::default()
    };

    let mut r = Reader {
        b: buf,
        p: off_checkpoints,
    };
    ix.checkpoints.reserve(checkpoint_count);
    for _ in 0..checkpoint_count {
        ix.checkpoints.push(r.u64("checkpoints")?);
    }
    let mut r = Reader {
        b: buf,
        p: off_elem_start,
    };
    for _ in 0..indexed {
        ix.elem_start.push(r.u64("elem_start")?);
    }
    let mut r = Reader {
        b: buf,
        p: off_elem_end,
    };
    for _ in 0..indexed {
        ix.elem_end.push(r.u64("elem_end")?);
    }
    let mut r = Reader {
        b: buf,
        p: off_elem_line,
    };
    for _ in 0..indexed {
        ix.elem_line.push(r.u64("elem_line")?);
    }
    let mut r = Reader {
        b: buf,
        p: off_elem_parent,
    };
    for _ in 0..indexed {
        ix.elem_parent.push(r.i32("elem_parent")?);
    }
    let mut r = Reader {
        b: buf,
        p: off_elem_name,
    };
    for _ in 0..indexed {
        ix.elem_name.push(r.u32("elem_name")?);
    }
    let mut r = Reader {
        b: buf,
        p: off_elem_depth,
    };
    for _ in 0..indexed {
        ix.elem_depth.push(r.u16("elem_depth")?);
    }
    let mut r = Reader {
        b: buf,
        p: off_names,
    };
    for _ in 0..name_count {
        let len = r.u32("names")? as usize;
        ix.names.push(r.string(len, "names")?);
    }
    let mut r = Reader {
        b: buf,
        p: off_errors,
    };
    let errs = r.u32("errors")? as usize;
    for _ in 0..errs {
        let offset = r.u64("errors")?;
        let line = r.u64("errors")?;
        let col = u64::from(r.u32("errors")?);
        let severity = Severity::from_u8(r.u8("errors")?);
        let has_fix = r.u8("errors")? != 0;
        let msg_len = r.u32("errors")? as usize;
        let msg = r.string(msg_len, "errors")?;
        let fix_len = r.u32("errors")? as usize;
        let fix = r.string(fix_len, "errors")?;
        ix.errors.push(WfError {
            offset,
            line,
            col,
            msg,
            severity,
            fix: if has_fix { Some(fix) } else { None },
        });
    }
    Ok(ix)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NO_PARENT;
    use alloc::string::ToString;

    fn sample() -> StructuralIndex {
        StructuralIndex {
            file_len: 4096,
            checkpoints: alloc::vec![0, 128, 256],
            stride: 32,
            line_count: 90,
            elem_start: alloc::vec![0, 12, 40],
            elem_end: alloc::vec![4096, 30, u64::MAX],
            elem_line: alloc::vec![1, 2, 4],
            elem_parent: alloc::vec![NO_PARENT, 0, 0],
            elem_name: alloc::vec![0, 1, 2],
            elem_depth: alloc::vec![0, 1, 1],
            names: alloc::vec![
                "Orders".to_string(),
                "Order".to_string(),
                "Ünïcødé".to_string()
            ],
            indexed_elements: 3,
            total_elements: 3,
            total_attributes: 7,
            max_depth: 2,
            errors: alloc::vec![
                WfError::error(11, 2, 3, "boom §3.1").with_fix("Change to </Order>"),
                WfError::error(99, 5, 1, "warn").as_warning(),
            ],
            error_count: 2,
        }
    }

    #[test]
    fn round_trip_is_lossless() {
        let ix = sample();
        let buf = encode(&ix, false);
        assert_eq!(&buf[0..4], b"XSI1");
        let back = decode(&buf).unwrap();
        assert_eq!(ix, back);
    }

    #[test]
    fn sections_are_eight_byte_aligned() {
        let buf = encode(&sample(), false);
        for at in [64usize, 68, 72, 76, 80, 84, 88, 92, 96] {
            let off = u32::from_le_bytes([buf[at], buf[at + 1], buf[at + 2], buf[at + 3]]) as usize;
            assert_eq!(off % 8, 0, "section at header+{at} not aligned");
            assert!(off <= buf.len());
        }
        let total = u32::from_le_bytes([buf[100], buf[101], buf[102], buf[103]]) as usize;
        assert_eq!(total, buf.len());
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(
            decode(b"nope").unwrap_err(),
            XsiError::Truncated { section: "header" }
        );
        let mut buf = encode(&sample(), true);
        buf[0] = b'Z';
        assert_eq!(decode(&buf).unwrap_err(), XsiError::BadMagic);
        let mut buf = encode(&sample(), false);
        buf[4] = 9;
        assert_eq!(decode(&buf).unwrap_err(), XsiError::UnsupportedVersion(9));
    }

    #[test]
    fn partial_flag_round_trips() {
        let buf = encode(&sample(), true);
        assert_eq!(u16::from_le_bytes([buf[6], buf[7]]), FLAG_PARTIAL);
    }
}
