#!/usr/bin/env node
/**
 * Vendored mini conformance suite — checked against the reference engine.
 *
 *   npm run test:conformance
 *
 * `rust/conformance/mini` holds 41 well-formedness cases (11 wf, 30 not-wf) that the Rust CLI
 * reports on (`xmlspy conformance`) and that `cargo test -p xmlspy-cli` turns into a gate. The
 * Rust side cannot run everywhere — this script can: it drives the *same* cases and the *same*
 * expectations through `src/engine/scanner.ts`, the behavioural contract the Rust scanner is
 * parity-locked to, so the suite itself is verified even where there is no Rust toolchain.
 *
 * It mirrors `rust/crates/xmlspy-cli/src/conformance.rs` exactly:
 *   runner_config(16) -> { maxIndexed: 0, stride: 32, maxErrors: 16 }
 *   wf     <=> errorCount === 0
 *   not-wf <=> errorCount > 0 and (expect is empty or some retained message contains it)
 *
 * Exit code 0 at 100 %, 1 otherwise.
 */
import { build } from "esbuild";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const SUITE = join(here, "..", "..", "rust", "conformance", "mini");
const out = mkdtempSync(join(tmpdir(), "xmlspy-conformance-"));
process.on("exit", () => rmSync(out, { recursive: true, force: true }));

await build({
  entryPoints: [join(here, "..", "src", "engine", "scanner.ts")],
  outdir: out,
  format: "esm",
  bundle: false,
  logLevel: "error",
});
const { createScanner } = await import(pathToFileURL(join(out, "scanner.js")).href);

const CFG = { maxIndexed: 0, stride: 32, maxErrors: 16 };

const failures = [];
let wfSeen = 0;
let nwfSeen = 0;

for (const raw of readFileSync(join(SUITE, "manifest.tsv"), "utf8").split("\n")) {
  const line = raw.replace(/\r$/, "");
  if (!line || line.startsWith("#")) continue;
  const f = line.split("\t");
  if (f[0] === "id") continue;
  if (f.length < 4) {
    failures.push(`${f[0]}: manifest row has ${f.length} columns, expected 4 or 5`);
    continue;
  }
  const [id, status, file, expectRaw] = f;
  const expect = expectRaw === "-" ? "" : expectRaw;

  let bytes;
  try {
    bytes = new Uint8Array(readFileSync(join(SUITE, file)));
  } catch (e) {
    failures.push(`${id}: cannot read ${file} (${e.message})`);
    continue;
  }

  const sc = createScanner(CFG);
  sc.feed(bytes, 0);
  sc.finish(bytes.length);
  const r = sc.result();
  const msgs = r.errors.map((e) => e.msg);
  if (status === "wf") wfSeen++;
  else if (status === "not-wf") nwfSeen++;
  else {
    failures.push(`${id}: status must be wf or not-wf, got ${JSON.stringify(status)}`);
    continue;
  }

  if (status === "wf") {
    if (r.errorCount !== 0) failures.push(`${id} (wf): ${r.errorCount} diagnostic(s) — ${msgs[0]}`);
  } else if (r.errorCount === 0) {
    failures.push(`${id} (not-wf): reported well-formed`);
  } else if (expect && !msgs.some((m) => m.includes(expect))) {
    failures.push(
      `${id} (not-wf): no diagnostic contains ${JSON.stringify(expect)}; got ${JSON.stringify(msgs.slice(0, 3))}`,
    );
  }
}

console.log(`conformance/mini: ${wfSeen} wf + ${nwfSeen} not-wf = ${wfSeen + nwfSeen} cases`);
if (failures.length === 0) {
  console.log("100 % — every case holds against src/engine/scanner.ts (the reference engine)");
} else {
  console.log(`${failures.length} case(s) do not hold:`);
  for (const f of failures) console.log(`  - ${f}`);
}
process.exit(failures.length === 0 ? 0 : 1);
