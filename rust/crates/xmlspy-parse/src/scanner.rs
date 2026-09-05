//! The resumable well-formedness scanner and structural indexer.
//!
//! One pass, no DOM, `O(index)` memory. [`Scanner::feed`] may be called with the
//! document split at **any** byte boundary — all parser state (current state, element
//! stack, line/column, entity buffer) lives in the struct, so an 8 MiB-chunked browser
//! worker, a memory-mapped native reader and a single `feed` of the whole file all
//! produce a bit-identical [`StructuralIndex`]. That property is enforced by
//! `tests/resumable.rs`, which replays every corpus at every chunk size.

use alloc::string::String;
use alloc::vec::Vec;
use xmlspy_core::WfError;
use xmlspy_index::{StructuralIndex, END_UNKNOWN, NO_PARENT};

use crate::classify::{
    find_any4, find_name_end, find_text_delim, is_name_char, is_name_start, is_ws,
};

/// Element end offset that is known to exist but has not been seen yet
/// (the matching `</name` was read, the closing `>` was not).
pub const END_PENDING: u64 = u64::MAX - 1;

/// Tuning knobs for one scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScannerConfig {
    /// Hard cap on fully indexed elements (memory budget). Depth ≤ 1 is always indexed.
    pub max_indexed: u32,
    /// Line-checkpoint stride: every `stride`-th line start is remembered.
    pub stride: u32,
    /// Maximum diagnostics retained (they are always *counted*).
    pub max_errors: u32,
}

impl Default for ScannerConfig {
    fn default() -> Self {
        Self {
            max_indexed: 300_000,
            stride: 32,
            max_errors: 200,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum St {
    Bom,
    Bom1,
    Bom2,
    Text,
    Ref,
    Lt,
    StartName,
    InTag,
    AttrName,
    AttrEq,
    AttrPreq,
    AttrVal,
    Empty,
    EndName,
    EndTrail,
    Pi,
    PiQ,
    Bang,
    Comment0,
    Comment,
    CommentD1,
    CommentD2,
    Cdata0,
    Cdata,
    CdataB1,
    CdataB2,
    Doctype,
}

/// Live counters for progress reporting while a scan is running.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Progress {
    /// Current 1-based line.
    pub line: u64,
    /// Elements seen so far.
    pub elements: u64,
    /// Diagnostics seen so far.
    pub errors: u64,
}

/// Single-pass, chunk-resumable XML scanner.
pub struct Scanner {
    cfg: ScannerConfig,
    state: St,
    ref_return: St,
    ref_buf: String,
    ref_start: u64,
    quote: u8,

    line: u64,
    line_start: u64,

    depth: usize,
    stack_name: Vec<u32>,
    stack_idx: Vec<i32>,

    names: Interner,
    attrs: Interner,
    name_buf: Vec<u8>,

    attr_names: Vec<u32>,
    attr_bytes_start: u64,
    tag_start_off: u64,

    cdata_match: usize,
    text_bracket: u32,
    doctype_bracket: i32,

    root_seen: bool,
    root_closed: bool,
    saw_non_ws_outside_root: bool,

    ix: StructuralIndex,
    elem_count: usize,
    seen_bytes: u64,
    finished: bool,
}

impl Scanner {
    /// Create a scanner. `checkpoints[0]` is pre-seeded with offset 0 (start of line 0).
    pub fn new(cfg: ScannerConfig) -> Self {
        let mut ix = StructuralIndex {
            stride: cfg.stride.max(1),
            line_count: 1,
            ..Default::default()
        };
        ix.checkpoints.push(0);
        Self {
            cfg: ScannerConfig {
                stride: cfg.stride.max(1),
                ..cfg
            },
            state: St::Bom,
            ref_return: St::Text,
            ref_buf: String::new(),
            ref_start: 0,
            quote: 0,
            line: 1,
            line_start: 0,
            depth: 0,
            stack_name: Vec::new(),
            stack_idx: Vec::new(),
            names: Interner::new(),
            attrs: Interner::new(),
            name_buf: Vec::with_capacity(64),
            attr_names: Vec::new(),
            attr_bytes_start: 0,
            tag_start_off: 0,
            cdata_match: 0,
            text_bracket: 0,
            doctype_bracket: 0,
            root_seen: false,
            root_closed: false,
            saw_non_ws_outside_root: false,
            ix,
            elem_count: 0,
            seen_bytes: 0,
            finished: false,
        }
    }

    /// Live counters (safe to read between `feed` calls, e.g. for a progress bar).
    pub fn progress(&self) -> Progress {
        Progress {
            line: self.line,
            elements: self.ix.total_elements,
            errors: self.ix.error_count,
        }
    }

    /// Bytes consumed so far.
    pub fn seen_bytes(&self) -> u64 {
        self.seen_bytes
    }

    fn push_error(&mut self, off: u64, msg: String, fix: Option<String>) {
        self.ix.error_count += 1;
        if (self.ix.errors.len() as u32) < self.cfg.max_errors {
            let col = off.saturating_sub(self.line_start) + 1;
            let mut e = WfError::error(off, self.line, col, msg);
            e.fix = fix;
            self.ix.errors.push(e);
        }
    }

    /// Intern the current name buffer as an **element** name (goes into the index).
    fn intern_name_buf(&mut self) -> u32 {
        let buf = core::mem::take(&mut self.name_buf);
        let id = self.names.intern(&buf);
        self.name_buf = buf;
        self.name_buf.clear();
        id
    }

    /// Intern the current name buffer as an **attribute** name. Attribute names live in
    /// a separate table so they never pollute `StructuralIndex::names`, which callers
    /// index by element-name id (and search by name, e.g. the XPath evaluator).
    fn intern_attr_buf(&mut self) -> u32 {
        let buf = core::mem::take(&mut self.name_buf);
        let id = self.attrs.intern(&buf);
        self.name_buf = buf;
        self.name_buf.clear();
        id
    }

    fn open_element(&mut self) {
        let id = self.intern_name_buf();
        self.ix.total_elements += 1;
        if self.root_closed && self.depth == 0 {
            let off = self.tag_start_off;
            let name = String::from(self.names.get(id));
            self.push_error(
                off,
                alloc::format!(
                    "Document contains more than one root element (<{name}>). XML 1.0 §2.1 [1] document ::= prolog element Misc*"
                ),
                Some(String::from("Wrap both elements in a single root element")),
            );
        }
        self.root_seen = true;

        let mut idx: i32 = -1;
        let budget = self.elem_count < self.cfg.max_indexed as usize || self.depth <= 1;
        if budget && self.elem_count < self.cfg.max_indexed as usize * 2 {
            idx = self.elem_count as i32;
            self.elem_count += 1;
            let parent = if self.depth > 0 {
                self.stack_idx[self.depth - 1]
            } else {
                NO_PARENT
            };
            self.ix.elem_start.push(self.tag_start_off);
            self.ix.elem_end.push(END_UNKNOWN);
            self.ix.elem_line.push(self.line);
            self.ix.elem_parent.push(parent);
            self.ix.elem_name.push(id);
            self.ix
                .elem_depth
                .push(self.depth.min(u16::MAX as usize) as u16);
        }
        if self.stack_name.len() == self.depth {
            self.stack_name.push(id);
            self.stack_idx.push(idx);
        } else {
            self.stack_name[self.depth] = id;
            self.stack_idx[self.depth] = idx;
        }
        self.depth += 1;
        if self.depth as u32 > self.ix.max_depth {
            self.ix.max_depth = self.depth as u32;
        }
        self.attr_names.clear();
    }

    fn close_element(&mut self, end_off: u64) {
        if self.depth == 0 {
            return;
        }
        self.depth -= 1;
        let idx = self.stack_idx[self.depth];
        if idx >= 0 {
            self.ix.elem_end[idx as usize] = end_off;
        }
        if self.depth == 0 {
            self.root_closed = true;
        }
    }

    fn patch_pending_end(&mut self, end_off: u64) {
        if let Some(&idx) = self.stack_idx.get(self.depth) {
            if idx >= 0 && self.ix.elem_end[idx as usize] == END_PENDING {
                self.ix.elem_end[idx as usize] = end_off;
            }
        }
    }

    fn validate_ref(&mut self) {
        let r = core::mem::take(&mut self.ref_buf);
        let ok = if let Some(rest) = r.strip_prefix('#') {
            if let Some(hex) = rest.strip_prefix('x') {
                !hex.is_empty() && hex.bytes().all(|b| b.is_ascii_hexdigit())
            } else {
                !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit())
            }
        } else {
            let mut it = r.bytes();
            match it.next() {
                Some(f) if f.is_ascii_alphabetic() || f == b'_' || f == b':' => it.all(|b| {
                    b.is_ascii_alphanumeric() || b == b'_' || b == b':' || b == b'.' || b == b'-'
                }),
                _ => false,
            }
        };
        if !ok {
            let off = self.ref_start;
            self.push_error(
                off,
                alloc::format!(
                    "Malformed entity/character reference '&{r};'. XML 1.0 §4.1 [67] Reference"
                ),
                Some(String::from("Escape the ampersand as &amp;")),
            );
        }
        self.ref_buf = r;
        self.ref_buf.clear();
    }

    /// Feed one chunk. `base` is the absolute offset of `buf[0]` in the document.
    pub fn feed(&mut self, buf: &[u8], base: u64) {
        let n = buf.len();
        let mut i = 0usize;
        let mut reproc = false;
        let mut st = self.state;

        while i < n {
            let b = buf[i];
            let off = base + i as u64;

            if b == b'\n' && !reproc {
                self.line += 1;
                self.line_start = off + 1;
                if (self.line - 1) % u64::from(self.cfg.stride) == 0 {
                    self.ix.checkpoints.push(self.line_start);
                }
            }
            reproc = false;

            match st {
                St::Bom => {
                    // A UTF-8 BOM may be split across chunk boundaries, so it is matched
                    // one byte at a time instead of peeking ahead into this buffer.
                    if b == 0xef {
                        st = St::Bom1;
                    } else {
                        st = St::Text;
                        reproc = true;
                        continue; // re-dispatch this byte in Text without advancing
                    }
                }
                St::Bom1 => {
                    st = if b == 0xbb { St::Bom2 } else { St::Text };
                }
                St::Bom2 => {
                    st = St::Text;
                    if b != 0xbf {
                        reproc = true;
                        continue;
                    }
                }

                St::Text => {
                    if b == b'<' {
                        st = St::Lt;
                        self.tag_start_off = off;
                        self.text_bracket = 0;
                    } else if b == b'&' {
                        self.ref_return = St::Text;
                        self.ref_buf.clear();
                        self.ref_start = off;
                        st = St::Ref;
                    } else if b == b']' {
                        self.text_bracket += 1;
                    } else if b == b'>' && self.text_bracket >= 2 {
                        self.push_error(
                            off.saturating_sub(2),
                            String::from("The sequence ']]>' is not allowed in element content. XML 1.0 §2.4 [14] CharData"),
                            Some(String::from("Escape as ]]&gt;")),
                        );
                        self.text_bracket = 0;
                    } else {
                        self.text_bracket = 0;
                        if self.depth == 0 && !is_ws(b) && !self.saw_non_ws_outside_root {
                            self.saw_non_ws_outside_root = true;
                            let msg = if self.root_seen {
                                "Non-whitespace character data after the root element. XML 1.0 §2.1 [27] Misc"
                            } else {
                                "Non-whitespace character data before the root element (or missing root). XML 1.0 §2.1"
                            };
                            self.push_error(
                                off,
                                String::from(msg),
                                Some(String::from(
                                    "Remove the stray text or move it inside the root element",
                                )),
                            );
                        }
                        // SWAR fast path: skip a run of plain character data inside the root.
                        if self.depth > 0 {
                            let next = find_text_delim(buf, i + 1);
                            if next > i + 1 {
                                i = next - 1;
                            }
                        }
                    }
                }

                St::Ref => {
                    if b == b';' {
                        self.validate_ref();
                        st = self.ref_return;
                    } else if is_name_char(b) || b == b'#' {
                        if self.ref_buf.len() < 32 {
                            self.ref_buf.push(b as char);
                        } else {
                            let off0 = self.ref_start;
                            self.push_error(
                                off0,
                                String::from("Unterminated entity reference. XML 1.0 §4.1"),
                                Some(String::from("Escape the ampersand as &amp;")),
                            );
                            st = self.ref_return;
                        }
                    } else {
                        let off0 = self.ref_start;
                        self.push_error(
                            off0,
                            String::from("Unescaped '&' (entity reference must be terminated by ';'). XML 1.0 §4.1 [68] EntityRef"),
                            Some(String::from("Replace '&' with '&amp;'")),
                        );
                        st = self.ref_return;
                        reproc = true;
                        self.state = st;
                        continue; // reprocess this byte in the previous state
                    }
                }

                St::Lt => {
                    if b == b'/' {
                        st = St::EndName;
                        self.name_buf.clear();
                    } else if b == b'?' {
                        st = St::Pi;
                    } else if b == b'!' {
                        st = St::Bang;
                    } else if is_name_start(b) {
                        st = St::StartName;
                        self.name_buf.clear();
                        self.name_buf.push(b);
                    } else {
                        self.push_error(
                            off,
                            String::from("'<' must be followed by an element name, '/', '?' or '!'. XML 1.0 §3.1 [40] STag"),
                            Some(String::from("Escape the '<' as &lt;")),
                        );
                        st = St::Text;
                    }
                }

                St::StartName => {
                    if is_name_char(b) {
                        let end = find_name_end(buf, i);
                        self.name_buf.extend_from_slice(&buf[i..end]);
                        i = end - 1;
                    } else if is_ws(b) {
                        self.open_element();
                        st = St::InTag;
                    } else if b == b'>' {
                        self.open_element();
                        st = St::Text;
                    } else if b == b'/' {
                        self.open_element();
                        st = St::Empty;
                    } else {
                        self.push_error(
                            off,
                            String::from(
                                "Invalid character in element name. XML 1.0 §2.3 [5] Name",
                            ),
                            None,
                        );
                        self.open_element();
                        st = St::InTag;
                    }
                }

                St::InTag => {
                    if is_ws(b) {
                        // skip
                    } else if b == b'>' {
                        st = St::Text;
                    } else if b == b'/' {
                        st = St::Empty;
                    } else if is_name_start(b) {
                        self.name_buf.clear();
                        self.name_buf.push(b);
                        self.attr_bytes_start = off;
                        st = St::AttrName;
                    } else {
                        self.push_error(
                            off,
                            String::from("Unexpected character in start tag; expected attribute name, '/' or '>'. XML 1.0 §3.1 [40]"),
                            Some(String::from("Quote attribute values and separate attributes with whitespace")),
                        );
                    }
                }

                St::AttrName => {
                    if is_name_char(b) {
                        let end = find_name_end(buf, i);
                        self.name_buf.extend_from_slice(&buf[i..end]);
                        i = end - 1;
                    } else {
                        let aid = self.intern_attr_buf();
                        self.ix.total_attributes += 1;
                        if self.attr_names.contains(&aid) {
                            let an = String::from(self.attrs.get(aid));
                            let off0 = self.attr_bytes_start;
                            self.push_error(
                                off0,
                                alloc::format!("Attribute '{an}' specified more than once. XML 1.0 §3.1 [WFC: Unique Att Spec]"),
                                Some(String::from("Remove the duplicate attribute")),
                            );
                        }
                        if self.attr_names.len() < 64 {
                            self.attr_names.push(aid);
                        }
                        if b == b'=' {
                            st = St::AttrPreq;
                        } else if is_ws(b) {
                            st = St::AttrEq;
                        } else {
                            let an = String::from(self.attrs.get(aid));
                            self.push_error(
                                off,
                                alloc::format!("Attribute '{an}' must be followed by '='. XML 1.0 §3.1 [41] Attribute"),
                                Some(String::from("Insert =\"\"")),
                            );
                            st = St::InTag;
                            reproc = true;
                            self.state = st;
                            continue;
                        }
                    }
                }

                St::AttrEq => {
                    if is_ws(b) {
                        // skip
                    } else if b == b'=' {
                        st = St::AttrPreq;
                    } else {
                        self.push_error(
                            off,
                            String::from("Expected '=' after attribute name. XML 1.0 §3.1 [41]"),
                            Some(String::from("Insert =\"\"")),
                        );
                        st = St::InTag;
                        reproc = true;
                        self.state = st;
                        continue;
                    }
                }

                St::AttrPreq => {
                    if is_ws(b) {
                        // skip
                    } else if b == b'"' || b == b'\'' {
                        self.quote = b;
                        st = St::AttrVal;
                    } else {
                        self.push_error(
                            off,
                            String::from(
                                "Attribute value must be quoted. XML 1.0 §3.1 [10] AttValue",
                            ),
                            Some(String::from("Enclose the value in double quotes")),
                        );
                        st = St::InTag;
                    }
                }

                St::AttrVal => {
                    if b == self.quote {
                        st = St::InTag;
                    } else if b == b'<' {
                        self.push_error(
                            off,
                            String::from("'<' is not allowed in attribute values. XML 1.0 §3.1 [WFC: No < in Attribute Values]"),
                            Some(String::from("Escape as &lt;")),
                        );
                    } else if b == b'&' {
                        self.ref_return = St::AttrVal;
                        self.ref_buf.clear();
                        self.ref_start = off;
                        st = St::Ref;
                    } else {
                        let next = find_any4(buf, i + 1, self.quote, b'<', b'&', b'\n');
                        if next > i + 1 {
                            i = next - 1;
                        }
                    }
                }

                St::Empty => {
                    if b == b'>' {
                        self.close_element(off + 1);
                        st = St::Text;
                    } else {
                        self.push_error(
                            off,
                            String::from("Expected '>' after '/' in empty-element tag. XML 1.0 §3.1 [44] EmptyElemTag"),
                            None,
                        );
                        st = St::InTag;
                    }
                }

                St::EndName => {
                    if is_name_char(b) {
                        let end = find_name_end(buf, i);
                        self.name_buf.extend_from_slice(&buf[i..end]);
                        i = end - 1;
                    } else if self.name_buf.is_empty() {
                        self.push_error(
                            off,
                            String::from("End tag must start with a name (no whitespace after '</'). XML 1.0 §3.1 [42] ETag"),
                            None,
                        );
                        st = St::EndTrail;
                    } else {
                        let eid = self.intern_name_buf();
                        let en = String::from(self.names.get(eid));
                        if self.depth == 0 {
                            let off0 = self.tag_start_off;
                            self.push_error(
                                off0,
                                alloc::format!("Unexpected end tag </{en}> — no element is open. XML 1.0 §3.1 [WFC: Element Type Match]"),
                                Some(String::from("Delete this end tag")),
                            );
                        } else if self.stack_name[self.depth - 1] != eid {
                            let expected =
                                String::from(self.names.get(self.stack_name[self.depth - 1]));
                            let mut found: i64 = -1;
                            for d in (0..self.depth).rev() {
                                if self.stack_name[d] == eid {
                                    found = d as i64;
                                    break;
                                }
                            }
                            let fix_text = if found >= 0 {
                                let mut s = String::from("Insert ");
                                let mut d2 = self.depth as i64 - 1;
                                while d2 > found {
                                    s.push_str("</");
                                    s.push_str(self.names.get(self.stack_name[d2 as usize]));
                                    s.push('>');
                                    d2 -= 1;
                                }
                                s
                            } else {
                                alloc::format!("Change to </{expected}>")
                            };
                            let off0 = self.tag_start_off;
                            self.push_error(
                                off0,
                                alloc::format!(
                                    "End tag </{en}> does not match start tag <{expected}>. XML 1.0 §3.1 [WFC: Element Type Match]"
                                ),
                                Some(fix_text),
                            );
                            if found >= 0 {
                                while self.depth as i64 > found {
                                    self.close_element(off + 1);
                                }
                            } else {
                                self.close_element(off + 1);
                            }
                        } else {
                            self.close_element(END_PENDING);
                        }
                        st = if b == b'>' { St::Text } else { St::EndTrail };
                        if b == b'>' {
                            self.patch_pending_end(off + 1);
                        }
                    }
                }

                St::EndTrail => {
                    if b == b'>' {
                        self.patch_pending_end(off + 1);
                        st = St::Text;
                    } else if !is_ws(b) {
                        self.push_error(
                            off,
                            String::from("Unexpected character in end tag. XML 1.0 §3.1 [42] ETag"),
                            None,
                        );
                    }
                }

                St::Pi => {
                    if b == b'?' {
                        st = St::PiQ;
                    } else {
                        let next = find_any4(buf, i + 1, b'?', b'\n', b'?', b'\n');
                        if next > i + 1 {
                            i = next - 1;
                        }
                    }
                }
                St::PiQ => {
                    if b == b'>' {
                        st = St::Text;
                    } else if b != b'?' {
                        st = St::Pi;
                    }
                }

                St::Bang => {
                    if b == b'-' {
                        st = St::Comment0;
                    } else if b == b'[' {
                        st = St::Cdata0;
                        self.cdata_match = 0;
                    } else if b == b'D' || b == b'd' {
                        st = St::Doctype;
                        self.doctype_bracket = 0;
                    } else {
                        self.push_error(off, String::from("Unknown markup declaration after '<!'. XML 1.0 §2.8 [29] markupdecl"), None);
                        st = St::Text;
                    }
                }
                St::Comment0 => {
                    if b == b'-' {
                        st = St::Comment;
                    } else {
                        self.push_error(
                            off,
                            String::from(
                                "Comment must start with '<!--'. XML 1.0 §2.5 [15] Comment",
                            ),
                            None,
                        );
                        st = St::Text;
                    }
                }
                St::Comment => {
                    if b == b'-' {
                        st = St::CommentD1;
                    } else {
                        let next = find_any4(buf, i + 1, b'-', b'\n', b'-', b'\n');
                        if next > i + 1 {
                            i = next - 1;
                        }
                    }
                }
                St::CommentD1 => {
                    st = if b == b'-' {
                        St::CommentD2
                    } else {
                        St::Comment
                    };
                }
                St::CommentD2 => {
                    if b == b'>' {
                        st = St::Text;
                    } else {
                        self.push_error(
                            off.saturating_sub(2),
                            String::from("'--' is not allowed inside comments. XML 1.0 §2.5 [15]"),
                            Some(String::from("Replace '--' with '- -'")),
                        );
                        st = if b == b'-' {
                            St::CommentD2
                        } else {
                            St::Comment
                        };
                    }
                }
                St::Cdata0 => {
                    let expect = b"CDATA["[self.cdata_match];
                    if b == expect {
                        self.cdata_match += 1;
                        if self.cdata_match == 6 {
                            st = St::Cdata;
                        }
                    } else {
                        self.push_error(
                            off,
                            String::from("Malformed CDATA section start; expected '<![CDATA['. XML 1.0 §2.7 [19] CDStart"),
                            None,
                        );
                        st = St::Text;
                    }
                }
                St::Cdata => {
                    if b == b']' {
                        st = St::CdataB1;
                    } else {
                        let next = find_any4(buf, i + 1, b']', b'\n', b']', b'\n');
                        if next > i + 1 {
                            i = next - 1;
                        }
                    }
                }
                St::CdataB1 => {
                    st = if b == b']' { St::CdataB2 } else { St::Cdata };
                }
                St::CdataB2 => {
                    if b == b'>' {
                        st = St::Text;
                    } else if b != b']' {
                        st = St::Cdata;
                    }
                }
                St::Doctype => {
                    if b == b'[' {
                        self.doctype_bracket += 1;
                    } else if b == b']' {
                        self.doctype_bracket -= 1;
                    } else if b == b'>' && self.doctype_bracket <= 0 {
                        st = St::Text;
                    } else {
                        let next = find_any4(buf, i + 1, b'[', b']', b'>', b'\n');
                        if next > i + 1 {
                            i = next - 1;
                        }
                    }
                }
            }

            i += 1;
        }

        self.state = st;
        self.seen_bytes = base + n as u64;
    }

    /// Finish the document: report unterminated markup and unclosed elements.
    pub fn finish(&mut self, total_bytes: u64) {
        if self.finished {
            return;
        }
        self.finished = true;
        if !self.root_seen {
            self.push_error(
                0,
                String::from("Document is empty or has no root element. XML 1.0 §2.1 [1]"),
                Some(String::from("Add a root element")),
            );
        }
        if !matches!(self.state, St::Text | St::Bom | St::Bom1 | St::Bom2) {
            self.push_error(
                total_bytes,
                String::from("Unexpected end of file inside markup (unterminated tag, comment, CDATA or PI)."),
                None,
            );
        }
        if self.depth > 0 {
            let mut open: Vec<String> = Vec::new();
            let mut d = self.depth as i64 - 1;
            while d >= 0 && open.len() < 5 {
                open.push(alloc::format!(
                    "<{}>",
                    self.names.get(self.stack_name[d as usize])
                ));
                d -= 1;
            }
            let listed = open.join(", ");
            let more = if self.depth > 5 { ", …" } else { "" };
            let appended: String = open
                .iter()
                .map(|s| alloc::format!("</{}", &s[1..]))
                .collect();
            self.push_error(
                total_bytes,
                alloc::format!(
                    "Unexpected end of file: {} element(s) not closed: {listed}{more}",
                    self.depth
                ),
                Some(alloc::format!("Append {appended}")),
            );
            while self.depth > 0 {
                self.close_element(total_bytes);
            }
        }
        self.ix.file_len = total_bytes;
        self.ix.line_count = self.line;
        self.ix.indexed_elements = self.elem_count as u32;
        self.ix.names = self.names.take();
    }

    /// Consume the scanner and return the index (call [`Scanner::finish`] first).
    pub fn into_index(mut self) -> StructuralIndex {
        if !self.finished {
            let total = self.seen_bytes;
            self.finish(total);
        }
        self.ix
    }

    /// Snapshot of the index while scanning is still in progress (progressive open).
    pub fn index_snapshot(&self) -> StructuralIndex {
        if self.finished {
            return self.ix.clone();
        }
        let mut ix = self.ix.clone();
        ix.line_count = self.line;
        ix.indexed_elements = self.elem_count as u32;
        ix.names.clone_from(self.names.as_slice());
        ix.file_len = self.seen_bytes;
        ix
    }

    /// Convenience: scan a whole buffer in one call.
    pub fn scan_all(cfg: ScannerConfig, buf: &[u8]) -> StructuralIndex {
        let mut s = Scanner::new(cfg);
        s.feed(buf, 0);
        s.finish(buf.len() as u64);
        s.into_index()
    }
}

/// Small open-addressing string interner (no external dependencies, `no_std`).
///
/// XML repeats the same handful of names millions of times, so this is one of the
/// hottest structures in the scanner: it keeps a one-entry "same as last time" cache in
/// front of an FNV-1a hash table and only allocates when a genuinely new name appears.
struct Interner {
    names: Vec<String>,
    /// `buckets[h] = id + 1`, `0` = empty. Length is always a power of two.
    buckets: Vec<u32>,
    last: u32,
}

impl Interner {
    fn new() -> Self {
        Self {
            names: Vec::new(),
            buckets: alloc::vec![0u32; 256],
            last: u32::MAX,
        }
    }

    #[inline]
    fn hash(bytes: &[u8]) -> u32 {
        let mut h: u32 = 0x811c_9dc5;
        for b in bytes {
            h ^= u32::from(*b);
            h = h.wrapping_mul(0x0100_0193);
        }
        h
    }

    fn grow(&mut self) {
        let cap = self.buckets.len() * 2;
        self.buckets.clear();
        self.buckets.resize(cap, 0);
        let mask = cap as u32 - 1;
        for (id, name) in self.names.iter().enumerate() {
            let mut h = Self::hash(name.as_bytes()) & mask;
            while self.buckets[h as usize] != 0 {
                h = (h + 1) & mask;
            }
            self.buckets[h as usize] = id as u32 + 1;
        }
    }

    fn intern(&mut self, bytes: &[u8]) -> u32 {
        if self.last != u32::MAX && self.names[self.last as usize].as_bytes() == bytes {
            return self.last;
        }
        if self.names.len() * 2 >= self.buckets.len() {
            self.grow();
        }
        let mask = self.buckets.len() as u32 - 1;
        let mut h = Self::hash(bytes) & mask;
        loop {
            let slot = self.buckets[h as usize];
            if slot == 0 {
                let id = self.names.len() as u32;
                self.names.push(String::from_utf8_lossy(bytes).into_owned());
                self.buckets[h as usize] = id + 1;
                self.last = id;
                return id;
            }
            let id = slot - 1;
            if self.names[id as usize].as_bytes() == bytes {
                self.last = id;
                return id;
            }
            h = (h + 1) & mask;
        }
    }

    #[inline]
    fn get(&self, id: u32) -> &str {
        &self.names[id as usize]
    }

    fn as_slice(&self) -> &Vec<String> {
        &self.names
    }

    fn take(&mut self) -> Vec<String> {
        core::mem::take(&mut self.names)
    }
}
