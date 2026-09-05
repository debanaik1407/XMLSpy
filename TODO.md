# XMLSpy-rs — Project Status & TODO

**Last audited:** 2026-09-05 · **Branch:** `arena/01a06f92-xmlspy` · **Commit base:** `1da92ad`
**Build state:** ✅ `npm run build` succeeds (single file, 490 KB) · ✅ `npx tsc --noEmit` clean ·
✅ `cargo test` 41 tests / `cargo clippy -D warnings` clean · ✅ `npm run test:parity` 84/84 ·
⚠️ still no browser (Playwright) tests, no CI, no linter

---

## 1. Executive summary

The delivered artefact is a **browser-native XMLSpy-style IDE** (React 19 + TypeScript + Vite 7 +
Tailwind 4) whose XML engine is **written in Rust and executed as WebAssembly in the browser**,
with the original TypeScript engine retained as a byte-for-byte-equivalent fallback.

**2026-09-05 update — the Rust engine is real.** `rust/` is a five-crate Cargo workspace with no
external dependencies that compiles to both `wasm32-unknown-unknown` (68 KB, inlined in the
bundle as base64) and a native `xmlspy` CLI. Scanning, the structural index and the streaming
Find all run in it; the status bar shows **⚙ Rust/WASM** or **⚙ TS fallback**. Measured on a
64 MiB corpus, single thread: 301 MB/s native, **252 MB/s in WASM**, 65 MB/s for the TypeScript
scanner — a 3.9× speed-up for the shipped path. `src/docs/rustCode.ts` (Ctrl+5) is still the
Phase 0/1 *design* listing and is now labelled as such; where it disagrees with `rust/`, the
crates win.

| Area | State | Est. complete |
| --- | --- | --- |
| Streaming scanner + structural index (TS) | Working | **90 %** |
| Web Worker pipeline (chunked scan, search, cancel) | Working | **85 %** |
| Document model (paging, LRU, overlay, streamed save) | Working | **80 %** |
| Text View (virtualized, multi-GB) | Working, thin editor | **65 %** |
| Grid View | Working, read-only | **55 %** |
| Schema View (schema-from-instance) | Working, inference-only | **40 %** |
| Browser View (XSLT 1.0) | Working, small docs only | **35 %** |
| Find / Search | Literal streaming only | **40 %** |
| XPath | Namespace-aware native 1.0 (small) + index subset (large) | **40 %** |
| Validation (XSD/DTD) | **Not started** | **0 %** |
| SmartFix | 10 syntactic fixes, no schema-aware fixes | **35 %** |
| Architecture & roadmap docs | Complete | **100 %** |
| Rust engine — scanner, index, `.xsi`, WASM, CLI | **Working, shipped in the browser** | **60 %** |
| Rust engine — parallel scan, mmap, edit rope, server | Not started | **0 %** |
| Tests (Rust unit/conformance + engine parity) | 41 Rust tests + 84 parity checks | **45 %** |
| CI / lint / release pipeline | **Not started** | **0 %** |

**Menu coverage:** ~40 of ~100 menu commands are wired to real behaviour; the other ~60 are
labelled with a `Phase N` badge and log a "scheduled for Phase N" message.
Breakdown of unwired items: Phase 1 → 3, Phase 2 → 3, Phase 3 → 17, Phase 4 → 13, Phase 5 → 13, Phase 6 → 11.

**Code size:** ~6,000 lines. Engine ≈ 2,185 · Components ≈ 2,282 · Docs strings ≈ 1,275 · CSS 248.

---

## 2. ✅ Completed and working

### 2.1 Engine — `src/engine/`

- [x] **`scanner.ts` (608 L)** — single-pass, chunk-resumable well-formedness scanner + structural indexer.
  - [x] 25-state byte-level machine (`S_BOM`, `S_TEXT`, `S_LT`, `S_STARTNAME`, `S_INTAG`, `S_ATTRNAME`,
        `S_ATTRVAL`, `S_EMPTY`, `S_ENDNAME`, `S_PI`, `S_COMMENT*`, `S_CDATA*`, `S_DOCTYPE`, `S_REF`, …)
  - [x] Resumable across chunk boundaries (state, depth stack, line/col carried between `feed()` calls)
  - [x] Never materializes a DOM; memory is O(index), not O(file)
  - [x] Spec-cited diagnostics (XML 1.0 §2.1/§2.5/§3.1/§4.1 with production numbers)
  - [x] Detects: mismatched end tag, stray end tag, unclosed elements at EOF, duplicate attribute
        (WFC: Unique Att Spec), unquoted attribute value, bare `&`, malformed char refs, `--` in
        comments, `]]>` in content, `<` in text, EOF inside markup, missing root
  - [x] SmartFix suggestion string attached to each error
  - [x] Typed-array index: `elemStart`/`elemEnd`/`elemLine`/`elemParent`/`elemName`/`elemDepth` + name table
  - [x] Sparse line-checkpoint table (`stride`, default every 32nd line)
  - [x] Depth/element budget (`maxIndexed`, default 300 000) so browser memory stays bounded
  - [x] BOM handling
- [x] **`worker.ts` (287 L)** — Blob-URL Web Worker (built via `Function.toString()`, no separate bundle entry).
  - [x] 8 MiB page-aligned `Blob.slice` chunks (browser analogue of mmap)
  - [x] Progress messages ~every 80–100 ms (bytes, MB/s, lines, elements, errors)
  - [x] Cooperative cancellation (Esc) leaving a usable partial index
  - [x] Streaming byte-level literal search with `qlen-1` carry-over across chunks,
        ASCII case-insensitive folding, line/col computation, preview extraction, hit cap
- [x] **`document.ts` (449 L)** — the shared `XmlDocument` model behind every view.
  - [x] Dual backend: piece-table (< 16 MiB) / on-demand paging + line overlay (≥ 16 MiB)
  - [x] `getLines(from,to)` via checkpoint → block decode → LRU (512 blocks × 32 lines)
  - [x] Lazy structural navigation: `rootIds`, `childrenOf` (cached, 4096 entries), `hasChildren`,
        `nodeInfo` (attrs, text, byte range, depth), `pathOf` (XPath with positional predicates)
  - [x] `toBlob()` streamed save — untouched ranges pass through as `Blob` slices (zero-copy),
        only dirty checkpoint blocks are rebuilt
  - [x] `commitEdits()` + `replaceIndex()` for re-index after edit
  - [x] Line-level undo/redo stacks, dirty flag, live stats (index bytes, scan MB/s, first-paint ms)
  - [x] Progressive open: 2 MiB provisional head index for instant first paint, swapped when the worker finishes
- [x] **`pieceTable.ts` (162 L)** — original/add buffers, `insert`/`delete`/`replace`/`slice`,
      cached line-start table, `setLine`/`insertLineAfter`/`deleteLine`, chunked serialization
- [x] **`xpath.ts` (399 L)** — namespace-aware native XPath 1.0 (DOMParser + `document.evaluate` with a
      real `XPathNSResolver`) for in-memory docs; index-backed evaluator for large docs
      (`/a/b`, `//b`, `*`, `[n]`, `count()`, `@attr`). "Match default namespace" binds unprefixed
      *element* names to the document's `xmlns=` (attributes deliberately stay unprefixed);
      strict mode keeps literal XPath 1.0 semantics and explains empty results.
      Covered by `npm run test:xpath` (97 checks)
- [x] **`schemaInfer.ts` (151 L)** — schema-from-instance: element inventory, parent/child sets,
      attribute inventory, primitive type guessing + merging, XSD 1.0 emission (Venetian Blind)
- [x] **`highlight.ts` (168 L)** — per-line tokenizer with comment/CDATA state carry-over,
      10 token classes, `prettyPrint()`
- [x] **`corpus.ts` (193 L)** — 3 sample documents (valid, deliberately broken, XSD) +
      synthetic corpus generator producing 10 MiB … 2 GiB blobs from a shared ~1 MiB template (~1 MiB heap)

### 2.2 UI — `src/App.tsx` + `src/components/`

- [x] **Shell**: menu bar (15 menus), toolbar, document tabs, view tabs, dockable bottom panel, status bar
- [x] **Text View (253 L)**: virtualization with overscan; scroll-height scaling past the browser's
      pixel ceiling (`MAX_SCROLL_PX = 6 M`, wheel-driven paging beyond it); error gutter markers;
      minimap with error marks + viewport indicator; syntax colouring; keyboard nav
      (↑↓, PgUp/PgDn, Ctrl+Home/End); inline single-line edit (F2 / Enter / double-click)
- [x] **Grid View (330 L)**: lazy hierarchical tree over the index, virtualized rows,
      expand/collapse, **table mode** for repeating siblings with column sort and inline filter,
      jump-to-line, selection → Info panel
- [x] **Schema View (162 L)**: component navigator, content-model diagram, attribute/type table,
      "export as XSD" into a new tab
- [x] **Browser View (84 L)**: native `XSLTProcessor` (XSLT 1.0), editable stylesheet pane,
      sandboxed iframe render, timing readout
- [x] **Docs View (133 L)**: in-app Markdown renderer with TOC for Architecture, Roadmap and Rust source
- [x] **Panels (322 L)**: Project (open docs, samples, corpus generator), Info (node inspector),
      Messages (severity, timestamps, click-to-navigate, **Apply SmartFix**), Find in Files,
      XPath/XQuery, Benchmarks (live gates table)
- [x] **SmartFix apply** — 10 syntactic repairs wired end-to-end (rename end tag, insert missing end tag,
      escape `&`, quote attribute value, `--` → `- -`, `]]>` → `]]&gt;`, remove duplicate attribute,
      delete stray end tag, escape `<`, append all missing closers)
- [x] **Save** — File System Access API (`showSaveFilePicker` + `blob.stream().pipeTo`) with
      `<a download>` fallback
- [x] **Keyboard map** — Ctrl+N/O/S, F5, F7, F8, F10, Ctrl+F, F3, Ctrl+H, Ctrl+G, Ctrl+Shift+E,
      Ctrl+Shift+P, Ctrl+Z/Y, Ctrl+1…5, Esc (cancel job)
- [x] **Light/dark theme** (follows `prefers-color-scheme`, toggleable)
- [x] **Live perf instrumentation** — rAF frame-time p50/p99/max over 240 frames + JS heap (Chrome)

### 2.3 Documentation — `src/docs/`

- [x] `architecture.ts` (295 L): component diagram, 10 GB open/edit/save data flow, `DocumentModel`
      trait, 26-crate workspace layout, `.xsi` format, parallel/resumable builder, rope-of-pieces,
      crash recovery, cache policy, benchmark methodology, crate selection, from-scratch list,
      coding standards, UI/UX + keymap spec, browser-sandbox → Tauri fallback table
- [x] `roadmap.ts` (53 L): Phases 0–6 with scope, acceptance criteria and performance gates
- [x] `rustCode.ts` (927 L): Phase 0/1 Rust listings (workspace `Cargo.toml`, `ByteSource`, audited
      `mmap.rs`, `EditBuffer` + proptest, SIMD scanner, `simd.rs`, `.xsi` format + rayon builder,
      `xmlspy-bench`, Leptos Text View, conformance tests)
- [x] `README.md` (repo root): new-machine setup, build, troubleshooting

---

## 3. 🟡 Partially done — known limitations to close

### 3.1 Editing
- [ ] Editing is **line-granular only** — no character-level caret, no selection, no multi-line edit,
      no copy/paste, no typing directly into the view (must press F2/Enter to open a line input)
- [ ] `insertLineAfter` / `deleteLine` exist in `PieceTable` + `XmlDocument` but are **not bound to any key or menu**
- [ ] Undo/redo records whole-line before/after only; no coalescing, no cursor restoration
- [ ] No multi-cursor, no column selection (menu item is Phase-1 badged)
- [ ] No auto-indent, tag auto-close, or tag-rename-pair editing
- [ ] Grid View is **read-only** — no cell editing, no row insert/delete/duplicate, no drag-and-drop reorder

### 3.2 Scanner / index correctness
- [x] ~~BOM split across chunk boundaries produced a spurious "non-whitespace before root" error~~
      — found by the Rust port's resumability tests, fixed in **both** engines (byte-at-a-time BOM states)
- [ ] **UTF-8 only.** BOM is skipped but UTF-16/UTF-32/legacy encodings are not detected or transcoded;
      the `encoding=` pseudo-attribute of the XML declaration is ignored
- [ ] **No namespace well-formedness** (undeclared prefix, duplicate `xmlns`, illegal `xmlns:xml` rebinding)
- [ ] DOCTYPE internal subset is skipped by bracket counting — no entity declarations, no entity
      expansion, no notation/parameter entities, no XXE / billion-laughs guards
- [ ] XML declaration is not validated (version/encoding/standalone order and values)
- [ ] Name validity is not checked against `NameStartChar`/`NameChar` productions
- [ ] `maxIndexed` (300 k elements) silently truncates the navigable tree on very large files —
      Grid/XPath/Schema only see the first N elements; needs on-disk/IndexedDB spill or depth-budgeting UI
- [ ] Element `end` offsets for elements that close after the index cap are reported as `?`

### 3.3 Search
- [ ] Literal only — **no regex**, no whole-word, no wildcard
- [ ] **No Replace / Replace All** (Ctrl+H just opens the Find panel)
- [ ] "Find in Files" searches the **active document only** — no project-wide, no folder, no file globs
- [ ] No XPath-filtered find, no search history, no result grouping per file
- [ ] Case-insensitivity is ASCII-only (no Unicode case folding)

### 3.4 XPath
- [x] ~~The version dropdown (XPath 3.1 / 2.0 / 1.0 / XQuery 3.1) is **cosmetic** — the selection is ignored~~
      — it now defaults to **XPath 1.0 — native** (the truth), the others are labelled `(Phase 3)`
      and picking one shows an inline notice that evaluation still runs as native 1.0
- [x] ~~Unprefixed names silently match nothing on a namespaced document~~ — the sample
      `PurchaseOrders.xml` declares `xmlns="urn:xmlspy:orders"`, so all five built-in chips
      (`//PurchaseOrder[1]`, `count(//Item)`, …) returned **0 node(s)** with no explanation.
      Fixed: a real `XPathNSResolver` + a "Match default namespace" toggle in the panel
- [x] ~~Declared prefixes threw~~ — `dom.evaluate(expr, dom, null, …)` passed a `null` resolver, so
      any `//o:Item` failed. Prefixes now resolve, including ones declared below the root
- [x] ~~Large-file path matched everything for an unknown name~~ — `if (nameId === -2)` guarded a
      sentinel `indexOf()` can never return, so `-1` (not found) fell through to the wildcard
      branch: `//NoSuchElement` returned every element. Now an unknown name matches nothing
- [ ] Large-file evaluator supports only `/a/b`, `//b`, `*`, `[n]`, `count()`, `@attr`
      — no general predicates, functions, axes, variables or sequence types, and it matches the
      index's literal QNames (bare local names in smart mode) without resolving `xmlns` declarations
- [ ] The default-namespace binding is a syntactic rewrite, not a real XDM: `*:Name` name tests,
      namespace nodes, `namespace-uri()` on the bound prefix and re-declared prefixes in a subtree
      are not handled
- [ ] In-memory path builds a full DOM (hard 16 MiB ceiling) and caps results at 2 000 nodes
- [ ] No XQuery engine, no visual XPath builder, no expression explainer, no result export

### 3.5 Validation & SmartFix
- [ ] **No XSD or DTD validation at all.** F8 runs a well-formedness pass and then logs a
      "requires the XSD 1.0/1.1 validator (Phase 2)" warning
- [ ] No schema assignment UI (`xsi:schemaLocation` is only detected by regex for the warning text)
- [ ] SmartFix has no schema-aware fixes (missing required element/attribute, enumeration
      Levenshtein suggestion, child reorder, namespace correction, type cast, key/unique repair)
- [ ] No "fix all" / batch apply, no fix preview diff
- [ ] SmartFix "Append missing closers" only works in in-memory mode

### 3.6 Schema View
- [ ] Content model is **inferred from the instance**, never parsed from a real `.xsd`
- [ ] Sequence-only: no `choice`/`all` detection, no facets, no simple/complex type distinction,
      no substitution groups, no imports/includes, no namespace handling
- [ ] `occ()` in `schemaInfer.ts` is a stub with unused parameters — occurrence is approximated
- [ ] Inference samples the first 100 000 elements only
- [ ] No graphical *editing* (it is a viewer): no add/remove particle, no facet editor, no docs generation

### 3.7 Views / platform
- [ ] Browser View is limited to in-memory docs (< 16 MiB) and XSLT 1.0 (whatever the browser ships)
- [ ] Pretty-print only works in memory mode (no streaming rewrite for large files)
- [ ] No Tree/Outline view, no split view, no cascade/tile windows
- [ ] `Ctrl+G` uses `window.prompt()` instead of a proper dialog
- [ ] No session restore, no recent-files list, no IndexedDB cache of the `.xsi` index —
      a page reload loses every open document
- [ ] No drag-and-drop file open, no `File System Access` re-open handles
- [ ] Menu bar is mouse-only (no Alt-key access, arrow navigation or roving tabindex)
- [ ] No error boundary — an exception in a view blanks the app

---

## 4. ❌ Not started — by roadmap phase

### Phase 0 — Foundation (infrastructure)
- [x] **Cargo workspace that actually compiles** — 5 crates (`core`, `index`, `parse`, `wasm`, `cli`),
      zero external dependencies, `no_std` + `alloc` in the three library crates, pinned toolchain
- [x] **Deterministic corpus generator** — `xmlspy gen --size 256MiB` (997 MB/s, byte-identical output)
- [x] **Benchmark harness + report** — `xmlspy bench` prints throughput, index size, peak RSS and
      PASS/FAIL per gate; report committed to `rust/bench/reports/`
- [ ] The remaining 21 crates of the design (validator, XSLT/XQuery, diff, server, Tauri shell)
- [ ] `cargo xtask ci` (fmt, clippy `-D warnings`, test, wasm build, miri on unsafe, fuzz smoke) —
      the individual commands all pass today, but nothing runs them automatically
- [ ] Playwright frame-time probe + coverage upload
- [ ] Release pipeline: musl / macOS universal / Windows binaries + Tauri bundles

### Phase 1 — Large-file Text View (Rust parity)
- [x] **Resumable tokenizer** with SWAR (8-bytes-at-a-time) classification — feeds at *any* byte
      boundary produce an identical index (tested at chunk sizes 1, 3, 7 …). Explicit AVX2/wasm128
      intrinsics are still open, but `simd128` measured no gain over the portable SWAR path
- [x] **`.xsi` persistence** — v1 codec, `xmlspy index`/`info`, and the same buffer is how the
      index crosses the WASM boundary
- [x] **WASM front end without wasm-bindgen** — 68 KB `cdylib`, hand-written C ABI, `f64` offsets
      (no BigInt), base64-inlined so the single-file build keeps working
- [x] **Engine parity harness** — `npm run test:parity`: 27 documents × 3 chunk sizes, all index
      arrays + every diagnostic string, plus the minified worker source run in a VM
- [ ] `ByteSource` mmap on native (the CLI streams 8 MiB `read()` chunks instead) and audited `mmap.rs`
- [ ] Parallel index builder (rayon) + write-ahead journal
- [ ] Rope-of-pieces edit buffer with proptest round-trips
- [ ] Code folding via the index, bracket matching, bookmarks (Ctrl+F2)
- [ ] Crash recovery from journal; cache eviction policy
- [ ] Session restore; W3C not-wf conformance suite at 100 %
- [ ] Perf gates: 10 GiB first paint < 2 s; **≥ 500 MB/s 1-thread (currently 323 MB/s native /
      252 MB/s WASM on a 2-vCPU sandbox — `xmlspy bench` reports this as FAIL)**; ≥ 1.2 GB/s
      8-thread (no parallel scan yet); RSS < 512 MB **(PASS: 75 MiB on 256 MiB)**; p99 frame < 16 ms

### Phase 2 — Grid, validation, XPath
- [ ] Streaming **XSD 1.0 validator** (PSVI-lite) + **DTD validator**
- [ ] Full SmartFix catalogue (schema-aware)
- [ ] Schema-driven autocompletion in Text View
- [ ] XPath 1.0/2.0/3.1 engine (index-backed + streaming) with QT3 ≥ 99.5 %
- [ ] XPath/XQuery window with live results, visual builder, expression explainer
- [ ] Entities / Elements / Attributes helper windows; namespace prefix tool
- [ ] Grid drag-and-drop, editing, hierarchical↔table toggle polish
- [ ] Gates: XSD 1.0 TS ≥ 99 %, validate ≥ 200 MB/s streaming, expand any node < 50 ms on 10 GiB

### Phase 3 — Schema Designer, XSLT/XQuery, Diff
- [ ] XSD 1.1 (assert, CTA, openContent, xs:all extensions)
- [ ] Graphical schema designer (facets, docs, component navigator, configure view)
- [ ] Flatten / subset / DTD⇄XSD / sample-XML generation / schema docs (HTML/PDF/DOCX)
- [ ] Schema-aware rename refactoring across instances
- [ ] XSLT 1.0/2.0/3.0 compiler + streaming + debugger (breakpoints, step, watch, call stack)
- [ ] XSLT profiler (flame chart) + Speed Optimizer; XSL:FO
- [ ] XQuery 1.0/3.1 + XQuery Update
- [ ] Structural diff/merge (2-way, 3-way, directory compare) + Tree/Outline view
- [ ] Gates: XSLT 3.0 TS ≥ 98 %, XQuery QT3 ≥ 99.5 %, XSD 1.1 TS ≥ 99 %, 2 GiB diff < 5 min

### Phase 4 — Modern data & integrations
- [ ] JSON / JSON5 / JSONC / JSON Lines multi-GB support with the same index strategy
- [ ] JSON Grid + Text views; JSON Schema editor (draft-04 … 2020-12, OpenAPI dialect); JSONPath
- [ ] XML ⇄ JSON ⇄ YAML conversion rules; CSV/text import; Word import/export
- [ ] Apache Avro (`.avsc` editor, container decoding, schema evolution)
- [ ] HTTP/REST client window + OpenAPI/Swagger editor + mock server
- [ ] Database integration (connection manager, SQL editor, result grid, import/export, schema⇄DDL)
- [ ] Charts + wizard + SVG/PNG/PDF export
- [ ] Code generation (C++ / C# / Java / Rust / TS)

### Phase 5 — Enterprise
- [ ] XBRL 2.1 + Dimensions validation, Taxonomy Editor (5 linkbases), Table Linkbase, Formula, XULE, iXBRL, EFM/DQC
- [ ] WSDL 1.1/2.0 designer + validation + documentation
- [ ] SOAP client, WS-Security, SOAP debugger (proxy)
- [ ] XML-DSig (enveloped/enveloping/detached, C14N 1.0/1.1/EXC, RSA/ECDSA/HMAC, X.509, XAdES-BES)
- [ ] Authentic View (SPS subset: templates, auto-calcs, conditions, tables, input controls)

### Phase 6 — Platform & hardening
- [ ] Engine server (job queue, REST `/v1/{validate,xslt,xquery,xbrl,wf,diff,codegen}`, gRPC, auth, metrics)
- [ ] LSP server (diagnostics, completion, hover, rename, formatting, code actions = SmartFix)
- [ ] CLI with RaptorXML-compatible exit codes + Rhai scripting
- [ ] Plugin API (WIT) + VS Code / Eclipse extension shells
- [ ] Global resources, XML catalogs, URL/WebDAV/FTP open, Git awareness (Tauri), spell check,
      templates/snippets, batch operations, encoding dialog, printing
- [ ] Security: cargo-audit/deny, CSP report-only → enforce, XXE / billion-laughs / zip-bomb threat model,
      24 h fuzzing, 48 h soak tests

---

## 5. 🔧 Repository & engineering hygiene (do these first — they are cheap)

- [x] ~~No test suite at all~~ — `cargo test` (41 tests: conformance, resumability, `.xsi` round-trip,
      CLI helpers) and `npm run test:parity` (84 checks) now exist
- [ ] Add Vitest + unit tests for the **TypeScript-only** modules: `pieceTable`, `document` paging,
      `schemaInfer`, `highlight` (these are pure and trivially testable). `xpath` is already covered
      by `npm run test:xpath` — a zero-browser harness that runs the real `xpath.ts` against a
      spec-conformant XPath 1.0 evaluator on documents indexed by the real scanner
- [ ] Add Playwright smoke tests: open sample → F7 → SmartFix → Grid expand → XPath eval → save
- [ ] **No CI.** Add `.github/workflows/ci.yml`: install → `tsc --noEmit` → lint → `npm run test:parity`
      → build, plus a Rust job (`cargo fmt --check`, `cargo clippy -D warnings`, `cargo test`,
      wasm build) that fails if the committed `wasmBinary.ts` is stale
- [ ] **No linter/formatter.** Add ESLint (react-hooks, react-refresh) + Prettier
- [ ] Add npm scripts: `typecheck`, `lint`, `test`, `format`
- [ ] Add a `LICENSE` file
- [ ] **Flatten the nested `XMLSpy/XMLSpy/` folder** (or document it — currently `npm install` at the
      repo root fails). `README.md` documents it for now
- [ ] Property/fuzz tests for the scanner (random byte streams must never throw, never hang)
- [ ] Differential test: compare scanner verdicts against `DOMParser` on a corpus of valid/invalid docs
- [ ] W3C XML Conformance Test Suite harness (`xmlconf`) with a shrinking known-failures list
- [ ] Benchmark regression gates in CI (index MB/s, first-paint ms, p99 frame) with 5 % tolerance
- [ ] Bundle: 490 KB single-file inline (68 KB of it the base64 WASM module); consider dropping `vite-plugin-singlefile` for a normal
      build + real worker entry (`new Worker(new URL(...), {type:'module'})`) instead of Blob-URL + `Function.toString()`
- [ ] Accessibility pass: menu keyboard navigation, ARIA on the virtualized grid, focus management, contrast
- [ ] Replace `prompt()` (Go to Line) with an in-app dialog component
- [ ] Add a React error boundary + a crash/report surface in Messages

---

## 6. 🎯 Suggested next 10 tasks (highest value / lowest risk first)

| # | Task | Why | Est. |
| --- | --- | --- | --- |
| 1 | Vitest + unit tests for `scanner.ts` and `document.ts` paging | Zero safety net today on the most intricate code | 1 d |
| 2 | ESLint + Prettier + GitHub Actions CI | Prevents regressions from here on | 0.5 d |
| 3 | Find: **regex** + **Replace/Replace All** (streaming, chunk-safe) | Highest-frequency missing editor feature | 2 d |
| 4 | Character-level editing in Text View (caret, selection, multi-line) | Current line-input editing is the biggest UX gap | 4 d |
| 5 | IndexedDB persistence of the `.xsi`-style index + session restore | Turns re-open of a 1 GiB file from seconds into instant | 2 d |
| 6 | Namespace well-formedness + XML declaration/encoding validation in the scanner | Correctness gap vs. XML 1.0 + Namespaces 1.0 | 2 d |
| 7 | Minimal streaming **XSD 1.0 validator** (structure + simple types) | F8 is the flagship XMLSpy action and does nothing today | 5 d |
| 8 | Grid View editing (cell edit → overlay → re-index) | Makes Grid a real editor, reuses existing overlay plumbing | 3 d |
| 9 | Real XPath 2.0/3.1 evaluator over the index (predicates, functions, axes) | Replaces the native-1.0 path and the index subset; the panel's `(Phase 3)` entries become real | 5 d |
| 10 | Code folding + bracket matching + bookmarks in Text View | Remaining Phase-1 Text View scope | 3 d |

**Strategic decision — answered (2026-09-05):** the Rust/WASM engine was built and is now the
default execution path in the browser. The TypeScript scanner stays as the fallback and as the
behavioural reference the parity suite diffs against. Remaining Rust work, in value order:

1. **Parallel chunked scan** — the only credible route to the ≥ 500 MB/s gate; the scanner is
   already resumable, so the machinery exists.
2. **`wf` without building the index** — the index arrays dominate per-element cost.
3. Move validation (XSD/DTD) into Rust rather than writing it twice.
4. Native `mmap` `ByteSource`, and IndexedDB persistence of the `.xsi` for instant re-open.
5. CI that rebuilds the module and fails when `src/engine/wasmBinary.ts` is out of date.

---

## 7. Verification commands

```bash
cd XMLSpy              # the app lives in the nested folder
npm ci
npx tsc --noEmit       # currently clean
npm run test:parity    # 84/84 — Rust/WASM engine vs the TypeScript engine
npm run build          # succeeds → dist/index.html (single file, ~490 KB)
npm run dev            # http://localhost:5173

cd ../rust             # the engine (needs Rust only if you change it)
cargo test                                   # 41 tests
cargo clippy --all-targets -- -D warnings    # clean
cargo fmt --all -- --check                   # clean
./build-wasm.sh                              # regenerates ../XMLSpy/src/engine/wasmBinary.ts
cargo run -p xmlspy-cli --release -- gen --size 256MiB --out /tmp/big.xml
cargo run -p xmlspy-cli --release -- bench /tmp/big.xml
```

In-app checks: status bar shows **⚙ Rust/WASM** · `Ctrl+5` Architecture/Roadmap/Rust engine ·
`F7` well-formedness ·
Project ▸ *catalog-broken.xml* → Messages ▸ **Apply SmartFix** ·
Project ▸ *Generate 1 GiB corpus* → large-file mode · `Ctrl+2` Grid · `Ctrl+3` Schema ·
`Ctrl+Shift+E` XPath · Benchmarks tab for live gates.
