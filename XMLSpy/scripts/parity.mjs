#!/usr/bin/env node
/**
 * Engine parity + worker self-test.
 *
 *   npm run test:parity
 *
 * Phase 1 — index parity: every sample document (plus a pile of malformed edge cases) is
 * scanned by BOTH engines, at several chunk sizes, and every field of the resulting
 * structural index is compared: counters, checkpoint table, the six element arrays, the
 * name table and every diagnostic (message, line/col, severity, SmartFix string).
 * The TypeScript scanner in src/engine/scanner.ts is the behavioural contract; the Rust
 * engine must reproduce it byte for byte.
 *
 * Phase 2 — worker self-test: the *minified production* worker source (the same string
 * `EngineWorker` hands to `URL.createObjectURL`) is executed in a Node VM with a fake
 * `self`, then driven through a real scan and a real search. This catches bundler damage
 * to the `Function.toString()` serialisation, which a type-check cannot see.
 */
import { build } from "esbuild";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import vm from "node:vm";

const out = mkdtempSync(join(tmpdir(), "xmlspy-parity-"));
process.on("exit", () => rmSync(out, { recursive: true, force: true }));

await build({
  entryPoints: [
    "src/engine/scanner.ts",
    "src/engine/corpus.ts",
    "src/engine/wasmEngine.ts",
    "src/engine/wasmBinary.ts",
    "src/engine/worker.ts",
  ],
  outdir: out,
  format: "esm",
  bundle: false,
  logLevel: "error",
});
// The worker module is also built the way `vite build` ships it: bundled + minified.
await build({
  entryPoints: ["src/engine/worker.ts"],
  outfile: join(out, "worker.min.mjs"),
  format: "esm",
  bundle: true,
  minify: true,
  target: "es2022",
  logLevel: "error",
});

const load = (f) => import(pathToFileURL(join(out, f)).href);
const { createScanner } = await load("scanner.js");
const { SAMPLE_ORDERS, SAMPLE_BROKEN, SAMPLE_XSD } = await load("corpus.js");
const { bootWasmEngine } = await load("wasmEngine.js");
const { WASM_BASE64, WASM_BUILD } = await load("wasmBinary.js");

const enc = new TextEncoder();
let failures = 0;
let checks = 0;
const ok = (label, extra = "") => {
  checks++;
  console.log(`  \u2713 ${label}${extra ? "  " + extra : ""}`);
};
const bad = (label, diffs) => {
  checks++;
  failures++;
  console.log(`  \u2717 ${label}\n      ${diffs.slice(0, 8).join("\n      ")}`);
};

const engine = await bootWasmEngine(WASM_BASE64);
console.log(`engine: Rust ${WASM_BUILD.rustc} \u2192 ${WASM_BUILD.target}, ${WASM_BUILD.bytes} bytes, ABI v${engine.abi}\n`);

// ---------------------------------------------------------------- phase 1
const docs = {
  SAMPLE_ORDERS,
  SAMPLE_BROKEN,
  SAMPLE_XSD,
  bom: "\uFEFF<a><b/></a>",
  cdata: `<r><![CDATA[<not/> & "raw" ]]]><x/></r>`,
  comments: `<!-- lead --><r><!--in--><a/><!-- multi\nline --></r><!-- trail -->`,
  pi: `<?xml version="1.0"?><?php echo "x" ?><r><?target data?></r>`,
  doctype: `<!DOCTYPE r [ <!ELEMENT r (#PCDATA)> ]><r>text</r>`,
  entities: `<r>a &amp; b &lt;c&gt; &#65; &#x42; &unknown; &;</r>`,
  attrs: `<r a="1" b='2' c = "3" d="&quot;q&quot;" e="<bad" />`,
  crlf: "<r>\r\n<a/>\r\n</r>\r\n",
  utf8: `<r><t>Z\u00fcrich \u2013 na\u00efve \u2013 \u65e5\u672c\u8a9e \u2013 \ud83c\udf89</t></r>`,
  nested: "<a>" + "<b>".repeat(40) + "x" + "</b>".repeat(40) + "</a>",
  empty: "",
  textOnly: "hello",
  unclosedTag: "<a><b",
  badName: "<1bad/>",
  mismatch: "<a></b>",
  extraClose: "<a/></a>",
  twoRoots: "<a/><b/>",
  dupAttr: `<a x="1" x="2"/>`,
  rawAmp: "<a>Tom & Jerry</a>",
  rawLt: "<a>1 < 2</a>",
  unquoted: "<a x=1/>",
  badCommentDash: "<a><!-- a -- b --></a>",
  lonelyGt: "<a>1 > 2</a>",
  deepAttrs: `<r ${Array.from({ length: 40 }, (_, i) => `a${i}="${i}"`).join(" ")}/>`,
};

const SCALARS = ["stride", "lineCount", "indexedElements", "totalElements", "totalAttributes", "maxDepth", "errorCount"];
const ARRAYS = ["checkpoints", "elemStart", "elemEnd", "elemLine", "elemParent", "elemName", "elemDepth"];
const fmtErrors = (e) => e.map((x) => `${x.line}:${x.col}@${x.offset} ${x.severity} ${x.msg} | ${x.fix ?? ""}`);

function diffIndex(a, b) {
  const d = [];
  for (const f of SCALARS) if (a[f] !== b[f]) d.push(`${f}: ts=${a[f]} rust=${b[f]}`);
  for (const f of ARRAYS) {
    const x = Array.from(a[f]);
    const y = Array.from(b[f]);
    if (x.length !== y.length) {
      d.push(`${f}.length: ts=${x.length} rust=${y.length}`);
      continue;
    }
    for (let i = 0; i < x.length; i++)
      if (x[i] !== y[i]) {
        d.push(`${f}[${i}]: ts=${x[i]} rust=${y[i]}`);
        break;
      }
  }
  if (JSON.stringify(a.names) !== JSON.stringify(b.names)) d.push(`names: ts=${JSON.stringify(a.names)} rust=${JSON.stringify(b.names)}`);
  const ea = fmtErrors(a.errors);
  const eb = fmtErrors(b.errors);
  if (ea.length !== eb.length) d.push(`errors.length: ts=${ea.length} rust=${eb.length}`);
  for (let i = 0; i < Math.min(ea.length, eb.length); i++) if (ea[i] !== eb[i]) d.push(`errors[${i}]:\n        ts   = ${ea[i]}\n        rust = ${eb[i]}`);
  return d;
}

console.log("phase 1 \u2014 index parity (TypeScript scanner vs Rust/WASM engine)");
for (const [name, text] of Object.entries(docs)) {
  const bytes = enc.encode(text);
  for (const chunk of [1 << 20, 7, 1]) {
    const cfg = { maxIndexed: 100000, stride: 8, maxErrors: 50 };
    const ts = createScanner(cfg);
    const rs = engine.createScanner(cfg);
    for (let i = 0; i < bytes.length; i += chunk) {
      const part = bytes.subarray(i, Math.min(bytes.length, i + chunk));
      ts.feed(part, i);
      rs.feed(part, i);
    }
    ts.finish(bytes.length);
    rs.finish(bytes.length);
    const a = ts.result();
    const b = rs.result();
    rs.free();
    const d = diffIndex(a, b);
    const label = `${name} @chunk=${chunk}`;
    if (d.length) bad(label, d);
    else if (chunk === 1) ok(label, `${a.totalElements} el, ${a.totalAttributes} attr, ${a.errorCount} err`);
    else checks++;
  }
}

// element budget: identical truncation behaviour under a tight cap
{
  const bytes = enc.encode(`<r>${"<row><c/></row>".repeat(200)}</r>`);
  const cfg = { maxIndexed: 40, stride: 32, maxErrors: 10 };
  const ts = createScanner(cfg);
  const rs = engine.createScanner(cfg);
  ts.feed(bytes, 0);
  rs.feed(bytes, 0);
  ts.finish(bytes.length);
  rs.finish(bytes.length);
  const d = diffIndex(ts.result(), rs.result());
  rs.free();
  if (d.length) bad("element budget @maxIndexed=40", d);
  else ok("element budget @maxIndexed=40", `${ts.result().indexedElements} of ${ts.result().totalElements} indexed`);
}

// ---------------------------------------------------------------- phase 2
console.log("\nphase 2 \u2014 minified worker source, executed in a VM with a fake `self`");
const { workerSource } = await load("worker.min.mjs");
const src = workerSource();

function runWorker(message) {
  return new Promise((resolve, reject) => {
    const messages = [];
    const sandbox = {
      TextEncoder,
      TextDecoder,
      WebAssembly,
      Blob,
      performance,
      atob,
      URL,
      console,
      setTimeout,
      queueMicrotask,
      Uint8Array,
      Float64Array,
      Int32Array,
      Uint16Array,
      Error,
    };
    sandbox.self = {
      onmessage: null,
      postMessage(m) {
        messages.push(m);
        if (m.type === "done" || m.type === "searchDone") resolve({ final: m, messages });
        else if (m.type === "error") reject(new Error(m.message));
      },
    };
    sandbox.globalThis = sandbox;
    vm.createContext(sandbox);
    vm.runInContext(src, sandbox, { filename: "xmlspy-worker.js" });
    if (typeof sandbox.self.onmessage !== "function") return reject(new Error("worker never installed onmessage"));
    sandbox.self.onmessage({ data: message });
  });
}

const big = SAMPLE_ORDERS.repeat(1).replace(/^<\?xml[^>]*\?>\n?/, "");
const docText = `<?xml version="1.0"?>\n<root>${big}${"<pad>Z\u00fcrich needle</pad>".repeat(500)}</root>`;
const blob = new Blob([docText]);

{
  const { final, messages } = await runWorker({
    type: "scan",
    id: 1,
    file: blob,
    chunkSize: 4096,
    maxIndexed: 100000,
    stride: 32,
    maxErrors: 100,
  });
  const ref = createScanner({ maxIndexed: 100000, stride: 32, maxErrors: 100 });
  const bytes = enc.encode(docText);
  ref.feed(bytes, 0);
  ref.finish(bytes.length);
  const d = diffIndex(ref.result(), final.result);
  if (final.engine !== "rust-wasm") bad("worker scan uses the Rust engine", [`engine=${final.engine}`]);
  else if (d.length) bad("worker scan index matches the TypeScript scanner", d);
  else ok("worker scan", `${final.result.totalElements} elements, engine=${final.engine}, ${messages.filter((m) => m.type === "progress").length} progress messages`);
}

{
  const { final } = await runWorker({ type: "search", id: 2, file: blob, query: "z\u00fcrich", caseSensitive: false, chunkSize: 4096, maxHits: 5000 });
  const expected = docText.toLowerCase().split("z\u00fcrich").length - 1;
  const bytesOf = (s) => enc.encode(s).length;
  const offsetsOk = final.hits.every((h) => bytesOf(docText.slice(0, 0)) === 0 && h.offset >= 0 && h.preview.toLowerCase().includes("z\u00fcrich"));
  if (final.engine !== "rust-wasm") bad("worker search uses the Rust engine", [`engine=${final.engine}`]);
  else if (final.totalHits !== expected) bad("worker search hit count", [`expected=${expected} got=${final.totalHits}`]);
  else if (!offsetsOk) bad("worker search previews", ["a hit preview does not contain the needle"]);
  else ok("worker search", `${final.totalHits} hits, ${final.hits.length} with previews, engine=${final.engine}`);
}

console.log(`\n${checks - failures}/${checks} checks passed`);
process.exit(failures ? 1 : 0);
