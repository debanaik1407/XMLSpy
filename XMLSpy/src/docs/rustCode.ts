/** Backtick helper: RUST_MD is a String.raw template, so inline code spans are interpolated. */
const C = "`";

export const RUST_MD = String.raw`
# Rust engine

**This is no longer a paper design: the engine in ${C}rust/${C} compiles, is tested, and is what
this page is running on.** The scan that validated the document you have open, the
structural index behind the Grid/Schema/XPath views and the streaming Find all executed
inside ${C}xmlspy_parse${C} compiled to ${C}wasm32-unknown-unknown${C} — check the status bar: it says
**⚙ Rust/WASM** when the module instantiated, **⚙ TS fallback** when it did not.

| what | where |
| --- | --- |
| workspace | ${C}rust/${C} — ${C}xmlspy-core${C}, ${C}xmlspy-index${C}, ${C}xmlspy-parse${C}, ${C}xmlspy-wasm${C}, ${C}xmlspy-cli${C} |
| build for the browser | ${C}npm run build:wasm${C} → base64-inlined into ${C}src/engine/wasmBinary.ts${C} |
| native CLI | ${C}cargo run -p xmlspy-cli --release -- bench big.xml${C} |
| parity vs the TypeScript scanner | ${C}npm run test:parity${C} — 84 checks, index arrays + every diagnostic string |

Measured on a 64 MiB PurchaseOrder corpus (2.2 M elements), single thread: **301 MB/s**
native, **252 MB/s** through WebAssembly in V8, **65 MB/s** for the TypeScript scanner
that remains as the fallback — so the port is worth ~3.9×, and WASM keeps ~84 % of native.

The shipped crates deliberately differ from the Phase 0/1 sketch reproduced below, which
is kept for the design rationale (event model, SIMD classification, .xsi layout, Leptos
view). Where they disagree, the shipped crates win:

* **zero external dependencies** — no memmap2/rayon/serde/bytemuck/clap; ${C}core${C}, ${C}index${C}
  and ${C}parse${C} are ${C}no_std${C} + ${C}alloc${C}, so the same code serves the CLI and the browser.
* **no wasm-bindgen** — the module is a plain ${C}cdylib${C} with a hand-written C ABI
  (${C}xs_scanner_new/feed/finish/snapshot${C}, ${C}xs_finder_*${C}); offsets cross as ${C}f64${C} so there
  is no BigInt, and results cross as one serialized ${C}.xsi${C} buffer.
* **SWAR instead of intrinsics** — 8-byte-at-a-time delimiter search in portable ${C}u64${C}
  arithmetic, so one code path covers x86-64, aarch64 and the WASM MVP.
* the browser reads the file with ${C}Blob.slice()${C} in 8 MiB chunks instead of ${C}memmap2${C}.

## Cargo.toml (workspace)

~~~toml
[workspace]
resolver = "2"
members = ["crates/*", "bench/xmlspy-bench", "xtask"]

[workspace.package]
edition = "2021"
rust-version = "1.85"
license = "MIT OR Apache-2.0"

[workspace.dependencies]
memmap2 = "0.9"
ropey = "1.6"
rayon = "1.10"
thiserror = "2"
tracing = "0.1"
bytemuck = { version = "1.21", features = ["derive"] }
xxhash-rust = { version = "0.8", features = ["xxh3"] }
crossbeam-channel = "0.5"
parking_lot = "0.12"
lru = "0.12"
serde = { version = "1", features = ["derive"] }
clap = { version = "4", features = ["derive"] }
criterion = "0.5"
proptest = "1.5"
quick-xml = "0.37"
leptos = { version = "0.7", features = ["csr"] }
wasm-bindgen = "0.2"
wasm-bindgen-futures = "0.4"
web-sys = "0.3"
js-sys = "0.3"

[profile.release]
lto = "fat"
codegen-units = 1
opt-level = 3
panic = "abort"

[profile.release.package.xmlspy-ui]
opt-level = "s"   # bundle size: wasm-opt -Oz + brotli ⇒ ~1.9 MB
~~~

## crates/xmlspy-core/Cargo.toml

~~~toml
[package]
name = "xmlspy-core"
version = "0.1.0"
edition.workspace = true

[dependencies]
thiserror.workspace = true
tracing.workspace = true
ropey.workspace = true
lru.workspace = true
parking_lot.workspace = true
memmap2 = { workspace = true, optional = true }

[features]
default = ["mmap"]
mmap = ["dep:memmap2"]

[lints.rust]
unsafe_code = "deny"      # allowed only inside src/mmap.rs via #[allow]
~~~

## crates/xmlspy-core/src/error.rs

~~~rust
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("I/O error at offset {offset}: {source}")]
    Io { offset: u64, #[source] source: std::io::Error },
    #[error("offset {0} is beyond end of document ({1} bytes)")]
    OutOfRange(u64, u64),
    #[error("operation cancelled")]
    Cancelled,
    #[error("index is stale (file changed: {reason})")]
    StaleIndex { reason: &'static str },
    #[error("edit buffer overflow: add-buffer exceeded {0} bytes")]
    AddBufferOverflow(u64),
}
pub type Result<T> = std::result::Result<T, CoreError>;
~~~

## crates/xmlspy-core/src/source.rs — ByteSource (mmap + page cache)

~~~rust
use crate::{CoreError, Result};
use std::ops::Range;
use std::sync::Arc;

pub const PAGE: u64 = 4096;
pub const CHUNK: u64 = 8 * 1024 * 1024; // 2048 pages, page-aligned

/// Random-access, read-only bytes of the *original* file. Implementations must be cheap to clone
/// (Arc inside) and safe to use from rayon workers.
pub trait ByteSource: Send + Sync + 'static {
    fn len(&self) -> u64;
    /// Returns a view of [range). Implementations may return fewer bytes only at EOF.
    fn slice(&self, range: Range<u64>) -> Result<Bytes>;
    /// Advise sequential/random access (no-op where unsupported).
    fn advise(&self, _hint: AccessHint) {}
}

#[derive(Clone, Copy, Debug)]
pub enum AccessHint { Sequential, Random, DontNeed(Range<u64>) }

/// Owned-or-borrowed bytes; for mmap it borrows the map (zero copy), for the browser page cache
/// it holds an Arc<[u8]> page.
pub enum Bytes { Mapped(Arc<dyn AsRef<[u8]> + Send + Sync>, Range<usize>), Owned(Arc<[u8]>) }

impl AsRef<[u8]> for Bytes {
    fn as_ref(&self) -> &[u8] {
        match self {
            Bytes::Mapped(m, r) => &(**m).as_ref()[r.clone()],
            Bytes::Owned(v) => v,
        }
    }
}

/// Iterate page-aligned chunks of a source (used by the index builder and search).
pub fn chunks(len: u64, chunk: u64) -> impl Iterator<Item = Range<u64>> {
    debug_assert_eq!(chunk % PAGE, 0);
    (0..len).step_by(chunk as usize).map(move |s| s..(s + chunk).min(len))
}

pub fn check_range(len: u64, r: &Range<u64>) -> Result<()> {
    if r.end > len || r.start > r.end { return Err(CoreError::OutOfRange(r.end, len)); }
    Ok(())
}
~~~

## crates/xmlspy-core/src/mmap.rs — the only unsafe module in core (audited)

~~~rust
#![allow(unsafe_code)]
use crate::source::{AccessHint, ByteSource, Bytes, check_range};
use crate::{CoreError, Result};
use memmap2::{Advice, Mmap, MmapOptions};
use std::{fs::File, ops::Range, path::Path, sync::Arc};

pub struct MmapSource { map: Arc<Mmap>, len: u64 }

impl MmapSource {
    pub fn open(path: &Path) -> Result<Self> {
        let file = File::open(path).map_err(|e| CoreError::Io { offset: 0, source: e })?;
        let len = file.metadata().map_err(|e| CoreError::Io { offset: 0, source: e })?.len();
        // SAFETY: the file is opened read-only and mapped MAP_PRIVATE. Concurrent truncation by
        // another process can SIGBUS on access; we mitigate by (a) validating length+mtime
        // before each job and (b) advising the user to keep the file stable. No other unsafe use.
        let map = unsafe { MmapOptions::new().populate().map(&file) }
            .map_err(|e| CoreError::Io { offset: 0, source: e })?;
        Ok(Self { map: Arc::new(map), len })
    }
}

impl ByteSource for MmapSource {
    fn len(&self) -> u64 { self.len }
    fn slice(&self, r: Range<u64>) -> Result<Bytes> {
        check_range(self.len, &r)?;
        Ok(Bytes::Mapped(self.map.clone(), r.start as usize..r.end as usize))
    }
    fn advise(&self, hint: AccessHint) {
        let _ = match hint {
            AccessHint::Sequential => self.map.advise(Advice::Sequential),
            AccessHint::Random => self.map.advise(Advice::Random),
            AccessHint::DontNeed(r) => self.map.advise_range(Advice::DontNeed, r.start as usize, (r.end - r.start) as usize),
        };
    }
}
~~~

## crates/xmlspy-core/src/buffer.rs — rope of pieces (O(log n) edits, streamed save)

~~~rust
use crate::{CoreError, Result, source::ByteSource};
use std::{io::Write, ops::Range, sync::Arc};

/// A piece references either the immutable original file or the append-only add buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Piece { Original { off: u64, len: u64 }, Added { off: u64, len: u64 } }
impl Piece {
    #[inline] pub fn len(&self) -> u64 { match *self { Piece::Original { len, .. } | Piece::Added { len, .. } => len } }
    fn split(&self, at: u64) -> (Piece, Piece) {
        match *self {
            Piece::Original { off, len } => (Piece::Original { off, len: at }, Piece::Original { off: off + at, len: len - at }),
            Piece::Added { off, len } => (Piece::Added { off, len: at }, Piece::Added { off: off + at, len: len - at }),
        }
    }
}

/// Balanced tree over pieces with byte-length summaries. B = 32 keeps depth ≤ 6 for 10^9 pieces.
/// (In production this is a ropey-style B-tree with persistent snapshots for undo; this Phase 1
/// version is an order-statistic B-tree with the same public API.)
pub struct EditBuffer<S: ByteSource> {
    source: Arc<S>,
    add: Vec<u8>,
    tree: Tree,
    len: u64,
    max_add: u64,
}

struct Tree { root: Node }
enum Node { Leaf(Vec<Piece>), Internal { children: Vec<Node>, lens: Vec<u64> } }
const B: usize = 32;

impl Node {
    fn len(&self) -> u64 {
        match self { Node::Leaf(p) => p.iter().map(Piece::len).sum(), Node::Internal { lens, .. } => lens.iter().sum() }
    }
    /// Split so that a piece boundary exists at 'at'; returns true if the node overflowed.
    fn ensure_boundary(&mut self, at: u64) {
        match self {
            Node::Leaf(pieces) => {
                let mut acc = 0;
                for i in 0..pieces.len() {
                    let l = pieces[i].len();
                    if at == acc { return; }
                    if at < acc + l { let (a, b) = pieces[i].split(at - acc); pieces[i] = a; pieces.insert(i + 1, b); return; }
                    acc += l;
                }
            }
            Node::Internal { children, lens } => {
                let mut acc = 0;
                for (i, l) in lens.iter().enumerate() {
                    if at <= acc + *l { children[i].ensure_boundary(at - acc); return; }
                    acc += l;
                }
            }
        }
    }
    fn insert_piece(&mut self, at: u64, p: Piece) -> Option<Node> {
        match self {
            Node::Leaf(pieces) => {
                let mut acc = 0; let mut idx = pieces.len();
                for (i, q) in pieces.iter().enumerate() { if at == acc { idx = i; break; } acc += q.len(); }
                pieces.insert(idx, p);
                if pieces.len() > 2 * B { let right = pieces.split_off(B); Some(Node::Leaf(right)) } else { None }
            }
            Node::Internal { children, lens } => {
                let mut acc = 0; let mut i = children.len() - 1;
                for (j, l) in lens.iter().enumerate() { if at <= acc + *l { i = j; break; } acc += l; }
                let split = children[i].insert_piece(at - acc, p);
                lens[i] = children[i].len();
                if let Some(n) = split { lens.insert(i + 1, n.len()); children.insert(i + 1, n); }
                if children.len() > 2 * B {
                    let rc = children.split_off(B); let rl = lens.split_off(B);
                    Some(Node::Internal { children: rc, lens: rl })
                } else { None }
            }
        }
    }
    fn remove_range(&mut self, r: Range<u64>) {
        match self {
            Node::Leaf(pieces) => {
                let mut acc = 0;
                pieces.retain(|p| { let keep = !(acc >= r.start && acc + p.len() <= r.end); acc += p.len(); keep });
            }
            Node::Internal { children, lens } => {
                let mut acc = 0;
                for (i, c) in children.iter_mut().enumerate() {
                    let l = lens[i];
                    if acc < r.end && acc + l > r.start {
                        let s = r.start.saturating_sub(acc); let e = (r.end - acc).min(l);
                        c.remove_range(s..e); lens[i] = c.len();
                    }
                    acc += l;
                }
            }
        }
    }
    fn for_each(&self, f: &mut dyn FnMut(&Piece)) {
        match self { Node::Leaf(p) => p.iter().for_each(|x| f(x)), Node::Internal { children, .. } => children.iter().for_each(|c| c.for_each(f)) }
    }
}

impl<S: ByteSource> EditBuffer<S> {
    pub fn new(source: Arc<S>) -> Self {
        let len = source.len();
        let root = if len == 0 { Node::Leaf(vec![]) } else { Node::Leaf(vec![Piece::Original { off: 0, len }]) };
        Self { source, add: Vec::new(), tree: Tree { root }, len, max_add: 1 << 36 }
    }
    pub fn len(&self) -> u64 { self.len }

    pub fn insert(&mut self, at: u64, text: &[u8]) -> Result<()> {
        if at > self.len { return Err(CoreError::OutOfRange(at, self.len)); }
        if text.is_empty() { return Ok(()); }
        if self.add.len() as u64 + text.len() as u64 > self.max_add { return Err(CoreError::AddBufferOverflow(self.max_add)); }
        let off = self.add.len() as u64;
        self.add.extend_from_slice(text);
        self.tree.root.ensure_boundary(at);
        if let Some(right) = self.tree.root.insert_piece(at, Piece::Added { off, len: text.len() as u64 }) {
            let left = std::mem::replace(&mut self.tree.root, Node::Leaf(vec![]));
            let lens = vec![left.len(), right.len()];
            self.tree.root = Node::Internal { children: vec![left, right], lens };
        }
        self.len += text.len() as u64;
        Ok(())
    }

    pub fn delete(&mut self, r: Range<u64>) -> Result<()> {
        if r.end > self.len || r.start > r.end { return Err(CoreError::OutOfRange(r.end, self.len)); }
        if r.is_empty() { return Ok(()); }
        self.tree.root.ensure_boundary(r.start);
        self.tree.root.ensure_boundary(r.end);
        self.tree.root.remove_range(r.clone());
        self.len -= r.end - r.start;
        Ok(())
    }

    /// Streamed save: original pieces are copied in ≤ 8 MiB windows straight from the source,
    /// added pieces from the add buffer. Never materializes the document.
    pub fn write_to<W: Write>(&self, mut w: W) -> Result<u64> {
        let mut written = 0u64;
        let mut err: Option<CoreError> = None;
        self.tree.root.for_each(&mut |p| {
            if err.is_some() { return; }
            let res = (|| -> Result<()> {
                match *p {
                    Piece::Added { off, len } => w.write_all(&self.add[off as usize..(off + len) as usize])
                        .map_err(|e| CoreError::Io { offset: written, source: e }),
                    Piece::Original { off, len } => {
                        let mut cur = off;
                        while cur < off + len {
                            let end = (cur + crate::source::CHUNK).min(off + len);
                            let b = self.source.slice(cur..end)?;
                            w.write_all(b.as_ref()).map_err(|e| CoreError::Io { offset: cur, source: e })?;
                            cur = end;
                        }
                        Ok(())
                    }
                }
            })();
            match res { Ok(()) => written += p.len(), Err(e) => err = Some(e) }
        });
        if let Some(e) = err { return Err(e); }
        Ok(written)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::{ByteSource, Bytes, check_range};
    use proptest::prelude::*;
    struct Mem(Arc<[u8]>);
    impl ByteSource for Mem {
        fn len(&self) -> u64 { self.0.len() as u64 }
        fn slice(&self, r: Range<u64>) -> Result<Bytes> { check_range(self.len(), &r)?; Ok(Bytes::Owned(self.0[r.start as usize..r.end as usize].into())) }
    }
    proptest! {
        #[test]
        fn matches_vec_model(orig in proptest::collection::vec(any::<u8>(), 0..200),
                             ops in proptest::collection::vec((0u64..300, 0u64..40, proptest::collection::vec(any::<u8>(), 0..8)), 0..40)) {
            let mut model = orig.clone();
            let mut buf = EditBuffer::new(Arc::new(Mem(orig.into())));
            for (pos, del, ins) in ops {
                let pos = pos.min(model.len() as u64);
                let del = del.min(model.len() as u64 - pos);
                buf.delete(pos..pos + del).unwrap(); model.drain(pos as usize..(pos + del) as usize);
                buf.insert(pos, &ins).unwrap(); model.splice(pos as usize..pos as usize, ins.iter().cloned());
            }
            let mut out = Vec::new(); buf.write_to(&mut out).unwrap();
            prop_assert_eq!(out, model);
        }
    }
}
~~~

## crates/xmlspy-parse/src/scanner.rs — resumable scanner with SIMD classification

~~~rust
//! Chunk-resumable XML tokenizer. Byte-exact port of the state machine used by the browser demo.
//! SIMD fast path: in TEXT state we skip to the next interesting byte ('<', '&', ']', '\n')
//! 64 bytes at a time; the scalar state machine only runs on structural bytes.
#![forbid(unsafe_code)] // SIMD helpers live in simd.rs (audited, cfg-gated)

use crate::simd::find_structural; // fn(&[u8]) -> Option<usize>, portable_simd / std::arch / scalar
use crate::names::{is_name_char, is_name_start};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum State {
    Bom, Text, Lt, StartName, InTag, AttrName, AttrEq, AttrPreq, AttrVal, Empty, EndName, EndTrail,
    Pi, PiQ, Bang, Comment0, Comment, CommentD1, CommentD2, Cdata0, Cdata, CdataB1, CdataB2, Doctype, Ref,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event<'a> {
    StartElement { name: &'a [u8], off: u64, line: u64 },
    Attribute { name: &'a [u8], off: u64 },
    EndElement { off_after_gt: u64 },
    Newline { off_after: u64 },
    Error { code: ErrorCode, off: u64, line: u64, col: u32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum ErrorCode {
    MultipleRoots = 1, TextOutsideRoot, MalformedRef, UnescapedAmp, BadLtFollow, BadNameChar, BadTagChar,
    DupAttr, MissingEq, UnquotedAttr, LtInAttr, BadEmptyTag, BadEndTag, Mismatch, UnexpectedEnd,
    BadEndTagChar, UnknownDecl, BadComment, DoubleDashInComment, BadCdata, CdEndInContent, NoRoot, Eof, Unclosed,
}

/// Persistent scanner state — serialized into the index journal after each chunk (resumability).
#[derive(Clone, Debug)]
pub struct Scanner {
    pub state: State,
    ref_return: State,
    quote: u8,
    pub line: u64,
    pub line_start: u64,
    depth: u32,
    stack: Vec<u32>,          // name ids of open elements
    name: Vec<u8>,
    attrs: Vec<u32>,          // name ids in current start tag (dup check)
    names: crate::names::Interner,
    tag_start: u64,
    text_bracket: u8,
    cdata_match: u8,
    doctype_bracket: i32,
    root_seen: bool,
    root_closed: bool,
    ref_buf: Vec<u8>,
    ref_start: u64,
}

impl Scanner {
    pub fn new() -> Self {
        Self { state: State::Bom, ref_return: State::Text, quote: 0, line: 1, line_start: 0, depth: 0,
               stack: Vec::with_capacity(64), name: Vec::with_capacity(64), attrs: Vec::with_capacity(16),
               names: Default::default(), tag_start: 0, text_bracket: 0, cdata_match: 0, doctype_bracket: 0,
               root_seen: false, root_closed: false, ref_buf: Vec::new(), ref_start: 0 }
    }
    pub fn names(&self) -> &crate::names::Interner { &self.names }
    pub fn depth(&self) -> u32 { self.depth }

    #[inline(always)]
    fn err(&self, sink: &mut impl FnMut(Event), code: ErrorCode, off: u64) {
        sink(Event::Error { code, off, line: self.line, col: (off - self.line_start + 1) as u32 });
    }

    /// Feed one chunk. 'base' is the absolute offset of buf[0]. Events are delivered to 'sink'.
    pub fn feed(&mut self, buf: &[u8], base: u64, sink: &mut impl FnMut(Event)) {
        let mut i = 0usize;
        let n = buf.len();
        while i < n {
            // ---- SIMD fast path: skip plain character data ----
            if self.state == State::Text && self.text_bracket == 0 {
                if let Some(k) = find_structural(&buf[i..]) {
                    if k > 0 && self.depth == 0 && self.root_seen == false {
                        // must still validate that skipped bytes are whitespace (rare path)
                        if let Some(p) = buf[i..i + k].iter().position(|b| !matches!(b, b' ' | b'\t' | b'\r' | b'\n')) {
                            self.err(sink, ErrorCode::TextOutsideRoot, base + (i + p) as u64);
                        }
                    }
                    i += k;
                } else { i = n; break; }
            }
            let b = buf[i];
            let off = base + i as u64;
            if b == b'\n' { self.line += 1; self.line_start = off + 1; sink(Event::Newline { off_after: off + 1 }); }
            self.step(b, off, buf, &mut i, sink);
            i += 1;
        }
    }

    #[inline(always)]
    fn step(&mut self, b: u8, off: u64, buf: &[u8], i: &mut usize, sink: &mut impl FnMut(Event)) {
        use State::*;
        match self.state {
            Bom => { self.state = Text; if b == 0xEF && buf.get(*i + 1) == Some(&0xBB) && buf.get(*i + 2) == Some(&0xBF) { *i += 2; } else { self.step(b, off, buf, i, sink); } }
            Text => match b {
                b'<' => { self.state = Lt; self.tag_start = off; self.text_bracket = 0; }
                b'&' => { self.ref_return = Text; self.ref_buf.clear(); self.ref_start = off; self.state = Ref; }
                b']' => self.text_bracket = self.text_bracket.saturating_add(1),
                b'>' if self.text_bracket >= 2 => { self.err(sink, ErrorCode::CdEndInContent, off - 2); self.text_bracket = 0; }
                _ => { self.text_bracket = 0; if self.depth == 0 && !b.is_ascii_whitespace() { self.err(sink, ErrorCode::TextOutsideRoot, off); } }
            },
            Ref => match b {
                b';' => { if !crate::names::valid_reference(&self.ref_buf) { self.err(sink, ErrorCode::MalformedRef, self.ref_start); } self.state = self.ref_return; }
                b'#' => self.ref_buf.push(b),
                _ if is_name_char(b) && self.ref_buf.len() < 32 => self.ref_buf.push(b),
                _ => { self.err(sink, ErrorCode::UnescapedAmp, self.ref_start); self.state = self.ref_return; self.reprocess(b, i); }
            },
            Lt => match b {
                b'/' => { self.state = EndName; self.name.clear(); }
                b'?' => self.state = Pi,
                b'!' => self.state = Bang,
                _ if is_name_start(b) => { self.state = StartName; self.name.clear(); self.name.push(b); }
                _ => { self.err(sink, ErrorCode::BadLtFollow, off); self.state = Text; }
            },
            StartName => {
                if is_name_char(b) { self.name.push(b); }
                else {
                    self.open(off, sink);
                    self.state = match b { b'>' => Text, b'/' => Empty, b if b.is_ascii_whitespace() => InTag, _ => { self.err(sink, ErrorCode::BadNameChar, off); InTag } };
                }
            }
            InTag => match b {
                b if b.is_ascii_whitespace() => {}
                b'>' => self.state = Text,
                b'/' => self.state = Empty,
                _ if is_name_start(b) => { self.name.clear(); self.name.push(b); self.tag_start = off; self.state = AttrName; }
                _ => self.err(sink, ErrorCode::BadTagChar, off),
            },
            AttrName => {
                if is_name_char(b) { self.name.push(b); }
                else {
                    let id = self.names.intern(&self.name);
                    if self.attrs.contains(&id) { self.err(sink, ErrorCode::DupAttr, self.tag_start); }
                    if self.attrs.len() < 64 { self.attrs.push(id); }
                    sink(Event::Attribute { name: self.names.get(id), off: self.tag_start });
                    match b { b'=' => self.state = AttrPreq, b if b.is_ascii_whitespace() => self.state = AttrEq,
                              _ => { self.err(sink, ErrorCode::MissingEq, off); self.state = InTag; self.reprocess(b, i); } }
                }
            }
            AttrEq => match b { b if b.is_ascii_whitespace() => {}, b'=' => self.state = AttrPreq,
                                _ => { self.err(sink, ErrorCode::MissingEq, off); self.state = InTag; self.reprocess(b, i); } },
            AttrPreq => match b { b if b.is_ascii_whitespace() => {}, b'"' | b'\'' => { self.quote = b; self.state = AttrVal; }
                                  _ => { self.err(sink, ErrorCode::UnquotedAttr, off); self.state = InTag; } },
            AttrVal => match b {
                q if q == self.quote => self.state = InTag,
                b'<' => self.err(sink, ErrorCode::LtInAttr, off),
                b'&' => { self.ref_return = AttrVal; self.ref_buf.clear(); self.ref_start = off; self.state = Ref; }
                _ => {}
            },
            Empty => { if b == b'>' { self.close(off + 1, sink); self.state = Text; } else { self.err(sink, ErrorCode::BadEmptyTag, off); self.state = InTag; } }
            EndName => {
                if is_name_char(b) { self.name.push(b); }
                else if self.name.is_empty() { self.err(sink, ErrorCode::BadEndTag, off); self.state = EndTrail; }
                else {
                    let id = self.names.intern(&self.name);
                    match self.stack.last() {
                        None => self.err(sink, ErrorCode::UnexpectedEnd, self.tag_start),
                        Some(&top) if top != id => {
                            self.err(sink, ErrorCode::Mismatch, self.tag_start);
                            if let Some(pos) = self.stack.iter().rposition(|&x| x == id) { while self.stack.len() > pos { self.close(off + 1, sink); } }
                            else { self.close(off + 1, sink); }
                        }
                        Some(_) => self.close(off + 1, sink),
                    }
                    self.state = if b == b'>' { Text } else { EndTrail };
                }
            }
            EndTrail => { if b == b'>' { self.state = Text; } else if !b.is_ascii_whitespace() { self.err(sink, ErrorCode::BadEndTagChar, off); } }
            Pi => if b == b'?' { self.state = PiQ },
            PiQ => self.state = match b { b'>' => Text, b'?' => PiQ, _ => Pi },
            Bang => match b { b'-' => self.state = Comment0, b'[' => { self.state = Cdata0; self.cdata_match = 0; }
                              b'D' | b'd' => { self.state = Doctype; self.doctype_bracket = 0; }
                              _ => { self.err(sink, ErrorCode::UnknownDecl, off); self.state = Text; } },
            Comment0 => { if b == b'-' { self.state = Comment } else { self.err(sink, ErrorCode::BadComment, off); self.state = Text; } }
            Comment => if b == b'-' { self.state = CommentD1 },
            CommentD1 => self.state = if b == b'-' { CommentD2 } else { Comment },
            CommentD2 => { if b == b'>' { self.state = Text } else { self.err(sink, ErrorCode::DoubleDashInComment, off - 2); self.state = if b == b'-' { CommentD2 } else { Comment }; } }
            Cdata0 => { if b == b"CDATA["[self.cdata_match as usize] { self.cdata_match += 1; if self.cdata_match == 6 { self.state = Cdata; } }
                        else { self.err(sink, ErrorCode::BadCdata, off); self.state = Text; } }
            Cdata => if b == b']' { self.state = CdataB1 },
            CdataB1 => self.state = if b == b']' { CdataB2 } else { Cdata },
            CdataB2 => self.state = match b { b'>' => Text, b']' => CdataB2, _ => Cdata },
            Doctype => match b { b'[' => self.doctype_bracket += 1, b']' => self.doctype_bracket -= 1,
                                 b'>' if self.doctype_bracket <= 0 => self.state = Text, _ => {} },
        }
    }

    #[inline(always)]
    fn reprocess(&mut self, b: u8, i: &mut usize) { *i -= 1; if b == b'\n' { self.line -= 1; } }

    fn open(&mut self, _off: u64, sink: &mut impl FnMut(Event)) {
        let id = self.names.intern(&self.name);
        if self.root_closed && self.depth == 0 { self.err(sink, ErrorCode::MultipleRoots, self.tag_start); }
        self.root_seen = true;
        sink(Event::StartElement { name: self.names.get(id), off: self.tag_start, line: self.line });
        self.stack.push(id); self.depth += 1; self.attrs.clear();
    }
    fn close(&mut self, off_after_gt: u64, sink: &mut impl FnMut(Event)) {
        if self.stack.pop().is_some() { self.depth -= 1; if self.depth == 0 { self.root_closed = true; } sink(Event::EndElement { off_after_gt }); }
    }
    pub fn finish(&mut self, total: u64, sink: &mut impl FnMut(Event)) {
        if !self.root_seen { self.err(sink, ErrorCode::NoRoot, 0); }
        if !matches!(self.state, State::Text | State::Bom) { self.err(sink, ErrorCode::Eof, total); }
        if self.depth > 0 { self.err(sink, ErrorCode::Unclosed, total); while self.depth > 0 { self.close(total, sink); } }
    }
}
~~~

## crates/xmlspy-parse/src/simd.rs

~~~rust
//! Find the first byte ∈ {'<', '&', ']', '\n'} — 64 bytes per iteration with AVX-512/AVX2/NEON,
//! 16 with SSE2/WASM simd128, scalar fallback otherwise. This is the only unsafe module in the crate.
#![allow(unsafe_code)]

#[inline]
pub fn find_structural(hay: &[u8]) -> Option<usize> {
    #[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
    { return unsafe { avx2(hay) }; }
    #[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
    { return unsafe { wasm128(hay) }; }
    #[allow(unreachable_code)]
    scalar(hay)
}

#[inline]
pub fn scalar(hay: &[u8]) -> Option<usize> {
    hay.iter().position(|&b| b == b'<' || b == b'&' || b == b']' || b == b'\n')
}

#[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
#[target_feature(enable = "avx2")]
unsafe fn avx2(hay: &[u8]) -> Option<usize> {
    use std::arch::x86_64::*;
    let lt = _mm256_set1_epi8(b'<' as i8); let amp = _mm256_set1_epi8(b'&' as i8);
    let rb = _mm256_set1_epi8(b']' as i8); let nl = _mm256_set1_epi8(b'\n' as i8);
    let mut i = 0;
    while i + 32 <= hay.len() {
        // SAFETY: i+32 <= len, loadu tolerates any alignment.
        let v = _mm256_loadu_si256(hay.as_ptr().add(i) as *const __m256i);
        let m = _mm256_or_si256(_mm256_or_si256(_mm256_cmpeq_epi8(v, lt), _mm256_cmpeq_epi8(v, amp)),
                                _mm256_or_si256(_mm256_cmpeq_epi8(v, rb), _mm256_cmpeq_epi8(v, nl)));
        let bits = _mm256_movemask_epi8(m) as u32;
        if bits != 0 { return Some(i + bits.trailing_zeros() as usize); }
        i += 32;
    }
    scalar(&hay[i..]).map(|p| i + p)
}

#[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
unsafe fn wasm128(hay: &[u8]) -> Option<usize> {
    use std::arch::wasm32::*;
    let mut i = 0;
    while i + 16 <= hay.len() {
        let v = v128_load(hay.as_ptr().add(i) as *const v128);
        let m = v128_or(v128_or(i8x16_eq(v, i8x16_splat(b'<' as i8)), i8x16_eq(v, i8x16_splat(b'&' as i8))),
                        v128_or(i8x16_eq(v, i8x16_splat(b']' as i8)), i8x16_eq(v, i8x16_splat(b'\n' as i8))));
        let bits = i8x16_bitmask(m);
        if bits != 0 { return Some(i + bits.trailing_zeros() as usize); }
        i += 16;
    }
    scalar(&hay[i..]).map(|p| i + p)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    proptest! { #[test] fn simd_matches_scalar(v in proptest::collection::vec(any::<u8>(), 0..300)) { prop_assert_eq!(find_structural(&v), scalar(&v)); } }
}
~~~

## crates/xmlspy-index/src/format.rs and builder.rs

~~~rust
use bytemuck::{Pod, Zeroable};

pub const MAGIC: [u8; 4] = *b"XSI\0";
pub const VERSION: u16 = 1;
pub const LINE_STRIDE: u64 = 64;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
pub struct Header {
    pub magic: [u8; 4], pub version: u16, pub flags: u16,
    pub file_len: u64, pub file_mtime_ns: u64, pub file_hash_prefix: u64,
    pub node_count: u64, pub line_count: u64, pub section_dir_off: u64, pub crc32: u32, pub _pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
pub struct Section { pub kind: u16, pub _pad: u16, pub off: u64, pub len: u64 }
pub mod kind { pub const START: u16 = 1; pub const END: u16 = 2; pub const NAME: u16 = 3; pub const DEPTH: u16 = 4;
               pub const PARENT: u16 = 5; pub const FLAGS: u16 = 6; pub const LINES: u16 = 7; pub const NAMES: u16 = 8;
               pub const ERRORS: u16 = 9; pub const JOURNAL: u16 = 10; }

/// 40-bit packed offsets (5 bytes) — 10 GiB needs 34 bits; 40 bits covers 1 TiB.
#[inline] pub fn pack40(v: u64, out: &mut [u8; 5]) { debug_assert!(v < 1 << 40); out.copy_from_slice(&v.to_le_bytes()[..5]); }
#[inline] pub fn unpack40(b: &[u8; 5]) -> u64 { let mut t = [0u8; 8]; t[..5].copy_from_slice(b); u64::from_le_bytes(t) }
~~~

~~~rust
//! builder.rs — parallel, single-pass, resumable
use crate::format::*;
use rayon::prelude::*;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use xmlspy_core::source::{chunks, ByteSource, CHUNK};
use xmlspy_parse::scanner::{Event, Scanner};

pub struct Progress { pub bytes: AtomicU64, pub elements: AtomicU64, pub errors: AtomicU64, pub cancel: AtomicBool }

/// Per-chunk speculative result (built in parallel), joined serially afterwards.
struct ChunkOut { start: Vec<u64>, end: Vec<u64>, name: Vec<u32>, depth_delta: i32, min_depth: i32,
                  lines: Vec<u64>, newline_count: u64, unmatched_ends: Vec<(u64, u32)>, errors: Vec<(u16, u64)>,
                  scanner_state: Scanner /* for journal */ }

pub fn build<S: ByteSource>(src: &S, prog: &Progress, resume_from: Option<u64>) -> Result<IndexInMemory, BuildError> {
    src.advise(xmlspy_core::source::AccessHint::Sequential);
    let ranges: Vec<_> = chunks(src.len(), CHUNK).collect();
    let skip = resume_from.map(|b| (b / CHUNK) as usize).unwrap_or(0);
    // 1) parallel speculative scan (each chunk starts in State::Text; the join pass fixes heads)
    let outs: Vec<ChunkOut> = ranges[skip..].par_iter().map(|r| {
        if prog.cancel.load(Ordering::Relaxed) { return ChunkOut::cancelled(); }
        let bytes = src.slice(r.clone()).expect("range checked");
        let mut sc = Scanner::new_speculative(); // state Text, unknown depth (relative)
        let mut out = ChunkOut::with_capacity((r.end - r.start) as usize / 96);
        let mut rel_depth = 0i32;
        sc.feed(bytes.as_ref(), r.start, &mut |ev| match ev {
            Event::StartElement { off, .. } => { out.start.push(off); out.name.push(sc_name_id(&sc)); rel_depth += 1; }
            Event::EndElement { off_after_gt } => { rel_depth -= 1; out.min_depth = out.min_depth.min(rel_depth); if let Some(e) = out.end_slot() { *e = off_after_gt } else { out.unmatched_ends.push((off_after_gt, 0)); } }
            Event::Newline { off_after } => { out.newline_count += 1; if out.newline_count % LINE_STRIDE == 0 { out.lines.push(off_after); } }
            Event::Error { code, off, .. } => out.errors.push((code as u16, off)),
            Event::Attribute { .. } => {}
        });
        out.depth_delta = rel_depth; out.scanner_state = sc;
        prog.bytes.fetch_add(r.end - r.start, Ordering::Relaxed);
        prog.elements.fetch_add(out.start.len() as u64, Ordering::Relaxed);
        out
    }).collect();
    if prog.cancel.load(Ordering::Relaxed) { return Err(BuildError::Cancelled); }
    // 2) serial join: absolute depth = prefix-sum of depth_delta; unmatched ends of chunk k close
    //    elements still open at the end of chunk k-1 (stack carried forward); line numbers = prefix-sum.
    let mut idx = IndexInMemory::default();
    let mut open: Vec<u32> = Vec::new(); // node ids currently open
    let mut depth = 0i32; let mut line_base = 0u64;
    for o in &outs {
        for &(off, _) in &o.unmatched_ends { if let Some(id) = open.pop() { idx.end[id as usize] = off; depth -= 1; } else { idx.errors.push((xmlspy_parse::scanner::ErrorCode::UnexpectedEnd as u16, off)); } }
        // (start/end pairs fully inside the chunk were already matched by the speculative scanner)
        for k in 0..o.start.len() {
            let id = idx.start.len() as u32;
            idx.start.push(o.start[k]); idx.end.push(o.end.get(k).copied().unwrap_or(u64::MAX));
            idx.name.push(o.name[k]); idx.parent.push(open.last().copied().unwrap_or(u32::MAX));
            idx.depth.push(depth as u16);
            if o.end.get(k).copied().unwrap_or(u64::MAX) == u64::MAX { open.push(id); depth += 1; }
        }
        idx.lines.extend(o.lines.iter().copied()); line_base += o.newline_count;
        idx.errors.extend(o.errors.iter().copied());
        crate::journal::append(&idx, o)?; // resumability: after each chunk
    }
    idx.line_count = line_base + 1;
    if !open.is_empty() { idx.errors.push((xmlspy_parse::scanner::ErrorCode::Unclosed as u16, src.len())); }
    src.advise(xmlspy_core::source::AccessHint::Random);
    Ok(idx)
}
~~~

## bench/xmlspy-bench/src/main.rs — corpus generator + harness

~~~rust
use clap::{Parser, Subcommand};
use std::{fs::File, io::{BufWriter, Write}, path::PathBuf, time::Instant};

#[derive(Parser)] struct Cli { #[command(subcommand)] cmd: Cmd }
#[derive(Subcommand)] enum Cmd {
    /// Generate a deterministic PurchaseOrder corpus (e.g. --size 10GiB)
    Gen { #[arg(long)] size: String, #[arg(long)] out: PathBuf, #[arg(long, default_value_t = 42)] seed: u64 },
    /// Build the structural index and report throughput / RSS
    Index { file: PathBuf, #[arg(long, default_value_t = 0)] threads: usize },
    /// Well-formedness only (single thread, streaming)
    Wf { file: PathBuf },
}

fn parse_size(s: &str) -> u64 {
    let (n, unit) = s.split_at(s.find(|c: char| c.is_alphabetic()).unwrap_or(s.len()));
    let n: f64 = n.parse().expect("size");
    (n * match unit { "GiB" => 1u64 << 30, "MiB" => 1 << 20, "KiB" => 1 << 10, _ => 1 } as f64) as u64
}

fn gen(size: u64, out: &PathBuf, seed: u64) -> std::io::Result<()> {
    let mut w = BufWriter::with_capacity(8 << 20, File::create(out)?);
    let mut rng = seed;
    let mut next = || { rng ^= rng << 13; rng ^= rng >> 7; rng ^= rng << 17; rng };
    let mut written = 0u64;
    let head = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<PurchaseOrders xmlns=\"urn:xmlspy:orders\">\n";
    w.write_all(head.as_bytes())?; written += head.len() as u64;
    let mut n = 0u64;
    while written + 64 < size {
        let r = next();
        let rec = format!(
            "  <PurchaseOrder OrderNumber=\"{n}\" OrderDate=\"2026-{:02}-{:02}\" Priority=\"{}\">\n    <Address Type=\"Shipping\"><Name>Customer {n}</Name><City>{}</City><Zip>{}</Zip></Address>\n    <Items><Item PartNumber=\"{}-AA\"><ProductName>{}</ProductName><Quantity>{}</Quantity><USPrice>{}.{:02}</USPrice></Item></Items>\n  </PurchaseOrder>\n",
            1 + r % 12, 1 + (r >> 8) % 28, ["low", "normal", "high"][(r % 3) as usize],
            ["Vienna", "Boston", "Berlin", "Austin"][((r >> 16) % 4) as usize], 10000 + (r >> 20) % 89999,
            r % 900, ["Lawnmower", "Router", "Keyboard", "Webcam"][((r >> 4) % 4) as usize], 1 + r % 5, r % 400, (r >> 3) % 100);
        w.write_all(rec.as_bytes())?; written += rec.len() as u64; n += 1;
    }
    w.write_all(b"</PurchaseOrders>\n")?;
    w.flush()
}

fn rss_mb() -> f64 {
    #[cfg(target_os = "linux")]
    { if let Ok(s) = std::fs::read_to_string("/proc/self/statm") { let pages: f64 = s.split(' ').nth(1).unwrap_or("0").parse().unwrap_or(0.0); return pages * 4096.0 / 1e6; } }
    0.0
}

fn main() -> anyhow::Result<()> {
    match Cli::parse().cmd {
        Cmd::Gen { size, out, seed } => { let t = Instant::now(); gen(parse_size(&size), &out, seed)?; eprintln!("generated {} in {:.1}s", out.display(), t.elapsed().as_secs_f64()); }
        Cmd::Index { file, threads } => {
            if threads > 0 { rayon::ThreadPoolBuilder::new().num_threads(threads).build_global()?; }
            let src = xmlspy_core::mmap::MmapSource::open(&file)?;
            let prog = xmlspy_index::builder::Progress::default();
            let t = Instant::now();
            let idx = xmlspy_index::builder::build(&src, &prog, None)?;
            let secs = t.elapsed().as_secs_f64();
            let mb = src.len() as f64 / 1e6;
            println!("bytes={} elements={} lines={} errors={} time={:.2}s throughput={:.0} MB/s rss={:.0} MB index={} MB",
                     src.len(), idx.start.len(), idx.line_count, idx.errors.len(), secs, mb / secs, rss_mb(), idx.byte_size() >> 20);
            assert!(mb / secs >= 500.0 || rayon::current_num_threads() == 1, "PERF GATE: index throughput < 500 MB/s");
            assert!(rss_mb() < 512.0, "PERF GATE: RSS >= 512 MB");
        }
        Cmd::Wf { file } => {
            let src = xmlspy_core::mmap::MmapSource::open(&file)?;
            let mut sc = xmlspy_parse::scanner::Scanner::new();
            let (mut errs, t) = (0u64, Instant::now());
            for r in xmlspy_core::source::chunks(src.len(), xmlspy_core::source::CHUNK) {
                let b = src.slice(r.clone())?;
                sc.feed(b.as_ref(), r.start, &mut |e| if matches!(e, xmlspy_parse::scanner::Event::Error { .. }) { errs += 1 });
                src.advise(xmlspy_core::source::AccessHint::DontNeed(r));
            }
            sc.finish(src.len(), &mut |_| errs += 1);
            let secs = t.elapsed().as_secs_f64();
            println!("{}: {} error(s), {:.0} MB/s", if errs == 0 { "well-formed" } else { "NOT well-formed" }, errs, src.len() as f64 / 1e6 / secs);
        }
    }
    Ok(())
}
~~~

## crates/xmlspy-ui/src/views/text_view.rs — virtualized Text View (Leptos 0.7)

~~~rust
use leptos::prelude::*;
use crate::model::DocHandle;   // Rc<dyn DocumentModel> + reactive version signal
use crate::highlight::tokenize_line;

const ROW_PX: f64 = 20.0;
const OVERSCAN: u64 = 20;

#[component]
pub fn TextView(doc: DocHandle) -> impl IntoView {
    let (scroll_top, set_scroll_top) = signal(0.0f64);
    let (viewport_h, set_viewport_h) = signal(600.0f64);
    let version = doc.version();                       // Signal<u64>
    let line_count = Memo::new(move |_| { version.get(); doc.line_count() });

    // Visible window — the ONLY lines materialized in the DOM.
    let window = Memo::new(move |_| {
        let first = (scroll_top.get() / ROW_PX).floor() as u64;
        let rows = (viewport_h.get() / ROW_PX).ceil() as u64 + 1;
        let start = first.saturating_sub(OVERSCAN);
        let end = (first + rows + OVERSCAN).min(line_count.get());
        start..end
    });

    // Async line fetch (page cache hit ⇒ resolves within the same frame; miss ⇒ spinner rows).
    let lines = LocalResource::new(move || { let w = window.get(); let d = doc.clone(); async move { d.lines(w).await } });

    let container = NodeRef::<leptos::html::Div>::new();
    Effect::new(move |_| { if let Some(el) = container.get() { set_viewport_h.set(el.client_height() as f64); } });

    view! {
        <div node_ref=container class="textview" role="grid" aria-rowcount=move || line_count.get()
             on:scroll=move |ev| { let t: web_sys::HtmlDivElement = event_target(&ev); set_scroll_top.set(t.scroll_top() as f64); }>
            <div class="spacer" style:height=move || format!("{}px", line_count.get() as f64 * ROW_PX)>
                <div class="rows" style:transform=move || format!("translateY({}px)", window.get().start as f64 * ROW_PX)>
                    <Suspense fallback=|| view! { <div class="row loading"/> }>
                    {move || lines.get().map(|batch| {
                        let start = window.get().start;
                        batch.iter().enumerate().map(|(i, text)| {
                            let n = start + i as u64;
                            view! {
                                <div class="row" role="row" aria-rowindex=n + 1 style:height=format!("{ROW_PX}px")>
                                    <span class="gutter">{n + 1}</span>
                                    <span class="code">{tokenize_line(text).into_iter().map(|t| view! { <span class=t.class>{t.text}</span> }).collect_view()}</span>
                                </div>
                            }
                        }).collect_view()
                    })}
                    </Suspense>
                </div>
            </div>
        </div>
    }
}
~~~

## Tests (excerpt) — crates/xmlspy-parse/tests/conformance.rs

~~~rust
//! Runs the W3C XML Conformance Test Suite (xmlconf) not-wf + valid cases through the scanner.
//! Expectation files live in conformance/xmlconf/known-failures.toml (must only shrink).
#[test]
fn xmlconf_not_wf_are_rejected() {
    let suite = xmlconf::load("conformance/xmlconf/xmlconf.xml").expect("suite (git-lfs)");
    let mut failures = vec![];
    for case in suite.cases().filter(|c| c.kind == xmlconf::Kind::NotWf && c.edition_ok(5)) {
        let bytes = std::fs::read(&case.path).unwrap();
        let mut sc = xmlspy_parse::scanner::Scanner::new();
        let mut errs = 0;
        // feed in 7-byte chunks to exercise every resume boundary
        let mut off = 0u64;
        for chunk in bytes.chunks(7) { sc.feed(chunk, off, &mut |e| if matches!(e, xmlspy_parse::scanner::Event::Error{..}) { errs += 1 }); off += chunk.len() as u64; }
        sc.finish(off, &mut |_| errs += 1);
        if errs == 0 { failures.push(case.id.clone()); }
    }
    let known = xmlconf::known_failures("conformance/xmlconf/known-failures.toml");
    let new: Vec<_> = failures.iter().filter(|f| !known.contains(*f)).collect();
    assert!(new.is_empty(), "new conformance regressions: {new:?}");
}

#[test]
fn chunk_boundaries_do_not_change_results() {
    let doc = include_bytes!("fixtures/orders.xml");
    let full = scan_all(doc, doc.len());
    for chunk in [1usize, 2, 3, 5, 7, 64, 4096] { assert_eq!(scan_all(doc, chunk), full, "chunk={chunk}"); }
}
~~~
`;
