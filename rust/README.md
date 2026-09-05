# XMLSpy-rs — Rust engine

The XML engine that the browser app actually runs. Eight crates, **zero external
dependencies**, one C ABI, two front ends (WebAssembly and a native CLI).

~~~
crates/
  xmlspy-core      no_std + alloc   shared types: diagnostics, byte sources, limits, CHUNK_SIZE
  xmlspy-index     no_std + alloc   StructuralIndex, the .xsi v1 codec, folding/brackets/bookmarks
  xmlspy-parse     no_std + alloc   the resumable Scanner state machine, SWAR byte classifier, Finder
  xmlspy-rope      no_std + alloc   rope-of-pieces edit buffer (immutable original + append-only adds)
  xmlspy-io        std              audited mmap, buffered fallback, .xsi cache (LRU), write-ahead journal
  xmlspy-parallel  std              split pass + threaded segment scans + an exact merge
  xmlspy-wasm      std, cdylib      hand-written C ABI for the browser (no wasm-bindgen)
  xmlspy-cli       std, bin + lib   `xmlspy` — wf / index / info / search / gen / bench / edit / fold / recover / conformance
~~~

`core`, `index`, `parse` and `rope` are `no_std` + `alloc`, which is what lets the identical
code serve a 32-bit WASM module and a native binary. `unsafe` is confined to one audited
module (`xmlspy-io/src/mmap.rs`); every other crate is `unsafe_code = "forbid"`. Nothing is
behind a feature flag or a `cfg(target_arch)` fork except the two front ends.

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

**Parallel, but bit-identical.** `xmlspy-parallel` cuts a document at *legal* boundaries
only: a sequential pass of the real `Scanner` (with `max_indexed = 0`, so it retains no
records) captures a full `BoundaryState` at the first depth-1 element close at or after
each target offset. One thread per segment resumes from its boundary, so every segment
starts in exactly the state the sequential scanner would be in — depth, open-element stack,
line, quote/CDATA/DOCTYPE sub-state. The merge then renumbers ids, remaps parents,
concatenates checkpoints and diagnostics, re-applies the *global* element budget and patches
the top-level element's end offset. The result equals a single sequential scan field for
field; `xmlspy-parallel/src/merge.rs` carries the proof that a segment always retains a
superset of what the merge keeps, and `tests/parity.rs` asserts the equality over 45 corpora
× thread counts 1–8 × budgets 0–100 000 × strides 1–32. Because the split pass is
sequential and costs a fraction `c` of a full scan, the speed-up ceiling is `1/(c + 1/N)` —
reported, not promised.

**mmap, a cache and a journal.** `xmlspy-io` maps documents read-only (audited, the
workspace's only `unsafe`) with a buffered `read()` fallback, so `ByteSource::as_slice()`
hands threads a shared `&[u8]` with no copy. `.xsi` indexes live in a byte-budgeted
directory keyed by length + mtime + a CRC of the first and last 4 KiB, with LRU eviction.
A parallel build journals each segment's `.xsi` **as it arrives** (CRC-guarded, torn-tail
tolerant), so `xmlspy recover` finishes an interrupted build by re-scanning only the
segments that never completed — and a committed build deletes its log.

**Edits do not rewrite the document.** `xmlspy-rope` keeps the original buffer immutable and
appends new bytes to a second buffer; an edit splits at most two pieces. A 3-byte edit in a
4 MiB document leaves 3 pieces and an `unchanged_ratio` of 0.999999+, and saving streams the
untouched runs straight from the original (`try_each_chunk`), which is the same shape as the
browser's `Blob`-part save. `tests/props.rs` runs 5 seeds × 1500 random operations against a
`Vec<u8>` oracle, checking every invariant after every operation.

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
against the TypeScript scanner. Rust-side tests: `cargo test` — conformance, resumability,
boundary round-trips, parallel/sequential parity, `.xsi` round-trips, rope properties,
mmap/cache/journal behaviour, folding and bookmarks, and the CLI helpers.

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

Those numbers predate the mmap + parallel work and were measured on a 2-vCPU sandbox, which
cannot show an 8-thread result. `xmlspy bench file.xml --threads 8 --min-segment 1MiB` now
prints both routes (well-formedness with no index retained, and the full index) plus the
sequential/parallel comparison and both gates; re-run it on real hardware and update the
report. `./verify-phase1.sh` runs the whole Phase 1 gate set in one go.

## CLI

~~~bash
xmlspy wf      file.xml              # well-formedness; exit 0 ok, 1 not well-formed, 2 usage/IO
xmlspy index   file.xml --out f.xsi  # build + persist the structural index
xmlspy info    f.xsi                 # inspect a .xsi
xmlspy search  file.xml "needle"     # streaming literal search, grep-style output
xmlspy gen     --size 256MiB --out big.xml
xmlspy bench   file.xml --threads 8  # both routes, sequential vs parallel, RSS, gates
xmlspy edit    file.xml --insert-after 12 --text "  <row/>" --out edited.xml
xmlspy fold    file.xml --lines 1-500   # fold regions; also --bracket OFFSET, --line N, --bookmark 1,5,9
xmlspy recover file.xml              # finish an interrupted build from its journal
xmlspy conformance --verbose         # the vendored mini suite (or --suite DIR for W3C xmlconf)
~~~

Shared flags: `--threads N` (0 = auto), `--sequential`, `--min-segment 32MiB`,
`--no-index`, `--cache-dir DIR`, `--cache-budget 2GiB`, `--journal`.

## Conformance

`rust/conformance/mini` is a vendored 41-case well-formedness suite (11 `wf`, 30 `not-wf`),
one case per production and WFC the scanner implements, each not-wf case expecting a
*specific* diagnostic substring. `xmlspy conformance --suite DIR` also runs an unpacked W3C
`xmlconf` tree, classifying documents by directory (`not-wf/` must fail; `wf/`, `valid/`,
`invalid/` must not). See [`conformance/mini/README.md`](conformance/mini/README.md) — the
real W3C suite is not vendored and has to be downloaded where the network allows.
