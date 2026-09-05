import type { ScanIndex, WfError } from "./scanner";
import { PieceTable } from "./pieceTable";

/** Tiny LRU (browser twin of the `lru` crate usage in xmlspy-core::cache). */
export class LRU<K, V> {
  private map = new Map<K, V>();
  constructor(private cap: number) {}
  get(k: K): V | undefined {
    const v = this.map.get(k);
    if (v !== undefined) {
      this.map.delete(k);
      this.map.set(k, v);
    }
    return v;
  }
  set(k: K, v: V) {
    if (this.map.has(k)) this.map.delete(k);
    this.map.set(k, v);
    if (this.map.size > this.cap) {
      const first = this.map.keys().next().value as K;
      this.map.delete(first);
    }
  }
  clear() {
    this.map.clear();
  }
  get size() {
    return this.map.size;
  }
}

export type NodeInfo = {
  id: number;
  name: string;
  depth: number;
  line: number;
  start: number;
  end: number;
  parent: number;
  attrs: [string, string][];
  text: string;
  hasChildren: boolean;
  childCount: number;
};

export type DocStats = {
  bytes: number;
  lines: number;
  elements: number;
  indexedElements: number;
  attributes: number;
  maxDepth: number;
  scanMs: number;
  openToFirstPaintMs: number;
  indexBytes: number;
  errors: number;
};

export type EditRecord = { line: number; before: string; after: string };

/**
 * The shared document model used by every view (Text/Grid/Schema/Tree).
 *
 * Two backends behind one interface:
 *  - Large mode (any size): the File stays on disk; lines are fetched on demand
 *    through the sparse line-checkpoint index (stride N) with an LRU block cache;
 *    edits are a line overlay; saves stream untouched byte ranges + overlays.
 *  - Memory mode (< 16 MiB): full piece-table buffer, insert/delete lines.
 */
export class XmlDocument {
  readonly id: string;
  readonly name: string;
  file: Blob;
  index: ScanIndex;
  errors: WfError[];
  stats: DocStats;
  readonly large: boolean;
  private pt: PieceTable | null = null;
  private overlay = new Map<number, string>();
  private cache = new LRU<number, string[]>(512); // 512 blocks × 32 lines
  private childCache = new LRU<number, number[]>(4096);
  undoStack: EditRecord[] = [];
  redoStack: EditRecord[] = [];
  dirty = false;
  version = 0;
  /** Bytes covered by the current index (== file.size once the background index completes). */
  indexedEnd: number;
  /** True while only the head chunk is indexed (progressive open). */
  provisional = false;

  static MEMORY_LIMIT = 16 * 1024 * 1024;

  constructor(name: string, file: Blob, index: ScanIndex, scanMs: number, firstPaintMs: number, text?: string, indexedEnd?: number) {
    this.id = Math.random().toString(36).slice(2);
    this.name = name;
    this.file = file;
    this.index = index;
    this.errors = index.errors;
    this.indexedEnd = indexedEnd ?? file.size;
    this.provisional = this.indexedEnd < file.size;
    this.large = file.size >= XmlDocument.MEMORY_LIMIT;
    if (!this.large && text !== undefined) this.pt = new PieceTable(text);
    this.stats = this.computeStats(index, scanMs, firstPaintMs);
  }

  private computeStats(index: ScanIndex, scanMs: number, firstPaintMs: number): DocStats {
    return {
      bytes: this.file.size,
      lines: index.lineCount,
      elements: index.totalElements,
      indexedElements: index.indexedElements,
      attributes: index.totalAttributes,
      maxDepth: index.maxDepth,
      scanMs,
      openToFirstPaintMs: firstPaintMs,
      indexBytes: index.checkpoints.byteLength + index.elemStart.byteLength * 3 + index.elemParent.byteLength * 2 + index.elemDepth.byteLength,
      errors: index.errorCount,
    };
  }

  /** Swap in the complete index produced by the background worker. */
  replaceIndex(index: ScanIndex, scanMs: number) {
    this.index = index;
    this.errors = index.errors;
    this.indexedEnd = this.file.size;
    this.provisional = false;
    this.stats = this.computeStats(index, scanMs, this.stats.openToFirstPaintMs);
    this.cache.clear();
    this.childCache.clear();
    this.version++;
  }

  /**
   * Materialize pending edits as the new base Blob (zero-copy parts for untouched ranges)
   * so the document can be re-indexed. Mirrors the Rust "commit + incremental reindex" step.
   */
  async commitEdits(): Promise<Blob> {
    const blob = await this.toBlob();
    this.file = blob;
    this.overlay.clear();
    this.cache.clear();
    this.childCache.clear();
    if (this.pt) this.pt = new PieceTable(await blob.text());
    return blob;
  }

  get cacheSize() {
    return this.cache.size;
  }

  get lineCount(): number {
    return this.pt ? this.pt.lineCount() : this.index.lineCount;
  }

  get inMemory() {
    return this.pt !== null;
  }

  // ---- line access ------------------------------------------------------
  private async readBlock(b: number): Promise<string[]> {
    const cached = this.cache.get(b);
    if (cached) return cached;
    const cp = this.index.checkpoints;
    const stride = this.index.stride;
    const start = cp[b];
    const end = b + 1 < cp.length ? cp[b + 1] : this.indexedEnd;
    const buf = await this.file.slice(start, end).arrayBuffer();
    let lines = new TextDecoder("utf-8").decode(buf).split("\n");
    if (b + 1 < cp.length) lines.pop(); // range ends exactly on '\n'
    else {
      const want = this.index.lineCount - b * stride;
      if (lines.length > want) lines.length = want;
    }
    for (let i = 0; i < lines.length; i++) if (lines[i].endsWith("\r")) lines[i] = lines[i].slice(0, -1);
    this.cache.set(b, lines);
    return lines;
  }

  /** Lines [from, to) — 0-based. Only touches the blocks that intersect. */
  async getLines(from: number, to: number): Promise<string[]> {
    const total = this.lineCount;
    from = Math.max(0, from);
    to = Math.min(total, to);
    if (to <= from) return [];
    if (this.pt) {
      const out: string[] = [];
      for (let i = from; i < to; i++) out.push(this.pt.getLine(i));
      return out;
    }
    const stride = this.index.stride;
    const out: string[] = [];
    const b0 = Math.floor(from / stride);
    const b1 = Math.floor((to - 1) / stride);
    for (let b = b0; b <= b1; b++) {
      const lines = await this.readBlock(b);
      const s = b === b0 ? from - b * stride : 0;
      const e = b === b1 ? to - b * stride : lines.length;
      for (let i = s; i < e; i++) {
        const ln = b * stride + i;
        const ov = this.overlay.get(ln);
        out.push(ov !== undefined ? ov : lines[i] ?? "");
      }
    }
    return out;
  }

  /** Read a raw byte range as text (used by lazy subtree materialization). */
  async readRange(start: number, end: number): Promise<string> {
    const buf = await this.file.slice(start, end).arrayBuffer();
    return new TextDecoder("utf-8").decode(buf);
  }

  async lineOfOffset(off: number): Promise<number> {
    const cp = this.index.checkpoints;
    let lo = 0,
      hi = cp.length - 1;
    while (lo < hi) {
      const mid = (lo + hi + 1) >> 1;
      if (cp[mid] <= off) lo = mid;
      else hi = mid - 1;
    }
    const buf = new Uint8Array(await this.file.slice(cp[lo], off).arrayBuffer());
    let c = 0;
    for (let i = 0; i < buf.length; i++) if (buf[i] === 10) c++;
    return lo * this.index.stride + c;
  }

  // ---- editing ----------------------------------------------------------
  setLine(n: number, text: string, record = true) {
    const before = this.pt ? this.pt.getLine(n) : this.overlay.get(n);
    if (this.pt) this.pt.setLine(n, text);
    else this.overlay.set(n, text);
    if (record) {
      this.undoStack.push({ line: n, before: before ?? "\u0000ORIG", after: text });
      this.redoStack = [];
    }
    this.dirty = true;
    this.version++;
  }
  insertLineAfter(n: number, text: string): boolean {
    if (!this.pt) return false;
    this.pt.insertLineAfter(n, text);
    this.dirty = true;
    this.version++;
    return true;
  }
  deleteLine(n: number): boolean {
    if (!this.pt) return false;
    this.pt.deleteLine(n);
    this.dirty = true;
    this.version++;
    return true;
  }
  undo(): number | null {
    const r = this.undoStack.pop();
    if (!r) return null;
    if (r.before === "\u0000ORIG") this.overlay.delete(r.line);
    else this.setLine(r.line, r.before, false);
    this.redoStack.push(r);
    this.version++;
    return r.line;
  }
  redo(): number | null {
    const r = this.redoStack.pop();
    if (!r) return null;
    this.setLine(r.line, r.after, false);
    this.undoStack.push(r);
    return r.line;
  }
  get editCount() {
    return this.pt ? this.undoStack.length : this.overlay.size;
  }

  /** Streaming save: untouched blocks are passed through as Blob slices (zero-copy). */
  async toBlob(): Promise<Blob> {
    if (this.pt) {
      return new Blob(Array.from(this.pt.chunks()), { type: "application/xml" });
    }
    const cp = this.index.checkpoints;
    const stride = this.index.stride;
    const dirtyBlocks = new Set<number>();
    for (const ln of this.overlay.keys()) dirtyBlocks.add(Math.floor(ln / stride));
    const parts: BlobPart[] = [];
    let runStart = 0;
    const sorted = Array.from(dirtyBlocks).sort((a, b) => a - b);
    for (const b of sorted) {
      const bStart = cp[b];
      const bEnd = b + 1 < cp.length ? cp[b + 1] : this.file.size;
      if (bStart > runStart) parts.push(this.file.slice(runStart, bStart));
      const lines = await this.readBlock(b);
      const rebuilt = lines.map((l, i) => this.overlay.get(b * stride + i) ?? l);
      const last = b + 1 >= cp.length;
      parts.push(rebuilt.join("\n") + (last ? "" : "\n"));
      runStart = bEnd;
    }
    if (runStart < this.file.size) parts.push(this.file.slice(runStart));
    return new Blob(parts, { type: "application/xml" });
  }

  async fullText(): Promise<string> {
    if (this.pt) return this.pt.text();
    return (await this.toBlob()).text();
  }

  // ---- structural navigation (lazy DOM) ---------------------------------
  private _roots: { index: ScanIndex; ids: number[] } | null = null;
  rootIds(): number[] {
    if (this._roots && this._roots.index === this.index) return this._roots.ids;
    const ids: number[] = [];
    const d = this.index.elemDepth;
    for (let i = 0; i < d.length && ids.length < 50; i++) if (d[i] === 0) ids.push(i);
    this._roots = { index: this.index, ids };
    return ids;
  }

  childrenOf(id: number, limit = 200_000): number[] {
    const cached = this.childCache.get(id);
    if (cached) return cached;
    const d = this.index.elemDepth;
    const target = d[id] + 1;
    const out: number[] = [];
    for (let j = id + 1; j < d.length; j++) {
      const dj = d[j];
      if (dj <= d[id]) break;
      if (dj === target) {
        out.push(j);
        if (out.length >= limit) break;
      }
    }
    this.childCache.set(id, out);
    return out;
  }

  hasChildren(id: number): boolean {
    const d = this.index.elemDepth;
    return id + 1 < d.length && d[id + 1] === d[id] + 1;
  }

  nameOf(id: number) {
    return this.index.names[this.index.elemName[id]];
  }

  /** Materialize a single element's start tag attributes + direct text (bounded read). */
  async nodeInfo(id: number): Promise<NodeInfo> {
    const ix = this.index;
    const start = ix.elemStart[id];
    const end = ix.elemEnd[id] > 0 ? ix.elemEnd[id] : start + 4096;
    const hasChildren = this.hasChildren(id);
    const readEnd = Math.min(end, start + (hasChildren ? 64 * 1024 : 256 * 1024));
    const frag = await this.readRange(start, readEnd);
    const attrs = parseStartTagAttrs(frag);
    let text = "";
    if (!hasChildren) {
      const gt = frag.indexOf(">");
      if (gt >= 0 && frag[gt - 1] !== "/") {
        const lt = frag.lastIndexOf("</");
        const inner = frag.slice(gt + 1, lt > gt ? lt : undefined).replace(/<!\[CDATA\[([\s\S]*?)\]\]>/g, "$1").replace(/<!--[\s\S]*?-->/g, "");
        if (/<[A-Za-z_:]/.test(inner)) {
          // Children exist in the file but were not indexed (depth budget exceeded) — lazy subtree
          // materialization in the Rust engine would parse this fragment on demand.
          const n = (inner.match(/<[A-Za-z_:]/g) || []).length;
          text = `[${n}${readEnd < end ? "+" : ""} descendant element(s) beyond index depth budget]`;
        } else text = decodeEntities(inner.trim());
      }
    }
    const kids = hasChildren ? this.childrenOf(id) : [];
    return {
      id,
      name: this.nameOf(id),
      depth: ix.elemDepth[id],
      line: ix.elemLine[id],
      start,
      end,
      parent: ix.elemParent[id],
      attrs,
      text,
      hasChildren,
      childCount: kids.length,
    };
  }

  /** XPath-like absolute path for an indexed element (used by Info panel + XPath builder). */
  pathOf(id: number): string {
    const parts: string[] = [];
    let cur = id;
    while (cur >= 0) {
      const name = this.nameOf(cur);
      const parent = this.index.elemParent[cur];
      let pos = 1;
      if (parent >= 0) {
        const sibs = this.childrenOf(parent);
        let n = 0;
        for (const s of sibs) {
          if (this.nameOf(s) === name) {
            n++;
            if (s === cur) {
              pos = n;
              break;
            }
          }
        }
      }
      parts.unshift(`${name}[${pos}]`);
      cur = parent;
    }
    return "/" + parts.join("/");
  }

  invalidateAfterEdit() {
    this.cache.clear();
    this.childCache.clear();
  }
}

export function parseStartTagAttrs(frag: string): [string, string][] {
  const attrs: [string, string][] = [];
  const m = /^<([^\s/>]+)([^>]*?)\/?>/s.exec(frag);
  if (!m) return attrs;
  const re = /([^\s=]+)\s*=\s*("([^"]*)"|'([^']*)')/g;
  let a: RegExpExecArray | null;
  while ((a = re.exec(m[2]))) attrs.push([a[1], decodeEntities(a[3] ?? a[4] ?? "")]);
  return attrs;
}

export function decodeEntities(s: string): string {
  return s
    .replace(/&lt;/g, "<")
    .replace(/&gt;/g, ">")
    .replace(/&quot;/g, '"')
    .replace(/&apos;/g, "'")
    .replace(/&#x([0-9a-fA-F]+);/g, (_, h) => String.fromCodePoint(parseInt(h, 16)))
    .replace(/&#(\d+);/g, (_, d) => String.fromCodePoint(parseInt(d, 10)))
    .replace(/&amp;/g, "&");
}

export function escapeXml(s: string): string {
  return s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
}

export function fmtBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KiB`;
  if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)} MiB`;
  return `${(n / 1024 / 1024 / 1024).toFixed(2)} GiB`;
}

export function fmtNum(n: number): string {
  return n.toLocaleString("en-US");
}
