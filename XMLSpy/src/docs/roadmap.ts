export const ROADMAP_MD = String.raw`
# 5. Phased Delivery Roadmap

Each phase ends with a tagged release, a conformance report and a benchmark report committed to bench/reports/. A phase is "done" only when every acceptance criterion and performance gate is green in CI on Linux/macOS/Windows and in Chrome/Firefox/Safari/Edge (Playwright matrix).

## Phase 0 — Foundation (weeks 1–4)
**Deliverables**: workspace skeleton (all crates compile, empty APIs), CI (fmt, clippy -D warnings, test, wasm build, miri on unsafe modules, fuzz smoke 5 min), bench harness with corpus generator (1 MiB → 10 GiB, deterministic), Playwright frame-time probe, coverage upload, release pipeline producing static binaries (musl/macOS universal/windows) + Tauri bundles.
**Acceptance**: cargo xtask ci passes; gen 10GiB produces byte-identical corpora on all OSes; bench baseline reports published.

## Phase 1 — Large-file Text View foundation (weeks 5–12)
**Scope**: ByteSource (mmap + browser slice), resumable SIMD tokenizer, parallel index builder + .xsi persistence + journal, line table, rope piece buffer, streamed save, virtualized Text View (syntax colouring, folding via index, bracket matching, line numbers, minimap, go-to-line, bookmarks), well-formedness (F7) with Messages window + click-to-navigate, Find/Replace (literal + regex over chunks with carry-over, XPath filter stub), session restore, light/dark themes, keyboard map.
**Gates**: open 10 GiB first paint < 2 s (target ≤ 300 ms with cached index); index ≥ 500 MB/s single-thread native, ≥ 1.2 GB/s 8 threads; RSS < 512 MB; p99 frame < 16 ms while scrolling 10 GiB; save of a 3-byte edit in a 10 GiB file < 30 s (copy-bound) native / streaming in browser; W3C not-wf suite 100 %.

## Phase 2 — Grid View, validation, XPath (weeks 13–24)
**Scope**: LazyTree + Grid View (hierarchical, table mode, drag-drop, sort, inline filter), Info window, Entities/Elements/Attributes helpers, XSD 1.0 validator (streaming, PSVI-lite) + DTD, SmartFix engine (fix catalogue: missing required element/attribute, enumeration Levenshtein suggestion, child reorder, namespace correction, type cast, unique/key repair), schema-driven autocompletion in Text View, XPath 1.0/2.0/3.1 engine (index-backed + streaming), XPath/XQuery window with live results, visual XPath builder, expression explainer.
**Gates**: XSD 1.0 TS ≥ 99 %; QT3 XPath ≥ 99.5 %; validate 10 GiB streaming ≥ 200 MB/s; Grid expands any node in < 50 ms on 10 GiB.

## Phase 3 — Schema Designer, XSLT/XQuery, Diff (weeks 25–40)
**Scope**: XSD 1.1 (assert, CTA, openContent, xs:all extensions), graphical designer (content-model diagram, component navigator, facets, docs), flatten/subset, DTD⇄XSD, sample-gen, schema-from-instance, schema docs (HTML/PDF/DOCX), schema-aware rename refactoring; XSLT 1.0/2.0/3.0 compiler + streaming + debugger (breakpoints, step, watch, call stack, output tracing) + profiler (flame chart) + Speed Optimizer; XQuery 1.0/3.1 + Update; structural diff/merge (2/3-way, dir compare, options), Tree/Outline view.
**Gates**: XSLT 3.0 TS ≥ 98 %; XQuery QT3 ≥ 99.5 %; XSD 1.1 TS ≥ 99 %; diff of two 2 GiB files streaming < 5 min; transform 1 GiB with streamable stylesheet in constant memory.

## Phase 4 — Modern data + integrations (weeks 41–54)
**Scope**: JSON5/JSONC + multi-GB JSON/JSON Lines (same index strategy), JSON Grid/Text, JSON Schema editor (draft-04…2020-12, OpenAPI dialect), JSON⇄XML rules, YAML, JSONPath; Avro (.avsc editor, .avro container decoding, evolution); HTTP window + OpenAPI/Swagger editor + mock server + request generation; DB integration (connection manager, SQL editor, result grid, import/export, schema⇄DDL, XML columns); charts + wizard + SVG/PNG/PDF export; code generation (C++/C#/Java/Rust/TS, SPL-like templates).
**Gates**: JSON-Schema-Test-Suite ≥ 99 %; 10 GiB JSON Lines opens < 2 s; codegen round-trip tests compile with clang/dotnet/javac in CI.

## Phase 5 — Enterprise (weeks 55–72)
**Scope**: XBRL 2.1 + Dimensions validation, Taxonomy Editor (concepts + 5 linkbases), Table Linkbase editor + preview, Formula editor/processor, XULE, iXBRL viewer/validator, EFM + DQC rules, taxonomy packages, XBRL charts/tables; WSDL 1.1/2.0 designer + validation, SOAP client + WS-Security, SOAP debugger (proxy in server), WSDL docs; XML-DSig (enveloped/enveloping/detached, C14N 1.0/1.1/EXC, RSA/ECDSA/HMAC, X.509, XAdES-BES); Authentic View (SPS subset: templates, auto-calcs, conditions, tables, input controls) + Browser View.
**Gates**: XBRL-CONF-2014 ≥ 99 %, XDT-CONF 100 %, FORMULA-CONF ≥ 98 %, iXBRL 1.1 100 %; DSig interop vectors 100 %; WS-I BP 1.2 assertions pass.

## Phase 6 — Platform & hardening (weeks 73–84)
**Scope**: engine server (job queue, parallel workers, REST /v1/{validate,xslt,xquery,xbrl,wf,diff,codegen}, gRPC, auth, metrics), LSP server (diagnostics, completion, hover, rename, formatting, code actions = SmartFix), CLI (all commands, scripting with Rhai, exit codes compatible with RaptorXML), plugin API (WIT) + sample plugins, VS Code/Eclipse extension shells, global resources, catalogs, URL/WebDAV open, Git awareness (Tauri), spell check, templates/snippets, batch ops, docs site, security audit (cargo-audit, cargo-deny, CSP report-only → enforce), soak tests (48 h), threat model for XXE/billion-laughs/zip bombs.
**Gates**: 0 high CVEs; fuzz 24 h no crashes; LSP conformance with vscode-languageserver test harness; docs coverage 100 % of commands.

# Conformance & test corpus policy

* Suites are vendored via git-lfs in conformance/ and executed by cargo xtask conformance <suite> producing JUnit + a markdown badge table; per-test expectations live in TOML "known-failures" files that must shrink monotonically (CI fails if a previously passing test regresses).
* Each parser has: unit tests, proptest round-trips (serialize(parse(x)) == canonical(x)), differential tests against an oracle (quick-xml, libxml2 via CLI in CI, Saxon-HE for XSLT/XQuery, Arelle for XBRL), and a cargo-fuzz target.

# How to run (Phase 0/1 code in the "Rust Source" tab)

~~~bash
# prerequisites: rustup 1.85 stable, wasm32-unknown-unknown target, trunk, node 20 (playwright)
git clone https://github.com/example/xmlspy-rs && cd xmlspy-rs
cargo xtask ci                              # fmt + clippy + tests + wasm build
cargo run -p xmlspy-bench --release -- gen --size 10GiB --out /tmp/po10g.xml
cargo run -p xmlspy-bench --release -- index /tmp/po10g.xml        # prints MB/s, RSS, index size
cargo run -p xmlspy-bench --release -- wf /tmp/po10g.xml           # well-formedness only
cargo bench -p xmlspy-bench                                        # criterion micro-benchmarks
cargo run -p xmlspy-web --release -- --root ./dist --port 8080     # serves the Leptos bundle
trunk serve crates/xmlspy-ui                                       # dev server for the UI
cargo run -p xmlspy-cli -- wf /tmp/po10g.xml --index               # headless check
~~~
`;
