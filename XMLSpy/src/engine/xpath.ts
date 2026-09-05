import type { XmlDocument } from "./document";

/**
 * How unprefixed names in an expression are matched.
 *
 *  - "smart"  — unprefixed element names are bound to the document's default
 *               namespace (`xmlns="…"`), which is what people expect when they
 *               type `//Item` at a namespaced document. Attribute names are
 *               deliberately left alone: an unprefixed attribute has *no*
 *               namespace even when a default `xmlns` is in scope.
 *  - "strict" — literal XPath 1.0 semantics: an unprefixed name matches only
 *               nodes in no namespace. Empty results then explain why.
 */
export type NsMode = "smart" | "strict";

export type XPathResult = {
  kind: "nodes" | "value";
  value?: string;
  nodes: { id: number; name: string; line: number; path: string; preview: string }[];
  count: number;
  elapsedMs: number;
  engine: "native-xpath1" | "index-path";
  warning?: string;
  /** Namespace handling that was actually applied. */
  nsMode?: NsMode;
  /** Default namespace in scope on the document element, when there is one. */
  defaultNamespace?: string;
  /** The expression as handed to the evaluator, after namespace binding. */
  effectiveExpr?: string;
};

export type XPathOptions = { nsMode?: NsMode };

/** The browser's own XPath 1.0 result constants (aliased so the exported
 *  `XPathResult` *type* above cannot shadow them in value position). */
const DomXPath = globalThis.XPathResult;

const XPATH_KEYWORDS = new Set(["and", "or", "mod", "div", "node", "text", "comment", "processing-instruction"]);

const isNameStart = (c: string) => /[A-Za-z_]/.test(c) || c.charCodeAt(0) > 127;
const isNameChar = (c: string) => /[-A-Za-z0-9_.]/.test(c) || c.charCodeAt(0) > 127;

/**
 * Read a NameTest starting at `i`. A single `:` belongs to the name (it makes a
 * qualified `prefix:local` NameTest); `::` does not — that is an axis separator.
 */
function readName(expr: string, i: number): number {
  let j = i + 1;
  while (j < expr.length && isNameChar(expr[j])) j++;
  if (expr[j] === ":" && expr[j + 1] !== ":") {
    j++;
    while (j < expr.length && isNameChar(expr[j])) j++;
  }
  return j;
}

/**
 * Rewrite an XPath expression so every *unprefixed element NameTest* carries
 * `prefix:`. Leaves untouched: string literals, function and node-type names
 * (`count(`, `text(`), axis names (`child::`), attribute NameTests (`@Type`,
 * `attribute::Type`), already-qualified names (`o:Item`), wildcards, variables
 * (`$x`) and the operator keywords `and`, `or`, `mod`, `div`.
 *
 * Exported for `npm run test:xpath`.
 */
export function bindDefaultNamespace(expr: string, prefix: string): string {
  let out = "";
  let quote: string | null = null;
  let attrNext = false; // the next NameTest belongs to an attribute axis
  for (let i = 0; i < expr.length; ) {
    const c = expr[i];
    if (quote) {
      out += c;
      if (c === quote) quote = null;
      i++;
      continue;
    }
    if (c === '"' || c === "'") {
      quote = c;
      attrNext = false;
      out += c;
      i++;
      continue;
    }
    if (c === "@") {
      attrNext = true;
      out += c;
      i++;
      continue;
    }
    if (isNameStart(c) && (i === 0 || expr[i - 1] !== "$")) {
      const j = readName(expr, i);
      const name = expr.slice(i, j);
      let k = j;
      while (k < expr.length && /\s/.test(expr[k])) k++;
      // Axis names are consumed together with their "::" so the separator's
      // colons cannot clear the pending-attribute flag (attribute::Type).
      if (expr.slice(k, k + 2) === "::") {
        attrNext = name === "attribute";
        out += expr.slice(i, k + 2);
        i = k + 2;
        continue;
      }
      const keep = name.includes(":") || expr[k] === "(" /* function or node-type test */ || attrNext || XPATH_KEYWORDS.has(name);
      out += keep ? name : `${prefix}:${name}`;
      attrNext = false;
      i = j;
      continue;
    }
    if (!/[\s:]/.test(c)) attrNext = false;
    out += c;
    i++;
  }
  return out;
}

/** Namespaces declared on the document element (`xmlns=`, `xmlns:p=`). */
function rootNamespaces(dom: Document): { defaultNs: string; prefixes: Map<string, string> } {
  const prefixes = new Map<string, string>();
  let defaultNs = dom.documentElement?.namespaceURI ?? "";
  const attrs = dom.documentElement?.attributes;
  for (let i = 0; attrs && i < attrs.length; i++) {
    const a = attrs[i];
    if (a.nodeName === "xmlns" && a.value) defaultNs = defaultNs || a.value;
    else if (a.nodeName.startsWith("xmlns:") && a.value) prefixes.set(a.nodeName.slice(6), a.value);
  }
  return { defaultNs, prefixes };
}

/** Pick a bind prefix that cannot collide with one the document declares. */
function freePrefix(prefixes: Map<string, string>): string {
  for (let n = 0; ; n++) {
    const p = n === 0 ? "__dflt" : `__dflt${n}`;
    if (!prefixes.has(p)) return p;
  }
}

/**
 * Evaluate XPath against the document.
 *  - In-memory docs: full XPath 1.0 via the browser's native evaluator
 *    (DOMParser), namespace-aware — declared prefixes resolve, and in "smart"
 *    mode unprefixed element names are bound to the default namespace.
 *  - Large docs: index-backed evaluator for the streaming-friendly subset
 *    (/a/b, //b, *, [n], count(), plus @attr projection). Anything else
 *    reports a clear diagnostic instead of trying to materialize the DOM.
 */
export async function evaluateXPath(doc: XmlDocument, expr: string, opts: XPathOptions = {}): Promise<XPathResult> {
  const t0 = performance.now();
  const nsMode: NsMode = opts.nsMode ?? "smart";
  expr = expr.trim();
  if (!expr) return { kind: "value", value: "", nodes: [], count: 0, elapsedMs: 0, engine: "index-path", nsMode };

  if (doc.inMemory) {
    const text = await doc.fullText();
    const dom = new DOMParser().parseFromString(text, "application/xml");
    if (dom.querySelector?.("parsererror")) throw new Error("Document is not well-formed; fix errors first.");

    const { defaultNs, prefixes } = rootNamespaces(dom);
    const bind = defaultNs ? freePrefix(prefixes) : "";
    const allPrefixes = new Map(prefixes);
    let scannedAll = false;
    // A DOM XPathNSResolver only sees the prefix, never the context node, so a
    // document that re-declares a prefix deeper down resolves to the first
    // declaration found. Root-level declarations (the common case) are read
    // eagerly; the rest of the tree is walked only if a prefix needs it.
    const resolver = (prefix: string | null): string | null => {
      if (!prefix) return null;
      if (bind && prefix === bind) return defaultNs || null;
      const hit = allPrefixes.get(prefix);
      if (hit !== undefined) return hit;
      if (!scannedAll) {
        scannedAll = true;
        for (const el of Array.from(dom.getElementsByTagName("*"))) {
          const as = el.attributes;
          for (let i = 0; i < as.length; i++) {
            const a = as[i];
            if (a.nodeName.startsWith("xmlns:") && a.value && !allPrefixes.has(a.nodeName.slice(6))) allPrefixes.set(a.nodeName.slice(6), a.value);
          }
        }
      }
      return allPrefixes.get(prefix) ?? null;
    };

    let effective = nsMode === "smart" && defaultNs ? bindDefaultNamespace(expr, bind) : expr;
    const attempt = (e: string) => dom.evaluate(e, dom, resolver, DomXPath.ANY_TYPE, null);
    let r: globalThis.XPathResult;
    try {
      r = attempt(effective);
    } catch (err: any) {
      // A hand-written prefix we could not resolve, or a rewrite that does not
      // parse: fall back to the literal expression, then explain.
      if (effective !== expr) {
        try {
          r = attempt(expr);
          effective = expr;
        } catch {
          throw new Error(nsHint(err, defaultNs, allPrefixes));
        }
      } else throw new Error(nsHint(err, defaultNs, allPrefixes));
    }

    if (r.resultType === DomXPath.UNORDERED_NODE_ITERATOR_TYPE || r.resultType === DomXPath.ORDERED_NODE_ITERATOR_TYPE) {
      const nodes: XPathResult["nodes"] = [];
      // One pass: element → index id. (Previously an indexOf() per result node.)
      const all = Array.from(dom.getElementsByTagName("*"));
      const idOf = new Map<Element, number>();
      all.forEach((el, i) => idOf.set(el, i));
      const indexed = doc.index.elemDepth.length;
      let n: Node | null;
      let count = 0;
      while ((n = r.iterateNext())) {
        count++;
        if (nodes.length >= 2000) continue;
        const el: Element | null = n.nodeType === 1 ? (n as Element) : ((n as Attr).ownerElement ?? n.parentElement);
        const id = el ? (idOf.get(el) ?? -1) : -1;
        const known = id >= 0 && id < indexed;
        nodes.push({
          id,
          name: n.nodeType === 2 ? "@" + n.nodeName : n.nodeName,
          line: known ? (doc.index.elemLine[id] ?? 1) : 1,
          path: known ? doc.pathOf(id) : "",
          preview: (n.nodeType === 1 ? (n as Element).outerHTML : (n.textContent ?? "")).slice(0, 160),
        });
      }
      return {
        kind: "nodes",
        nodes,
        count,
        elapsedMs: performance.now() - t0,
        engine: "native-xpath1",
        warning: emptyNsWarning(count, nsMode, defaultNs, expr),
        nsMode,
        defaultNamespace: defaultNs || undefined,
        effectiveExpr: effective !== expr ? effective : undefined,
      };
    }
    let v = "";
    if (r.resultType === DomXPath.NUMBER_TYPE) v = String(r.numberValue);
    else if (r.resultType === DomXPath.STRING_TYPE) v = r.stringValue;
    else if (r.resultType === DomXPath.BOOLEAN_TYPE) v = String(r.booleanValue);
    // Only count() gets the namespace warning: any other value result (a
    // string(), a boolean(), a sum()) can legitimately be empty/false.
    const zeroCount = /^count\(/.test(expr) && r.resultType === DomXPath.NUMBER_TYPE && r.numberValue === 0 ? 0 : 1;
    return {
      kind: "value",
      value: v,
      nodes: [],
      count: 1,
      elapsedMs: performance.now() - t0,
      engine: "native-xpath1",
      warning: emptyNsWarning(zeroCount, nsMode, defaultNs, expr),
      nsMode,
      defaultNamespace: defaultNs || undefined,
      effectiveExpr: effective !== expr ? effective : undefined,
    };
  }
  return evalIndexPath(doc, expr, t0, nsMode);
}

function nsHint(err: any, defaultNs: string, prefixes: Map<string, string>): string {
  const base = err?.message || String(err);
  const known = [...prefixes.entries()].map(([p, u]) => `${p} → ${u}`).join(", ");
  const ns = defaultNs ? ` The document's default namespace is "${defaultNs}" — a prefix bound to it must be declared in the document to be usable here.` : "";
  return `${base}${known ? ` Declared prefixes: ${known}.` : ""}${ns}`;
}

/**
 * Why an empty node-set at a namespaced document is usually not a bug. Only
 * fires in "strict" mode, only when the expression really does contain an
 * unprefixed element NameTest (decided by the same rewriter smart mode uses,
 * so the two can never disagree).
 */
function emptyNsWarning(count: number, nsMode: NsMode, defaultNs: string, expr: string): string | undefined {
  if (count > 0 || !defaultNs || nsMode === "smart") return undefined;
  if (bindDefaultNamespace(expr, "__probe") === expr) return undefined;
  return `Empty result: this document has a default namespace (xmlns="${defaultNs}"), and in XPath 1.0 an unprefixed name matches only nodes in NO namespace — so "${expr}" selects nothing. Turn on "Match default namespace" (or use //*[local-name()='Name'], or a prefix the document declares).`;
}

type Step = { axis: "child" | "desc"; name: string; pos?: number };

function parseSteps(expr: string): { steps: Step[]; attr?: string; fn?: "count" } {
  let fn: "count" | undefined;
  let m = /^count\((.*)\)$/s.exec(expr);
  if (m) {
    fn = "count";
    expr = m[1].trim();
  }
  let attr: string | undefined;
  const am = /\/@([\w:.-]+|\*)$/.exec(expr);
  if (am) {
    attr = am[1];
    expr = expr.slice(0, am.index);
  }
  if (!expr.startsWith("/"))
    throw new Error("Large-file XPath subset requires an absolute path (e.g. /root/item[2] or //item). Full XPath 1.0 needs the document in memory (under 16 MiB).");
  const steps: Step[] = [];
  const re = /(\/\/?)([\w:.\-*]+)(?:\[(\d+)\])?/g;
  let s: RegExpExecArray | null;
  let consumed = 0;
  while ((s = re.exec(expr))) {
    if (s.index !== consumed) break;
    consumed = s.index + s[0].length;
    steps.push({ axis: s[1] === "//" ? "desc" : "child", name: s[2], pos: s[3] ? parseInt(s[3], 10) : undefined });
  }
  if (consumed !== expr.length || steps.length === 0)
    throw new Error(`Unsupported expression for index-backed evaluation near "${expr.slice(consumed, consumed + 20)}". Supported: /a/b, //b, *, [n], @attr, count().`);
  return { steps, attr, fn };
}

const localName = (q: string) => (q.includes(":") ? q.slice(q.indexOf(":") + 1) : q);

async function evalIndexPath(doc: XmlDocument, expr: string, t0: number, nsMode: NsMode): Promise<XPathResult> {
  const { steps, attr, fn } = parseSteps(expr);
  const ix = doc.index;
  let ctx: number[] = [-1]; // virtual document node
  const LIMIT = 500_000;
  let truncated = false;
  for (const st of steps) {
    const next: number[] = [];
    const byParent = new Map<number, number>();
    // Name ids this step accepts. The index stores raw QNames as written, so in
    // "smart" mode a bare local name also matches a prefixed element (and vice
    // versa) — the streaming twin of the DOM path's default-namespace binding.
    // An unknown name yields an EMPTY set: it must not fall through to the
    // wildcard sentinel (-1), which used to make //NoSuchThing match everything.
    let matchIds: Set<number> | null = null;
    if (st.name !== "*") {
      matchIds = new Set();
      const want = localName(st.name);
      for (let ni = 0; ni < ix.names.length; ni++) {
        const nm = ix.names[ni];
        if (nm === st.name || (nsMode === "smart" && localName(nm) === want)) matchIds.add(ni);
      }
    }
    if (st.axis === "child") {
      for (const c of ctx) {
        const kids = c < 0 ? doc.rootIds() : doc.childrenOf(c);
        let n = 0;
        for (const k of kids) {
          if (!matchIds || matchIds.has(ix.elemName[k])) {
            n++;
            if (st.pos === undefined || st.pos === n) next.push(k);
            if (next.length >= LIMIT) {
              truncated = true;
              break;
            }
          }
        }
      }
    } else {
      // descendant: scan the index range under each context node
      for (const c of ctx) {
        const from = c < 0 ? 0 : c + 1;
        const baseDepth = c < 0 ? -1 : ix.elemDepth[c];
        for (let j = from; j < ix.elemDepth.length; j++) {
          if (c >= 0 && ix.elemDepth[j] <= baseDepth) break;
          if (!matchIds || matchIds.has(ix.elemName[j])) {
            if (st.pos !== undefined) {
              const p = ix.elemParent[j];
              const n = (byParent.get(p) ?? 0) + 1;
              byParent.set(p, n);
              if (n !== st.pos) continue;
            }
            next.push(j);
            if (next.length >= LIMIT) {
              truncated = true;
              break;
            }
          }
        }
      }
    }
    ctx = next;
    if (!ctx.length) break;
  }
  const count = ctx.length;
  const warning = [
    truncated ? `Result truncated at ${LIMIT.toLocaleString()} nodes.` : "",
    ix.indexedElements < ix.totalElements ? `Index covers ${ix.indexedElements.toLocaleString()} of ${ix.totalElements.toLocaleString()} elements (depth-limited beyond budget); deep matches use streaming fallback in the Rust engine.` : "",
    nsMode === "smart" ? "Large-file mode matches the index's literal QNames (and bare local names); it does not resolve namespace declarations." : "",
  ]
    .filter(Boolean)
    .join(" ");
  if (fn === "count")
    return { kind: "value", value: String(count), nodes: [], count, elapsedMs: performance.now() - t0, engine: "index-path", warning, nsMode };
  const nodes: XPathResult["nodes"] = [];
  for (const id of ctx.slice(0, 500)) {
    const info = await doc.nodeInfo(id);
    let preview = "";
    if (attr) {
      const a = info.attrs.find((x) => attr === "*" || x[0] === attr || localName(x[0]) === attr);
      preview = a ? `${a[0]}="${a[1]}"` : "(no such attribute)";
    } else {
      preview = `<${info.name}${info.attrs.map(([k, v]) => ` ${k}="${v}"`).join("")}>${info.text.slice(0, 80)}`;
    }
    nodes.push({ id, name: attr ? "@" + attr : info.name, line: info.line, path: doc.pathOf(id), preview });
  }
  return { kind: "nodes", nodes, count, elapsedMs: performance.now() - t0, engine: "index-path", warning, nsMode };
}
