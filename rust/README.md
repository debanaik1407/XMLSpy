# XMLSpy-rs — Rust engine

The XML engine that the browser app actually runs. Five crates, **zero external
dependencies**, one C ABI, two front ends (WebAssembly and a native CLI).

~~~
crates/
  xmlspy-core    no_std + alloc   shared types: diagnostics, byte sources, limits, CHUNK_SIZE
  xmlspy-index   no_std + alloc   StructuralIndex + the .xsi v1 serialisation codec
  xmlspy-parse   no_std + alloc   the resumable Scanner state machine, SWAR byte classifier, Finder
  xmlspy-wasm    std, cdylib      hand-written C ABI for the browser (no wasm-bindgen)
  xmlspy-cli     std, bin         `xmlspy` — wf / index / info / search / gen / bench
~~~

`core`, `index` and `parse` are `no_std` + `alloc`, which is what lets the identical code
serve a 32-bit WASM module and a native binary. Nothing is behind a feature flag or a
`cfg(target_arch)` fork except the two front ends.

## Build

~~~bash
# native CLI
cargo build --release -p xmlspy-cli
./target/release/xmlspy bench big.xml

# the module the web app embeds (writes ../XMLSpy/src/engine/wasmBinary.ts)
./build-wasm.sh                 # or: npm run build:wasm  (from ../XMLSpy)
SIMD=1 ./build-wasm.sh          # + wasm simd128 (measured: no gain, see bench/reports)

# tests, lints, formatting
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check
~~~

Toolchain is pinned by `rust-toolchain.toml` (1.88.0 + the `wasm32-unknown-unknown`
target). Because there are no dependencies, every command above works with `--offline`.

## Design

**Resumable, allocation-light scanner.** `Scanner::feed(&[u8], base_offset)` may be called
with the document split at *any* byte boundary — including mid-tag, mid-entity, or in the
middle of a UTF-8 BOM — and produces the same index as a single call. That is what lets
the browser stream 8 MiB `Blob.slice()` chunks and the CLI stream 8 MiB `read()` chunks
through one code path. `tests/resumable.rs` asserts this at chunk sizes 1, 3, 7, …

**SWAR, not intrinsics.** The hot loop skips runs of uninteresting bytes eight at a time
using portable `u64` arithmetic (`has_byte`, `find_any4`, `find_text_delim`,
`find_name_end` in `classify.rs`). One code path covers x86-64, aarch64 and the WASM MVP;
`simd128` measured no improvement over it.

**Two interners.** Element names and attribute names live in separate open-addressing
tables, each fronted by a one-entry "same as last time" cache. Element names must stay
alone in `StructuralIndex::names` because callers look names up by id *and* search the
table by name (the XPath evaluator does). Replacing a `BTreeMap<String, u32>` with this
was worth +65 % end-to-end throughput.

**Bounded index.** Indexing stops at `max_indexed` elements, except that depth ≤ 1
elements are always indexed (so the top of the tree is never lost), with a hard stop at
`2 × max_indexed`. Without the cap, a structural index of element-dense XML is *larger
than the document itself* (112 % measured — see `bench/reports/`).

**One buffer crosses the ABI.** Results are serialised into a single `.xsi` v1 buffer
(magic `XSI1`, 112-byte header, nine 8-byte-aligned sections) and decoded once on the JS
side into transferable typed arrays. Offsets and counters cross as `f64`, so there is no
BigInt anywhere in the boundary.

## C ABI (`xmlspy-wasm`, `ABI_VERSION = 1`)

No wasm-bindgen, no glue generator, no `Function` imports: the module imports nothing and
exports plain functions, so it instantiates from a `Uint8Array` in a Blob-URL worker.

~~~
xs_abi_version() -> i32
xs_alloc(len) -> ptr        xs_free(ptr, len)

xs_scanner_new(max_indexed, stride, max_errors) -> handle
xs_scanner_feed(h, ptr, len, base_offset_f64)
xs_scanner_finish(h, total_len_f64)     serialises the .xsi
xs_scanner_snapshot(h)                  serialises the .xsi *without* ending the document
xs_scanner_xsi_ptr(h) / xs_scanner_xsi_len(h)
xs_scanner_line(h) / xs_scanner_elements(h) / xs_scanner_errors(h)   progress, no serialisation
xs_scanner_free(h)

xs_finder_new(ptr, len, case_insensitive, max_hits) -> handle
xs_finder_feed(h, ptr, len, base_offset_f64)   xs_finder_finish(h)
xs_finder_total(h) -> f64                      xs_finder_snapshot(h) -> count
xs_finder_hits_ptr(h)                          HIT_STRIDE = 24 bytes: {offset, line, col} as f64
xs_finder_clear_hits(h)                        xs_finder_free(h)
~~~

The JS side of this ABI is `../XMLSpy/src/engine/wasmEngine.ts`.

## Behavioural contract

`../XMLSpy/src/engine/scanner.ts` (the TypeScript engine that shipped first) is the
reference implementation, and the Rust port must match it **exactly** — not just element
counts, but line/column of every diagnostic, the wording of every message, and every
SmartFix suggestion string, since the UI matches those strings with regexes.

`npm run test:parity` (from `../XMLSpy`) enforces that: 27 documents × 3 chunk sizes, all
index arrays, the name table and every diagnostic compared field by field, plus a phase
that runs the *minified production worker source* in a Node VM and diffs its output
against the TypeScript scanner. Rust-side tests: `cargo test` — 41 tests across 11
binaries (conformance, resumability, `.xsi` round-trips, CLI helpers).

The one place they *did* disagree was a bug in the TypeScript scanner: it detected the
UTF-8 BOM by peeking two bytes ahead, so a BOM split across chunks produced a spurious
"Non-whitespace character data before the root element". The Rust engine matches the BOM
one byte at a time; the TypeScript scanner was fixed to do the same.

## Performance

Single thread, 64 MiB corpus, 2-vCPU sandbox: **301 MB/s** native, **252 MB/s** through
WebAssembly in V8, **65 MB/s** for the TypeScript scanner. Peak RSS on a 256 MiB document
is 75 MiB and the index is 7.9 % of the file.

The Phase 0/1 design's ≥ 500 MB/s gate is **not** met (`xmlspy bench` prints `FAIL` for it
rather than hiding it); it assumed a multi-core chunked scan on desktop hardware. Full
numbers, the `opt-level` study, and the routes to closing the gap:
[`bench/reports/2026-09-05-linux-x86_64.md`](bench/reports/2026-09-05-linux-x86_64.md).

## CLI

~~~bash
xmlspy wf      file.xml              # well-formedness; exit 0 ok, 1 not well-formed, 2 usage/IO
xmlspy index   file.xml --out f.xsi  # build + persist the structural index
xmlspy info    f.xsi                 # inspect a .xsi
xmlspy search  file.xml "needle"     # streaming literal search, grep-style output
xmlspy gen     --size 256MiB --out big.xml
xmlspy bench   file.xml              # 3 runs, throughput, index size, peak RSS, gates
~~~
