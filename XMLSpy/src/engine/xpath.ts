import type { XmlDocument } from "./document";

export type XPathResult = {
  kind: "nodes" | "value";
  value?: string;
  nodes: { id: number; name: string; line: number; path: string; preview: string }[];
  count: number;
  elapsedMs: number;
  engine: "native-xpath1" | "index-path";
  warning?: string;
};

/**
 * Evaluate XPath against the document.
 *  - In-memory docs: full XPath 1.0 via the browser's native evaluator (DOMParser).
 *  - Large docs: index-backed evaluator for the streaming-friendly subset
 *    (/a/b, //b, *, [n], count(), plus @attr projection). Anything else
 *    reports a clear diagnostic instead of trying to materialize the DOM.
 */
export async function evaluateXPath(doc: XmlDocument, expr: string): Promise<XPathResult> {
  const t0 = performance.now();
  expr = expr.trim();
  if (!expr) return { kind: "value", value: "", nodes: [], count: 0, elapsedMs: 0, engine: "index-path" };

  if (doc.inMemory) {
    try {
      const text = await doc.fullText();
      const dom = new DOMParser().parseFromString(text, "application/xml");
      const pe = dom.querySelector("parsererror");
      if (pe) throw new Error("Document is not well-formed; fix errors first.");
      const r = dom.evaluate(expr, dom, null, XPathResult.ANY_TYPE, null);
      if (r.resultType === XPathResult.UNORDERED_NODE_ITERATOR_TYPE || r.resultType === XPathResult.ORDERED_NODE_ITERATOR_TYPE) {
        const nodes: XPathResult["nodes"] = [];
        const elems = Array.from(dom.getElementsByTagName("*"));
        let n: Node | null;
        let count = 0;
        while ((n = r.iterateNext())) {
          count++;
          if (nodes.length >= 2000) continue;
          let el: Element | null = n.nodeType === 1 ? (n as Element) : (n as Attr).ownerElement ?? n.parentElement;
          const id = el ? elems.indexOf(el) : -1;
          nodes.push({
            id,
            name: n.nodeType === 2 ? "@" + n.nodeName : n.nodeName,
            line: id >= 0 ? doc.index.elemLine[id] : 1,
            path: id >= 0 ? doc.pathOf(id) : "",
            preview: (n.nodeType === 1 ? (n as Element).outerHTML : n.textContent ?? "").slice(0, 160),
          });
        }
        return { kind: "nodes", nodes, count, elapsedMs: performance.now() - t0, engine: "native-xpath1" };
      }
      let v = "";
      if (r.resultType === XPathResult.NUMBER_TYPE) v = String(r.numberValue);
      else if (r.resultType === XPathResult.STRING_TYPE) v = r.stringValue;
      else if (r.resultType === XPathResult.BOOLEAN_TYPE) v = String(r.booleanValue);
      return { kind: "value", value: v, nodes: [], count: 1, elapsedMs: performance.now() - t0, engine: "native-xpath1" };
    } catch (e: any) {
      throw new Error(e.message || String(e));
    }
  }
  return evalIndexPath(doc, expr, t0);
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
  if (!expr.startsWith("/")) throw new Error("Large-file XPath subset requires an absolute path (e.g. /root/item[2] or //item). Full XPath 3.1 runs in the Rust xmlspy-xpath engine.");
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

async function evalIndexPath(doc: XmlDocument, expr: string, t0: number): Promise<XPathResult> {
  const { steps, attr, fn } = parseSteps(expr);
  const ix = doc.index;
  let ctx: number[] = [-1]; // virtual document node
  const LIMIT = 500_000;
  let truncated = false;
  for (const st of steps) {
    const next: number[] = [];
    const byParent = new Map<number, number>();
    if (st.axis === "child") {
      for (const c of ctx) {
        const kids = c < 0 ? doc.rootIds() : doc.childrenOf(c);
        let n = 0;
        for (const k of kids) {
          if (st.name === "*" || doc.nameOf(k) === st.name) {
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
      const nameId = st.name === "*" ? -1 : ix.names.indexOf(st.name);
      if (nameId === -2) continue;
      for (const c of ctx) {
        const from = c < 0 ? 0 : c + 1;
        const baseDepth = c < 0 ? -1 : ix.elemDepth[c];
        for (let j = from; j < ix.elemDepth.length; j++) {
          if (c >= 0 && ix.elemDepth[j] <= baseDepth) break;
          if (nameId === -1 || ix.elemName[j] === nameId) {
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
  ]
    .filter(Boolean)
    .join(" ");
  if (fn === "count") return { kind: "value", value: String(count), nodes: [], count, elapsedMs: performance.now() - t0, engine: "index-path", warning };
  const nodes: XPathResult["nodes"] = [];
  for (const id of ctx.slice(0, 500)) {
    const info = await doc.nodeInfo(id);
    let preview = "";
    if (attr) {
      const a = info.attrs.find((x) => attr === "*" || x[0] === attr);
      preview = a ? `${a[0]}="${a[1]}"` : "(no such attribute)";
    } else {
      preview = `<${info.name}${info.attrs.map(([k, v]) => ` ${k}="${v}"`).join("")}>${info.text.slice(0, 80)}`;
    }
    nodes.push({ id, name: attr ? "@" + attr : info.name, line: info.line, path: doc.pathOf(id), preview });
  }
  return { kind: "nodes", nodes, count, elapsedMs: performance.now() - t0, engine: "index-path", warning };
}
