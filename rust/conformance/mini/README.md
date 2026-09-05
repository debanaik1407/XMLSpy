# Mini conformance suite

A small, vendored, self-contained well-formedness suite: **41 cases** (11 `wf`, 30 `not-wf`)
that pin what `xmlspy wf` must say about each construct the scanner implements.

```
rust/conformance/mini/
├── manifest.tsv          id · status · file · expected diagnostic substring · spec reference
└── cases/
    ├── wf/               documents that must scan with zero diagnostics
    └── not-wf/           documents that must scan with a *specific* diagnostic
```

Run it:

```sh
cargo run -q -p xmlspy-cli -- conformance                 # this suite
cargo run -q -p xmlspy-cli -- conformance --verbose       # one line per case
cargo run -q -p xmlspy-cli -- conformance --suite DIR     # an external suite (see below)
cargo test  -p xmlspy-cli --test conformance              # the same suite as a test gate
```

Exit status is `0` only at 100 %.

## What this is, and what it is not

This is **not** the W3C XML conformance suite. The real one (`xmlconf`, ~2 000 documents
published by the W3C XML Core Working Group) is not vendored here: it is downloaded
separately and its own licence and packaging apply. What is vendored instead is a suite
authored for this repository, one case per production and per well-formedness constraint
the scanner implements, with each expectation expressed as a substring of the diagnostic
the scanner must emit — so a case fails when the *message* regresses, not only when the
pass/fail verdict flips.

Every case here is also asserted by a Rust test, so the two cannot drift apart silently:
the file-based suite is the report, `cargo test` is the gate. Most expectations are covered
by [`xmlspy-parse/tests/conformance.rs`](../../crates/xmlspy-parse/tests/conformance.rs)
(its `mini_suite_diagnostics` test pins the six that nothing else exercises, using strings
byte-identical to the case files), and all 41 by
[`xmlspy-cli/tests/conformance.rs`](../../crates/xmlspy-cli/tests/conformance.rs), which runs
this directory through the runner itself.

### Verified against the reference engine — 41/41, 2026-09-05

The Rust crates were written in a sandbox with no Rust toolchain, so `xmlspy conformance` has
never been executed. What *has* been executed is the same suite against the engine the Rust
scanner is parity-locked to:

```sh
cd XMLSpy && npm run test:conformance
# conformance/mini: 11 wf + 30 not-wf = 41 cases
# 100 % — every case holds against src/engine/scanner.ts (the reference engine)
```

[`XMLSpy/scripts/conformance-mini.mjs`](../../../XMLSpy/scripts/conformance-mini.mjs) reads
`manifest.tsv`, scans every case with the TypeScript scanner under exactly the runner's
configuration (`maxIndexed: 0, stride: 32, maxErrors: 16`) and applies exactly the runner's
verdict rule. So the suite's expectations are known-good and the remaining question is Rust
parity — which `npm run test:parity` (84/84) covers — not suite authoring.

## Running the real W3C suite

`xmlspy conformance --suite DIR` accepts an unpacked `xmlconf` tree and classifies each
document by the directory it sits in, which is how the W3C suite states its expectation:

| path contains   | expectation                                        |
| --------------- | -------------------------------------------------- |
| `not-wf/`       | the scanner must report at least one diagnostic     |
| `wf/`, `valid/` | the scanner must report none                        |
| `invalid/`      | the scanner must report none (validity is Phase 2)  |

No manifest is needed in that mode; `.xml` files are found recursively.

```sh
curl -O https://www.w3.org/XML/Test/xmlconf-20130923.tar.gz   # or the current release
tar xzf xmlconf-20130923.tar.gz
xmlspy conformance --suite xmlconf/xmltest --verbose > conformance-report.txt
```

The 100 % target in `TODO.md` refers to that run. It has not been measured — w3.org is
unreachable from the sandbox this was developed in, and there is no Rust toolchain there
either. The runner, the classification rules and the report format are in place, so the
number can be produced with one command on a machine that has both; the vendored suite above
is what can be (and has been) checked meanwhile.
