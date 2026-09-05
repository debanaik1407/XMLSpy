#!/usr/bin/env node
/**
 * XPath conformance harness.
 *
 *   npm run test:xpath
 *
 * Runs the *real* `src/engine/xpath.ts` (compiled by esbuild, exactly like
 * `scripts/parity.mjs` compiles the scanner) against a spec-conformant XPath
 * 1.0 evaluator, on documents indexed by the real `src/engine/scanner.ts` and
 * wrapped in the real `src/engine/document.ts`.
 *
 * Node has no DOM and no `document.evaluate()`, so a thin shim supplies them
 * from `@xmldom/xmldom` + `xpath`. The shim deliberately mirrors browser
 * behaviour: a *function* namespace resolver is accepted, and a parse failure
 * surfaces as a `<parsererror>` element, because that is what xpath.ts looks
 * for.
 *
 * What it locks down:
 *   phase 1 — the default-namespace rewriter (element names bound, attribute
 *             names / functions / axes / literals / keywords left alone)
 *   phase 2 — the native DOM path in "smart" mode on the namespaced sample
 *   phase 3 — the native DOM path in "strict" mode (empty results must explain
 *             themselves instead of silently returning 0)
 *   phase 4 — prefixes the document declares must resolve (they used to throw:
 *             the resolver was `null`)
 *   phase 5 — documents with no default namespace must be unaffected
 *   phase 6 — malformed documents must report, not crash
 *   phase 7 — the large-file index path, where an unknown name used to match
 *             EVERY element (`if (nameId === -2)` — indexOf never returns -2)
 */
import { build } from "esbuild";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { DOMParser as XmlDomParser, XMLSerializer } from "@xmldom/xmldom";
import xpath from "xpath";

const out = mkdtempSync(join(tmpdir(), "xmlspy-xpath-"));
process.on("exit", () => rmSync(out, { recursive: true, force: true }));

// ---------------------------------------------------------------- browser shim
const XPathResultShim = {
  ANY_TYPE: 0,
  NUMBER_TYPE: 1,
  STRING_TYPE: 2,
  BOOLEAN_TYPE: 3,
  UNORDERED_NODE_ITERATOR_TYPE: 4,
  ORDERED_NODE_ITERATOR_TYPE: 5,
  UNORDERED_NODE_SNAPSHOT_TYPE: 6,
  ORDERED_NODE_SNAPSHOT_TYPE: 7,
  ANY_UNORDERED_NODE_TYPE: 8,
  FIRST_ORDERED_NODE_TYPE: 9,
};
globalThis.XPathResult = XPathResultShim;

let outerHtmlPatched = false;
globalThis.DOMParser = class {
  parseFromString(text, type) {
    // A browser never throws here: it hands back a document whose root is
    // <parsererror>. xmldom raises fatalError instead, so catch it and present
    // the browser's shape — that is what xpath.ts tests for.
    const problems = [];
    let doc = null;
    try {
      doc = new XmlDomParser({
        onError: (level, msg) => {
          problems.push({ level, msg });
          if (level === "fatalError") throw new Error(String(msg));
        },
      }).parseFromString(text, type);
    } catch (e) {
      problems.push({ level: "fatalError", msg: e.message });
      doc = null;
    }
    const fatal = problems.find((p) => p.level === "fatalError");
    if (!doc) {
      return {
        documentElement: null,
        querySelector: (sel) => (sel === "parsererror" ? { nodeName: "parsererror", textContent: String(fatal?.msg ?? "") } : null),
        evaluate: () => {
          throw new Error("Document is not well-formed");
        },
      };
    }
    if (!outerHtmlPatched && doc.documentElement) {
      outerHtmlPatched = true;
      const proto = Object.getPrototypeOf(doc.documentElement);
      Object.defineProperty(proto, "outerHTML", {
        get() {
          return new XMLSerializer().serializeToString(this);
        },
        configurable: true,
      });
    }
    doc.querySelector = (sel) => (sel === "parsererror" && fatal ? { nodeName: "parsererror", textContent: String(fatal.msg) } : null);
    // Browsers accept a bare function as the NS resolver; `xpath` wants an object.
    doc.evaluate = (expr, ctx, resolver, resultType) =>
      xpath.evaluate(expr, ctx, typeof resolver === "function" ? { lookupNamespaceURI: resolver } : resolver, resultType);
    return doc;
  }
};

// ---------------------------------------------------------------- real sources
// Bundled (not compiled file-by-file) so the extensionless relative imports
// inside src/ resolve, and so the code under test is the real thing — the same
// modules the app imports, pulled through one esbuild entry.
await build({
  stdin: {
    contents: [
      `export { evaluateXPath, bindDefaultNamespace } from "./src/engine/xpath";`,
      `export { createScanner } from "./src/engine/scanner";`,
      `export { XmlDocument } from "./src/engine/document";`,
      `export { SAMPLE_ORDERS, SAMPLE_BROKEN, SAMPLE_XSD, generateCorpus } from "./src/engine/corpus";`,
    ].join("\n"),
    resolveDir: process.cwd(),
    loader: "ts",
  },
  outfile: join(out, "bundle.mjs"),
  format: "esm",
  bundle: true,
  target: "es2022",
  logLevel: "error",
});
const { evaluateXPath, bindDefaultNamespace, createScanner, XmlDocument, SAMPLE_ORDERS, SAMPLE_BROKEN, SAMPLE_XSD, generateCorpus } = await import(pathToFileURL(join(out, "bundle.mjs")).href);

const enc = new TextEncoder();

/** Build a real XmlDocument with a real scanner index. `memory:false` forces the
 *  large-file (index-backed) XPath path. */
function makeDoc(name, text, memory = true) {
  const bytes = enc.encode(text);
  const s = createScanner({ maxIndexed: 100000, stride: 32, maxErrors: 100 });
  s.feed(bytes, 0);
  s.finish(bytes.length);
  const blob = new Blob([text], { type: "application/xml" });
  return new XmlDocument(name, blob, s.result(), 1, 1, memory ? text : undefined);
}

// ---------------------------------------------------------------- assertions
let checks = 0;
let failures = 0;
const ok = (label, detail = "") => {
  checks++;
  console.log(`  \u2713 ${label}${detail ? `  ${detail}` : ""}`);
};
const bad = (label, detail) => {
  checks++;
  failures++;
  console.log(`  \u2717 ${label}`);
  for (const d of Array.isArray(detail) ? detail : [detail]) console.log(`      ${d}`);
};
const expect = (label, actual, wanted) => (actual === wanted ? ok(label, `${JSON.stringify(wanted)}`) : bad(label, [`expected ${JSON.stringify(wanted)}`, `got      ${JSON.stringify(actual)}`]));
const describe = (r) => (r.kind === "value" ? `value ${r.value}` : `${r.count} node(s)`);
async function evalCase(label, doc, expr, opts, wanted) {
  try {
    const r = await evaluateXPath(doc, expr, opts);
    const got = r.kind === "value" ? `value ${r.value}` : `${r.count} node(s)`;
    if (typeof wanted === "function") {
      const why = wanted(r, got);
      if (why === true) ok(label, got);
      else bad(label, [String(why), `result: ${got}`]);
    } else if (got === wanted) ok(label, got);
    else bad(label, [`expected ${wanted}`, `got      ${got}`]);
    return r;
  } catch (e) {
    if (wanted instanceof RegExp) {
      if (wanted.test(e.message)) return ok(label, `throws /${wanted.source.slice(0, 40)}/`);
      return bad(label, [`expected error matching ${wanted}`, `got "${e.message}"`]);
    }
    return bad(label, [`expected ${wanted}`, `threw "${e.message}"`]);
  }
}

const NS = "urn:xmlspy:orders";
const orders = makeDoc("PurchaseOrders.xml", SAMPLE_ORDERS);
console.log(`\nsample: PurchaseOrders.xml — ${orders.index.totalElements} elements, default xmlns="${orders.index.names.length ? NS : ""}"\n`);

// ---------------------------------------------------------------- phase 1
console.log("phase 1 — default-namespace rewriter (pure function)");
{
  const b = (e) => bindDefaultNamespace(e, "o");
  const cases = [
    ["//Item", "//o:Item"],
    ["//Item/@PartNumber", "//o:Item/@PartNumber"], // attributes have NO namespace
    ["count(//Item)", "count(//o:Item)"], // function name untouched
    ["//*[local-name()='Item']", "//*[local-name()='Item']"], // wildcard + literal + function
    ['//Address[@Type="Shipping"]', '//o:Address[@Type="Shipping"]'],
    ["child::Item", "child::o:Item"], // axis name untouched
    ["attribute::Type", "attribute::Type"], // attribute axis untouched
    ["@Type", "@Type"],
    ["//o:Item", "//o:Item"], // already qualified
    ["/a/b[2]/c", "/o:a/o:b[2]/o:c"],
    ["//Item[Quantity > 1 and USPrice < 100]", "//o:Item[o:Quantity > 1 and o:USPrice < 100]"], // `and` is an operator
    ["5 div 2", "5 div 2"],
    ["//Item/text()", "//o:Item/text()"], // node-type test untouched
    ["//*[1]", "//*[1]"],
    ["//Item[not(Comment)]", "//o:Item[not(o:Comment)]"],
    ["string(//Item[1]/ProductName)", "string(//o:Item[1]/o:ProductName)"],
    ["//Item[contains(@PartNumber, 'AA')]", "//o:Item[contains(@PartNumber, 'AA')]"],
    ["$x/Item", "$x/o:Item"], // variable reference untouched
    // regressions: "::" used to leak into the pending-attribute flag, and a
    // qualified name used to be read as two names ("//o:Item" → "//o:o:o:Item")
    ["attribute ::Type", "attribute ::Type"], // whitespace before the axis separator
    ["child ::Item", "child ::o:Item"],
    ["//a:b/c:d", "//a:b/c:d"], // both steps already qualified
    ["//a:b/Item", "//a:b/o:Item"],
    ["ns:*", "ns:*"], // qualified wildcard
    ["@*/Item", "@*/o:Item"],
    ["//Item[@id][2]", "//o:Item[@id][2]"],
    ["self::node()", "self::node()"],
    ["//Item/..", "//o:Item/.."],
  ];
  for (const [input, want] of cases) {
    const got = b(input);
    if (got === want) ok(`rewrite ${input}`);
    else bad(`rewrite ${input}`, [`expected ${want}`, `got      ${got}`]);
  }
}

// ---------------------------------------------------------------- phase 2
console.log("\nphase 2 — native XPath 1.0, smart mode (the panel's five chips)");
{
  const chips = ["//PurchaseOrder[1]", "count(//Item)", "/PurchaseOrders/PurchaseOrder[2]/Items/Item", "//Item/@PartNumber", "//*[1]"];
  for (const c of chips)
    await evalCase(`chip ${c} is non-empty`, orders, c, { nsMode: "smart" }, (x) =>
      x.count > 0 && x.value !== "0" ? true : `EMPTY — ${describe(x)}`,
    );
  await evalCase("//PurchaseOrder[1] finds the namespaced element", orders, "//PurchaseOrder[1]", { nsMode: "smart" }, "1 node(s)");
  await evalCase("count(//Item)", orders, "count(//Item)", { nsMode: "smart" }, "value 3");
  await evalCase("deep absolute path", orders, "/PurchaseOrders/PurchaseOrder[2]/Items/Item", { nsMode: "smart" }, "1 node(s)");
  await evalCase("//Item/@PartNumber (attribute NOT namespaced)", orders, "//Item/@PartNumber", { nsMode: "smart" }, "3 node(s)");
  await evalCase("//Address[@Type='Shipping']", orders, "//Address[@Type='Shipping']", { nsMode: "smart" }, "2 node(s)");
  await evalCase("//*[1] was already fine", orders, "//*[1]", { nsMode: "smart" }, "13 node(s)");
  await evalCase("predicate with and + number()", orders, "count(//Item[Quantity > 1 and USPrice < 100])", { nsMode: "smart" }, "value 1");
  await evalCase("string(//Item[1]/ProductName)", orders, "string(//Item[1]/ProductName)", { nsMode: "smart" }, "value Lawnmower");
  await evalCase("count(//Address[@Type='Billing'])", orders, "count(//Address[@Type='Billing'])", { nsMode: "smart" }, "value 2");
  await evalCase("CDATA text via //DeliveryNotes", orders, "string(//PurchaseOrder[2]/DeliveryNotes)", { nsMode: "smart" }, (r, got) => (got.startsWith("value Signature required") ? true : `got ${got}`));
  const r = await evalCase("smart mode reports what it evaluated", orders, "//Item", { nsMode: "smart" }, "3 node(s)");
  expect("  result.defaultNamespace", r.defaultNamespace, NS);
  expect("  result.nsMode", r.nsMode, "smart");
  if (r.effectiveExpr && /:[A-Za-z]*Item$/.test(r.effectiveExpr) && r.effectiveExpr !== "//Item") ok("  result.effectiveExpr shows the binding", r.effectiveExpr);
  else bad("  result.effectiveExpr shows the binding", [`got ${JSON.stringify(r.effectiveExpr)}`]);
  await evalCase("click-to-navigate: line numbers are real", orders, "//PurchaseOrder[2]", { nsMode: "smart" }, (r2) => {
    const n = r2.nodes[0];
    if (!n) return "no node";
    if (n.line < 2) return `line ${n.line} looks unresolved`;
    if (!n.path.startsWith("/PurchaseOrders[1]/PurchaseOrder[2]")) return `path ${n.path}`;
    if (!n.preview.includes("<PurchaseOrder")) return `preview ${n.preview.slice(0, 40)}`;
    return true;
  });
}

// ---------------------------------------------------------------- phase 3
console.log("\nphase 3 — native XPath 1.0, strict mode (literal XPath 1.0 semantics)");
{
  await evalCase("//PurchaseOrder[1] selects nothing", orders, "//PurchaseOrder[1]", { nsMode: "strict" }, "0 node(s)");
  await evalCase("count(//Item) is 0", orders, "count(//Item)", { nsMode: "strict" }, "value 0");
  const r = await evalCase("empty node-set explains why", orders, "//PurchaseOrder[1]", { nsMode: "strict" }, (x) =>
    x.warning && x.warning.includes(NS) && /default namespace/i.test(x.warning) ? true : `warning=${JSON.stringify(x.warning)}`,
  );
  if (r?.warning) ok("  warning text names the xmlns", r.warning.slice(0, 72) + "…");
  await evalCase("count() 0 also warns", orders, "count(//Item)", { nsMode: "strict" }, (x) => (x.warning ? true : "no warning"));
  await evalCase("//*[1] needs no warning", orders, "//*[1]", { nsMode: "strict" }, (x) => (x.count === 13 && !x.warning ? true : `${x.count} nodes, warning=${x.warning}`));
  await evalCase("documented workaround local-name()", orders, "//*[local-name()='Item']", { nsMode: "strict" }, "3 node(s)");
  await evalCase("a namespace-free expression must NOT warn", orders, "count(//*)", { nsMode: "strict" }, (x) =>
    !x.warning && Number(x.value) === orders.index.totalElements ? true : `${describe(x)} warning=${x.warning} (scanner counted ${orders.index.totalElements})`,
  );
  await evalCase("count(//*) agrees with the Rust/TS scanner", orders, "count(//*)", { nsMode: "smart" }, `value ${orders.index.totalElements}`);
}

// ---------------------------------------------------------------- phase 4
console.log("\nphase 4 — prefixes the document declares (resolver used to be null)");
{
  const prefixed = makeDoc(
    "prefixed.xml",
    `<?xml version="1.0"?><o:Root xmlns:o="${NS}" xmlns:m="urn:meta"><o:Item m:tag="x">a</o:Item><o:Item>b</o:Item></o:Root>`,
  );
  await evalCase("//o:Item resolves in strict mode", prefixed, "//o:Item", { nsMode: "strict" }, "2 node(s)");
  await evalCase("//o:Item resolves in smart mode", prefixed, "//o:Item", { nsMode: "smart" }, "2 node(s)");
  await evalCase("//o:Item/@m:tag (namespaced attribute)", prefixed, "//o:Item/@m:tag", { nsMode: "strict" }, "1 node(s)");
  await evalCase("no default ns here → smart is a no-op", prefixed, "//o:Item", { nsMode: "smart" }, (x) => (x.effectiveExpr === undefined ? true : `rewrote to ${x.effectiveExpr}`));
  await evalCase("unknown prefix fails with a helpful message", prefixed, "//zzz:Item", { nsMode: "strict" }, /Declared prefixes|not a valid XPath|namespace/i);
  const deep = makeDoc("deep.xml", `<?xml version="1.0"?><Root xmlns="urn:outer"><Inner xmlns:d="urn:deep"><d:Leaf>v</d:Leaf></Inner></Root>`);
  await evalCase("prefix declared below the root still resolves", deep, "//d:Leaf", { nsMode: "strict" }, "1 node(s)");
}

// ---------------------------------------------------------------- phase 5
console.log("\nphase 5 — documents without a default namespace are untouched");
{
  const plain = makeDoc("plain.xml", `<?xml version="1.0"?><root><item id="1">one</item><item id="2">two</item></root>`);
  await evalCase("/root/item (smart)", plain, "/root/item", { nsMode: "smart" }, "2 node(s)");
  await evalCase("/root/item (strict)", plain, "/root/item", { nsMode: "strict" }, "2 node(s)");
  await evalCase("//item/@id", plain, "//item/@id", { nsMode: "smart" }, "2 node(s)");
  await evalCase("count(//item[@id='2'])", plain, "count(//item[@id='2'])", { nsMode: "smart" }, "value 1");
  await evalCase("string(/root/item[1])", plain, "string(/root/item[1])", { nsMode: "smart" }, "value one");
  await evalCase("//missing", plain, "//missing", { nsMode: "smart" }, "0 node(s)");
  const r = await evalCase("no rewrite, no ns metadata", plain, "//item", { nsMode: "smart" }, (x) => (x.effectiveExpr === undefined && x.defaultNamespace === undefined && !x.warning ? true : `${JSON.stringify({ e: x.effectiveExpr, d: x.defaultNamespace, w: x.warning })}`));
  if (r) ok("  defaultNamespace/effectiveExpr stay empty");
  const xsd = makeDoc("orders.xsd", SAMPLE_XSD);
  await evalCase("orders.xsd: //xs:element (prefix declared on root)", xsd, "count(//xs:element)", { nsMode: "strict" }, (x) => (Number(x.value) > 5 ? true : `count=${x.value}`));
  await evalCase("orders.xsd: smart binds the default ns too", xsd, "//xs:complexType/xs:sequence/xs:element", { nsMode: "smart" }, (x) => (x.count > 5 ? true : `${x.count} nodes`));
}

// ---------------------------------------------------------------- phase 6
console.log("\nphase 6 — malformed documents report instead of crashing");
{
  const broken = makeDoc("catalog-broken.xml", SAMPLE_BROKEN);
  await evalCase("catalog-broken.xml → clear diagnostic", broken, "//book", { nsMode: "smart" }, /not well-formed/i);
  await evalCase("empty expression is a no-op", orders, "   ", { nsMode: "smart" }, (x) => (x.kind === "value" && x.value === "" && x.count === 0 ? true : JSON.stringify(x)));
  await evalCase("relative path in large-file mode is rejected", makeDoc("big.xml", SAMPLE_ORDERS, false), "Item", { nsMode: "smart" }, /absolute path/i);
  await evalCase("garbage in large-file mode names the offender", makeDoc("big.xml", SAMPLE_ORDERS, false), "/a/b[(@x)]", { nsMode: "smart" }, /Unsupported expression/i);
}

// ---------------------------------------------------------------- phase 7
console.log("\nphase 7 — large-file index path (unknown names must match NOTHING)");
{
  const big = makeDoc("corpus.xml", SAMPLE_ORDERS, false);
  await evalCase("engine is index-backed", big, "//Item", { nsMode: "smart" }, (x) => (x.engine === "index-path" ? true : `engine=${x.engine}`));
  await evalCase("//Item", big, "//Item", { nsMode: "smart" }, "3 node(s)");
  await evalCase("//NoSuchElement → 0 (was: every element)", big, "//NoSuchElement", { nsMode: "smart" }, "0 node(s)");
  await evalCase("//TotallyMadeUp → 0", big, "//TotallyMadeUp", { nsMode: "smart" }, "0 node(s)");
  await evalCase("count(//DoesNotExist) → 0", big, "count(//DoesNotExist)", { nsMode: "smart" }, "value 0");
  await evalCase("count(//DoesNotExist) → 0 in strict mode too", big, "count(//DoesNotExist)", { nsMode: "strict" }, "value 0");
  await evalCase("//Item still 3 in strict mode (index is QName-literal)", big, "count(//Item)", { nsMode: "strict" }, "value 3");
  await evalCase("/PurchaseOrders/PurchaseOrder[2]", big, "/PurchaseOrders/PurchaseOrder[2]", { nsMode: "smart" }, "1 node(s)");
  await evalCase("//PurchaseOrder[1] (positional, descendant axis)", big, "//PurchaseOrder[1]", { nsMode: "smart" }, "1 node(s)");
  await evalCase("//*[1]", big, "//*[1]", { nsMode: "smart" }, (x) => (x.count > 0 ? true : `${x.count} nodes`));
  await evalCase("count(//Item/@PartNumber) projection", big, "//Item/@PartNumber", { nsMode: "smart" }, (x) =>
    x.count === 3 && x.nodes[0]?.preview.startsWith('PartNumber="') ? true : `${x.count} nodes, preview=${x.nodes[0]?.preview}`,
  );
  await evalCase("attribute matched by local name", big, "//Item/@Part", { nsMode: "smart" }, (x) => (x.nodes[0]?.preview === "(no such attribute)" ? true : `preview=${x.nodes[0]?.preview}`));
  await evalCase("node previews + paths are populated", big, "//Item[1]", { nsMode: "smart" }, (x) => {
    const n = x.nodes[0];
    if (!n) return "no node";
    if (!n.path.startsWith("/PurchaseOrders[1]/")) return `path ${n.path}`;
    if (!n.preview.startsWith("<Item ")) return `preview ${n.preview.slice(0, 40)}`;
    if (!(n.line > 1)) return `line ${n.line}`;
    return true;
  });

  const pfx = makeDoc(
    "prefixed-big.xml",
    `<?xml version="1.0"?><o:Root xmlns:o="${NS}"><o:Item>1</o:Item><o:Item>2</o:Item><o:Other>3</o:Other></o:Root>`,
    false,
  );
  await evalCase("index smart mode matches a bare local name", pfx, "count(//Item)", { nsMode: "smart" }, "value 2");
  await evalCase("index smart mode matches the literal QName", pfx, "count(//o:Item)", { nsMode: "smart" }, "value 2");
  await evalCase("index strict mode is QName-literal only", pfx, "count(//Item)", { nsMode: "strict" }, "value 0");
  await evalCase("index strict mode finds the QName", pfx, "count(//o:Item)", { nsMode: "strict" }, "value 2");
  await evalCase("index path warns it is QName-literal", pfx, "//Item", { nsMode: "smart" }, (x) => (x.warning && /literal QNames/.test(x.warning) ? true : `warning=${x.warning}`));
}

// ---------------------------------------------------------------- phase 8
console.log("\nphase 8 — a generated corpus behaves the same as the sample");
{
  const { blob, name } = generateCorpus(2);
  const text = await blob.text();
  const corpus = makeDoc(name, text, false);
  await evalCase(`${name}: count(//PurchaseOrder)`, corpus, "count(//PurchaseOrder)", { nsMode: "smart" }, (x) => (Number(x.value) > 0 ? true : `count=${x.value}`));
  await evalCase(`${name}: count(//Nope) is 0`, corpus, "count(//Nope)", { nsMode: "smart" }, "value 0");
  await evalCase(`${name}: index is complete`, corpus, "//PurchaseOrder[1]", { nsMode: "smart" }, (x) => (x.warning && /Index covers/.test(x.warning) ? `index incomplete: ${x.warning}` : true));
}

console.log(`\n${checks - failures}/${checks} checks passed`);
process.exit(failures ? 1 : 0);
