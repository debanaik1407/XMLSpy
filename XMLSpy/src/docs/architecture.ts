export const ARCHITECTURE_MD = String.raw`
# 1. System Architecture

XMLSpy-rs is a Cargo workspace of ~26 crates. Everything performance-critical is Rust; the UI is Rust compiled to WebAssembly (Leptos 0.7, CSR mode) with a ~9 KB JS interop shim for the browser APIs Rust cannot reach directly (File System Access API, Web Workers with SharedArrayBuffer, Clipboard, IndexedDB/OPFS).

## 1.1 Component diagram

~~~text
┌──────────────────────────────────────────────────────────────────────────────────────┐
│ BROWSER TAB (CSP: default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; COOP/COEP) │
│                                                                                      │
│  ┌──────────────── UI THREAD ────────────────┐   ┌────────── WORKER POOL (n = cores) ─┐ │
│  │ xmlspy-ui (Leptos/WASM)                   │   │ xmlspy-worker (WASM, wasm-bindgen- │ │
│  │  • Shell: menus/toolbars/dock manager     │   │  rayon on SharedArrayBuffer)       │ │
│  │  • Views: Text · Grid · Schema · Authentic│◄─►│  • parse / index / validate        │ │
│  │    Browser · WSDL · XBRL · JSON Grid       │   │  • search / XPath / XSLT / XQuery  │ │
│  │  • Virtualized renderers (only visible)   │   │  • diff / codegen / charts         │ │
│  │  • DocumentHandle (Rc<RefCell<..>>)       │   │  • Job queue, cancel tokens,       │ │
│  │  • Command bus (every action = Command)   │   │    progress channel (postMessage)  │ │
│  └───────────────┬───────────────────────────┘   └───────────────┬────────────────────┘ │
│                  │ js-interop (≈9 KB TS)                          │                      │
│  ┌───────────────▼────────────────────────────────────────────────▼────────────────────┐ │
│  │ STORAGE LAYER: FileSystemFileHandle (read slices) · OPFS (structural index, journal) │ │
│  │ IndexedDB (sessions, settings, snippets) · Cache Storage (WASM bundle)               │ │
│  └─────────────────────────────────────────────────────────────────────────────────────┘ │
└───────────────────────────────────────┬──────────────────────────────────────────────────┘
                                        │ HTTPS / WebSocket / gRPC-web (optional offload)
        ┌───────────────────────────────┼─────────────────────────────────────────────┐
        │  NATIVE SIDE (single static binary per role, all share the same core crates) │
        │                                                                             │
        │  xmlspy-web  (axum 0.8)   serves WASM bundle, REST+WS command API, file gate │
        │  xmlspy-server (engine)   RaptorXML-style job server: /v1/validate|xslt|…    │
        │  xmlspy-lsp  (tower-lsp)  XML/XSD/XSLT/XQuery/JSON language server           │
        │  xmlspy-cli  (clap)       headless: validate, transform, diff, gen, script   │
        │  xmlspy-desktop (Tauri 2) same UI + native fs/mmap/threads/DB sockets        │
        │                                                                             │
        │  CORE CRATES: core · parse · index · xsd · dtd · schematron · xpath · xslt · │
        │  xquery · xbrl · wsdl-soap · avro · json · yaml · diff · dsig · codegen ·     │
        │  db · charts · http · plugin-api · script (rhai)                             │
        └─────────────────────────────────────────────────────────────────────────────┘
~~~

## 1.2 Data flow: opening, editing, saving a 10 GB file

~~~text
OPEN
 1. User picks file → FileSystemFileHandle (browser) or PathBuf (native).
 2. Core creates ByteSource:
      native  → memmap2::Mmap (MAP_PRIVATE, madvise SEQUENTIAL during index, RANDOM after)
      browser → SliceSource { handle, page_cache: LRU<page_no, Bytes> } (Blob.slice, 8 MiB pages)
 3. Index probe: OPFS/<sha1(path|size|mtime)>.xsi  exists & valid header? → mmap it, DONE in <50 ms.
    else: spawn IndexBuild job on worker pool. Job is resumable (see §3.5).
 4. UI thread immediately renders Text View from *partial* index: the first 8 MiB page is
    scanned synchronously on the UI thread (≈6 ms) giving exact line offsets for the first
    ~200k lines. First screen paints in <100 ms; scrollbar length is estimated
    (bytes / avg_line_len) and corrected as checkpoints arrive.
 5. Progress events (every 80 ms): bytes, lines, elements, errors → status bar + Messages.

EDIT
 6. Keystroke → Command::InsertText{pos, text} → EditBuffer (ropey Rope of *pieces*, not bytes:
    each leaf is Piece{Original{off,len} | Added{off,len}}) → O(log n) splice.
 7. Incremental re-index: only the enclosing element range (from the structural index) is
    re-scanned on the UI thread if <256 KiB, otherwise dispatched to a worker. Index nodes
    after the edit get a delta via a Fenwick tree over chunk byte-deltas (no rewrite).
 8. Views observe DocumentModel::version; each view re-queries only its visible range.

SAVE
 9. Save = streamed diff-apply: iterate pieces in document order; Original pieces are
    copied with copy_file_range/sendfile (native) or Blob.slice parts (browser);
    Added pieces are written from the add-buffer. Write to  <name>.xmlspy-tmp  then
    atomic rename (native) / FileSystemWritableFileStream::close (browser).
10. Index is patched (offsets shifted by deltas) and re-persisted; journal truncated.
~~~

## 1.3 The shared DocumentModel abstraction (used by every view)

~~~rust
/// Every view (Text, Grid, Schema, Authentic, Diff, XBRL…) talks ONLY to this trait.
pub trait DocumentModel: Send + Sync {
    fn version(&self) -> u64;                      // bumped on every edit
    fn len_bytes(&self) -> u64;
    fn line_count(&self) -> u64;
    fn lines(&self, range: Range<u64>) -> Result<LineBatch>;      // virtualized text
    fn byte_to_pos(&self, off: u64) -> Result<Pos>;                // line/col mapping
    fn pos_to_byte(&self, p: Pos) -> Result<u64>;
    fn node(&self, id: NodeId) -> Result<NodeRecord>;              // structural index row
    fn children(&self, id: NodeId, window: Range<u32>) -> Result<Vec<NodeId>>; // lazy DOM
    fn subtree(&self, id: NodeId, budget: usize) -> Result<Arc<LazyTree>>;   // LRU-evicted
    fn apply(&mut self, edit: Edit) -> Result<EditReceipt>;        // rope splice + reindex
    fn query(&self, q: Query, cancel: CancelToken, prog: Progress) -> JobHandle<QueryResult>;
    fn diagnostics(&self) -> &DiagnosticSet;                       // WF/XSD/Schematron/…
}
~~~

Views map to the model as follows:

| View | Model calls | Virtualization unit |
|---|---|---|
| Text | lines(), byte_to_pos(), diagnostics() | visible lines ± 20 overscan |
| Grid | node(), children(window), subtree() | expanded rows (flattened tree), 40 px rows |
| Schema Designer | separate SchemaModel (xmlspy-xsd), linked by NodeId for instance ↔ schema jumps | visible diagram tiles |
| Authentic | subtree() + SPS template compiled to a layout tree | layout blocks in viewport |
| Tree/Outline | children(window) | expanded rows |
| Diff | two DocumentModels + xmlspy-diff EditScript stream | hunks in viewport |

# 2. Cargo Workspace Layout

~~~text
xmlspy-rs/
├─ Cargo.toml                   # [workspace] resolver = "2", shared [workspace.dependencies]
├─ rust-toolchain.toml          # 1.85 stable; nightly only for -Z build-std=wasm32 threads
├─ crates/
│  ├─ xmlspy-core        DocumentModel, EditBuffer (ropey pieces), ByteSource, LRU, Command bus, CancelToken/Progress, error types
│  ├─ xmlspy-parse       SIMD streaming tokenizer (portable_simd + std::arch fallbacks), UTF-8/UTF-16 decoders, entity decoder, XXE-safe (no external entity fetch by default)
│  ├─ xmlspy-index       .xsi structural index format, builder (rayon), resumable journal, incremental patch (Fenwick deltas), line table
│  ├─ xmlspy-xsd         XSD 1.0/1.1 schema model, loader, validator (streaming, PSVI), SmartFix engine, sample-gen, flatten/subset
│  ├─ xmlspy-dtd         DTD parser/validator, DTD⇄XSD
│  ├─ xmlspy-schematron  ISO Schematron (XSLT/XPath bindings via xmlspy-xslt), SVRL output
│  ├─ xmlspy-xpath       XPath 1.0/2.0/3.1 parser, static typing, XDM, function library (F&O 3.1), index-backed + streaming evaluators
│  ├─ xmlspy-xslt        XSLT 1.0/2.0/3.0 compiler → IR, streaming (xsl:stream/xsl:iterate, guaranteed-streamable analysis §19), debugger hooks, profiler, Speed Optimizer
│  ├─ xmlspy-xquery      XQuery 1.0/3.1 + Update Facility 1.0/3.0, shares XDM/F&O with xpath
│  ├─ xmlspy-xbrl        XBRL 2.1, Dimensions 1.0, Formula 1.0, Table 1.0, iXBRL 1.1, XULE, taxonomy packages, DQC/EFM rules
│  ├─ xmlspy-wsdl-soap   WSDL 1.1/2.0 model, SOAP 1.1/1.2 client, WS-Security (UsernameToken, X.509 sig via dsig), proxy debugger
│  ├─ xmlspy-avro        .avsc model, container decoder (apache-avro crate), evolution checks
│  ├─ xmlspy-json        JSON5/JSONC parser (streaming, index-backed like XML), JSON Schema draft-04…2020-12, OpenAPI 3.x, JSON⇄XML rules, JSONPath
│  ├─ xmlspy-yaml        YAML 1.2 (serde_yml + location-aware parser), YAML⇄JSON
│  ├─ xmlspy-diff        structural XML/JSON diff (XyDiff-style hashing + Myers on child sequences), 3-way merge, dir compare
│  ├─ xmlspy-dsig        XML-DSig core, C14N 1.0/1.1/Exclusive, XAdES-BES/EPES; crypto via RustCrypto (rsa, p256/p384, hmac, sha2, x509-cert)
│  ├─ xmlspy-codegen     XSD/JSON-Schema → C++/C#/Java/Rust/TypeScript via minijinja templates (SPL-compatible tags)
│  ├─ xmlspy-db          Connectors: sqlx (Postgres/MySQL/SQLite), tiberius (SQL Server), oracle, odbc-api (DB2/others); XML column editing; schema⇄DDL
│  ├─ xmlspy-charts      Chart model → SVG (plotters-svg) + PNG (resvg), wizard → XSLT/XQuery codegen
│  ├─ xmlspy-http        HTTP client (reqwest), OAuth2, environments, collections, mock server (axum) from OpenAPI
│  ├─ xmlspy-script      Rhai scripting/macro engine + WASM plugin host (wasmtime native / wasm-in-wasm via component model shim in browser)
│  ├─ xmlspy-plugin-api  stable ABI (WIT) for custom views, validators, connectors
│  ├─ xmlspy-lsp         tower-lsp server
│  ├─ xmlspy-cli         clap binary
│  ├─ xmlspy-server      engine server (axum + tokio, job queue, gRPC via tonic)
│  ├─ xmlspy-web         static host + REST/WS command API
│  ├─ xmlspy-worker      wasm32 worker entry: job dispatcher over SharedArrayBuffer ring buffer
│  ├─ xmlspy-ui          Leptos frontend (views, dock manager, keymap, i18n via fluent)
│  └─ xmlspy-desktop     Tauri 2 shell (native fs, sockets, mmap)
├─ bench/  xmlspy-bench  criterion + custom harness, corpus generator, regression gates
├─ conformance/          W3C XML TS, XSD TS, QT3, XSLT 3.0 TS, XBRL suites (git-lfs, gated CI job)
└─ fuzz/                 cargo-fuzz targets: parse, index, xpath, xslt, json, yaml, avro, c14n
~~~

Dependency direction is strictly downward: ui/worker/server/cli → feature crates → xpath/xslt → parse/index → core. No feature crate depends on another feature crate except through traits in xmlspy-core (e.g. Schematron uses xslt through core::XsltEngine trait so it can also be served by a remote engine).

# 3. Large-File Pipeline — Detailed Design

## 3.1 ByteSource and chunking

* Page size 4 KiB, chunk = 2048 pages = **8 MiB**. Every chunk boundary is page-aligned so mmap faults and Blob.slice reads never straddle.
* Native: memmap2::MmapOptions::new().map(&file) once; the indexer walks it with rayon::scope chunks; madvise(MADV_SEQUENTIAL) during indexing, MADV_RANDOM afterwards, MADV_DONTNEED on evicted ranges (keeps RSS < 512 MB on 10 GB files; verified with /proc/self/smaps in bench).
* Browser: the page cache holds ≤ 32 pages (256 MiB hard cap, 64 MiB default) as ArrayBuffers in a LRU keyed by page number; each view request is translated into page fetches (Blob.slice(...).arrayBuffer()) that run in parallel (Promise.all, max 4 in flight).
* Chunk carry-over: a tag can straddle a chunk boundary, so the scanner is a resumable state machine (no lookahead > 1 byte except the 'CDATA[' and BOM prefixes which are handled with an explicit match counter). Parallel indexing (§3.3) uses a speculative start state per chunk.

## 3.2 Structural index file format (.xsi, little-endian, mmap-able)

~~~text
Header (64 B)   magic "XSI\0" u32 | version u16 | flags u16 | file_len u64 | file_mtime_ns u64
                | file_hash_prefix u64 (xxh3 of first+last 1 MiB) | node_count u64 | line_count u64
                | section_dir_off u64 | crc32 u32 | pad
Section dir     [ {kind u16, pad u16, off u64, len u64} ] × n

NODES  (Struct-of-Arrays; each array is a separate section so views map only what they need)
  start   : u40 packed (5 B)  byte offset of '<'          → 10 GB ⇒ 2^40 ok
  end     : u40 packed         byte offset after '>' of end tag
  name_id : u24 packed (3 B)   into NAMES
  depth   : u16
  parent  : u32 (node id)      preorder ⇒ children are contiguous ranges → no child pointers stored;
  first_child_hint: u32        nearest descendant id (== id+1 or NONE), sibling found by end-offset skip
  flags   : u8                 has_attrs | has_text | mixed | is_empty | in_cdata | has_ns_decl
  = 20 B / element  ⇒ 10 GB file with ~120 M elements ⇒ 2.4 GB on disk, mmapped, never fully resident.

LINES   checkpoint every 64th line: u40 byte offset ⇒ 100 M lines ⇒ 7.8 MB.
        Exact line for any offset = binary-search checkpoint + scan ≤ 64 lines (≤ few KiB).
NAMES   deduplicated QName table: [u32 len-prefixed UTF-8], typically < 64 KiB.
NSDECL  (node_id u32, prefix_name_id u24, uri_name_id u24) — for in-scope namespace resolution.
ERRORS  well-formedness diagnostics (offset u40, code u16, arg_name_id u24) capped at 100k.
JOURNAL append-only: {chunk_no u32, state u8, partial_stack_len u16, ...} written after each chunk
        ⇒ resumable after crash (§3.5). DELTAS: Fenwick tree over 8 MiB buckets of byte deltas
        applied by edits so offsets stay valid without rewriting the NODES section.
~~~

Persistence location: native → next to the file as .<name>.xsi (fallback $XDG_CACHE_HOME/xmlspy/index/) ; browser → OPFS /index/<hash>.xsi.

## 3.3 Index builder (rayon, parallel, single pass)

1. File split into 8 MiB chunks; each rayon task tokenizes its chunk **speculatively** from state TEXT, recording (a) local events, (b) the "unknown prefix" up to the first unambiguous sync point ('<' at depth-independent position after a '>' outside quotes), (c) local depth delta and min-depth.
2. A serial fix-up pass (like simd-json's / parallel-bracket-matching) joins chunks: parent ids and absolute depths are computed by prefix-sum over per-chunk depth deltas; unmatched end tags at chunk heads are resolved against the previous chunk's open stack. Throughput target: 1.2 GB/s on 8 cores (native), 140–220 MB/s in a single browser worker (JS scanner in this demo), 400–600 MB/s with the WASM SIMD scanner across 4 workers.
3. Serial well-formedness verdict: after fix-up, mismatched names are reported with exact offsets; the whole pass is one read of the file (true single-pass, streaming).

## 3.4 Edit buffer

* Rope of Pieces: ropey stores a Vec<Piece> equivalent via a custom B-tree (xi-rope style) with summary = (bytes, newlines, elements_started). Original pieces reference the ByteSource; Added pieces reference an append-only Vec<u8> (also memory-mapped to a spill file when > 64 MiB).
* Complexity: insert/delete O(log n) tree ops; cursor↔offset O(log n) via summaries.
* Undo/redo: persistent tree snapshots (structural sharing) — each snapshot costs O(log n) nodes.
* Coalescing: consecutive typing coalesces into one Added piece; snapshots are taken on 500 ms idle or on structural commands.

## 3.5 Crash / recovery

* Index journal: after every chunk the builder appends a journal record; on reopen with a matching (len, mtime, hash) but an incomplete NODES section, the build resumes from the last journaled chunk with its serialized scanner state and stack.
* Edit journal (WAL): every Edit is appended to <hash>.wal (OPFS/IndexedDB in browser) before being applied; on restart the user is offered "Recover unsaved changes" (XMLSpy behaviour on restart after crash).
* Saves are atomic (tmp + rename / writable stream close); the index is updated *after* the rename and revalidated by hash prefix on next open.

## 3.6 Cache eviction

| Cache | Key | Capacity | Policy |
|---|---|---|---|
| Page cache | page_no | 64 MiB (tunable ≤ 256 MiB) | LRU; pinned while a view request is in flight |
| Decoded line blocks | checkpoint block | 512 blocks (~16k lines) | LRU |
| LazyTree subtrees (Grid) | node_id | 20k nodes or 32 MiB | LRU with size accounting; expanded rows pinned |
| XPath partial results | (expr, version) | 32 entries | LRU, invalidated on version bump |

## 3.7 Benchmark methodology (bench/xmlspy-bench)

* Corpus: gen.rs produces 1/10/100 MiB, 1/2/10 GiB PurchaseOrder documents (record templates, deterministic seed) plus pathological cases (single 2 GiB line; 50 M sibling elements; depth 100k).
* Metrics: open-to-first-paint (wall clock from file pick to first frame with real lines), index throughput MB/s, peak RSS (getrusage / performance.measureUserAgentSpecificMemory), frame time p50/p99 while scripted scrolling (Playwright + rAF timestamps), search MB/s, save time.
* Gates (CI fails on regression > 5%): index ≥ 500 MB/s native single-threaded; open 10 GiB < 2 s; p99 frame < 16 ms; RSS < 512 MB; WF check 10 GiB < 20 s.

# 4. Third-party crate selection

| Concern | Crate (version) | Why |
|---|---|---|
| mmap | memmap2 0.9 | maintained, cross-platform, unsafe surface is one audited module |
| Streaming XML | quick-xml 0.37 (bootstrap + tests) → custom xmlspy-parse | quick-xml is fast but not resumable across chunk boundaries nor SIMD; we keep it as an oracle in tests |
| SIMD | std::simd (portable_simd, nightly-gated feature) + std::arch fallbacks | classify '<', '>', '&', '"', '\n' 64 bytes per op (simd-json's structural-index technique) |
| Rope | ropey 1.6 (custom piece leaves) | proven B-tree rope; xi-rope is unmaintained |
| Parallelism | rayon 1.10, wasm-bindgen-rayon 1.2 | same code path native & browser (SharedArrayBuffer) |
| Async | tokio 1.x (native), wasm-bindgen-futures (browser) | |
| Web server | axum 0.8 + tower-http | ergonomic, hyper 1.x, small binary |
| gRPC | tonic 0.12 | engine server offload |
| UI | Leptos 0.7 (CSR) | fine-grained reactivity = cheapest virtualized lists among Rust UI frameworks; small bundle |
| Serialization | serde 1, rkyv 0.8 for index headers (zero-copy) | |
| JSON | simd-json 0.14 (tape) for < 256 MiB, custom streaming for larger; serde_json for config | |
| YAML | serde_yml + our own location-aware parser | positions needed for diagnostics |
| Avro | apache-avro 0.17 | official |
| DB | sqlx 0.8, tiberius 0.12, oracle 0.6, odbc-api 8 | native/Tauri only |
| HTTP client | reqwest 0.12 (rustls) | wasm + native |
| Crypto | rsa 0.9, p256/p384 0.13, hmac/sha2 0.10, x509-cert 0.2, pkcs8 | pure-Rust, wasm-friendly |
| Charts | plotters 0.3 + plotters-svg, resvg 0.44 for PNG | |
| Templates | minijinja 2 | codegen templates, sandboxed |
| Scripting | rhai 1.19 | safe, embeddable, wasm-capable |
| Plugins | wasmtime 27 (native) / wasm component shim (browser) | |
| LSP | tower-lsp 0.20 | |
| CLI | clap 4 | |
| Errors/logging | thiserror 2, tracing 0.1, tracing-wasm | |
| Testing | proptest 1.5, cargo-fuzz, criterion 0.5, insta | |

## 4.1 Written from scratch (no adequate crate) + conformance strategy

| Component | Reason | Conformance suite / target |
|---|---|---|
| Resumable SIMD XML tokenizer | quick-xml/xmlparser are not chunk-resumable nor parallel | W3C XML Conformance TS (xmlconf 20130923): 100% of valid/not-wf for XML 1.0 5th ed. |
| XSD 1.0/1.1 validator | no Rust XSD 1.1 (assertions, CTA, open content) | W3C XSD 1.1 TS (msxsdtest/nist/sun/boeing/ibm sets): ≥ 99% pass, tracked per-testset |
| XPath 3.1 / XQuery 3.1 / XSLT 3.0 | xrust is incomplete (no streaming, partial F&O) | QT3 (XPath/XQuery 3.1) ≥ 99.5%; XSLT 3.0 TS ≥ 98%; XQUF tests |
| Schematron | none maintained | schxslt testsuite |
| XBRL 2.1/Dimensions/Formula/Table/iXBRL | none | XBRL International conformance suites (XBRL-CONF-2014, XDT-CONF, FORMULA-CONF, TABLE-CONF, iXBRL-CONF 1.1) |
| XML-DSig + C14N | xml-c14n crates incomplete | W3C xmldsig interop vectors, C14N 1.1/EXC-C14N test vectors |
| Structural diff/merge | none | our own golden corpus + property tests (diff(a,b) applied to a == b) |
| WSDL/SOAP model | none | WS-I Basic Profile 1.2 test assertions |
| JSON Schema 2020-12 | jsonschema crate is good for validation but lacks graphical model/OpenAPI dialect; we wrap it and add the model | JSON-Schema-Test-Suite |

# 6. Coding Standards

* Edition 2021, MSRV 1.85; #![forbid(unsafe_code)] in every crate except xmlspy-core::mmap and xmlspy-parse::simd (each unsafe block has a // SAFETY: comment and a Miri/fuzz target).
* No panics on input: clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing = deny in parser/validator crates; all APIs return Result<T, XError> where XError is a thiserror enum per crate with #[source] chains and a stable ErrorCode (used by SmartFix and LSP).
* tracing spans on every job boundary (parse.chunk, index.build, xslt.template…) with byte counts as fields; tracing-wasm in browser, OTLP in server.
* Tests: unit + proptest strategies (arbitrary well-formed XML generator, mutation-based not-wf generator), cargo-fuzz targets for every parser (parse, index, xpath lexer/parser, xslt compiler, json, yaml, avro, c14n) run 15 min per PR and 24 h nightly; criterion benches with regression gate (critcmp, 5 %); cargo-llvm-cov ≥ 80 % on core/parse/index/xpath/xsd.
* API style: builder pattern for engines, Arc<dyn DocumentModel> handles, CancelToken + Progress on every long op, no global state, feature flags per format (e.g. --features xbrl,db).

# 7. UI/UX Specification (XMLSpy layout in the browser)

* Frame: Title/menu bar (File · Edit · Project · XML · DTD/Schema · Schema design · XSL/XQuery · Authentic · DB · Convert · View · Browser · WSDL · SOAP · XBRL · Tools · Window · Help) → toolbars (Standard, Text, Grid, Schema, XSL, Authentic; hide-able, draggable) → main MDI tab strip → editing area with bottom view tabs (Text · Grid · Schema/WSDL · Authentic · Browser) → status bar (Ln/Col/Sel · encoding · line-ending · file size · element count · index state · memory · engine).
* Dock windows (all dockable/auto-hide/floating, layout persisted): Project (left), Info (left-bottom), Entities/Elements/Attributes helper windows (right, Text/Grid/Authentic), Schema Components (right, Schema view), Messages, Find in Files, XPath/XQuery, Charts, Database Query, HTTP, XSLT Debugger/Profiler, XBRL (bottom).
* Keyboard map (XMLSpy-compatible where documented): Ctrl+N new · Ctrl+O open · Ctrl+S save · Ctrl+Shift+S save as · Ctrl+P print · Ctrl+Z/Y undo/redo · Ctrl+F find · F3 find next · Ctrl+H replace · Ctrl+G go to line · Ctrl+Shift+F find in files · F7 check well-formedness · F8 validate · Ctrl+F8 validate on server · F10 XSL transformation · Ctrl+F10 XSL:FO · Ctrl+Shift+F10 XQuery · Alt+F8 assign schema · Ctrl+Shift+P pretty-print (via XML menu) · Ctrl+Alt+N/F/T/U/A/B switch Text/Grid/Schema/… (adapted: Ctrl+1…6 in browser since Ctrl+Alt is intercepted on some OSes) · F2 rename node (Grid) · Ins insert sibling · Ctrl+Ins append · Ctrl+Shift+Ins add child · Ctrl+I toggle collapse · Ctrl+Shift+E evaluate XPath · F5 refresh · F9 breakpoint · F11 step into · F10 step over · Shift+F11 step out · Esc cancel job.
* View switching: tabs at the bottom of the document pane exactly like XMLSpy; the active view is remembered per document; switching never re-parses — all views read the same DocumentModel version; if the document is not well-formed, Grid/Schema/Authentic show XMLSpy's "not well-formed" barrier with a jump-to-error button.
* Accessibility: every virtualized list is role="grid"/"tree" with aria-rowcount/aria-rowindex; focus is managed on the virtual cursor; all commands reachable from the menu with mnemonics; themes light/dark/high-contrast via CSS variables; strings through fluent (i18n).

# 8. Browser-sandbox limitations and Tauri fallbacks

| Feature | Browser | Fallback (Tauri desktop / engine server) |
|---|---|---|
| True mmap, madvise, copy_file_range | ✗ (Blob.slice + page cache instead) | Tauri: native memmap2 |
| Persisted index next to file | OPFS only (quota-bound, ~⅓ disk) | Tauri: .xsi next to file |
| Database sockets (Postgres/Oracle/DB2/SQL Server) | ✗ (no raw TCP) | Tauri direct; browser via xmlspy-server /v1/db proxy |
| FTP/WebDAV/arbitrary URL open (CORS) | limited | Tauri or server proxy |
| SOAP intercepting proxy | ✗ | xmlspy-server proxy module |
| Git status | ✗ (no fs), use isomorphic model only | Tauri: git2 |
| File watching/auto-reload | ✗ (polling FileSystemFileHandle.getFile().lastModified) | Tauri: notify crate |
| Multi-threaded WASM | needs COOP/COEP headers (served by xmlspy-web); falls back to 1 worker + async yields | native threads |
| Print to PDF (schema docs) | browser print dialog | Tauri: headless printpdf |
| Memory > 4 GiB per tab | ✗ (wasm32 limit; we never need it by design) | native |
`;
