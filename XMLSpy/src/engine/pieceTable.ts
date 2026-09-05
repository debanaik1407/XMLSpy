/**
 * Piece table edit buffer (browser twin of the `ropey`-backed buffer in
 * xmlspy-core). Original text is immutable; inserts go to an append-only
 * "add" buffer; the document is a sequence of pieces pointing into either.
 * Edits are O(pieces) here (O(log n) with the Rust rope); saves stream pieces.
 */
type Piece = { buf: 0 | 1; start: number; len: number };

export class PieceTable {
  private original: string;
  private add = "";
  private pieces: Piece[];
  private _length: number;
  private lineStartsCache: number[] | null = null;

  constructor(original: string) {
    this.original = original;
    this.pieces = original.length ? [{ buf: 0, start: 0, len: original.length }] : [];
    this._length = original.length;
  }

  get length() {
    return this._length;
  }

  private bufText(p: Piece): string {
    const src = p.buf === 0 ? this.original : this.add;
    return src.substr(p.start, p.len);
  }

  /** Split so that a piece boundary exists at `pos`; returns piece index at boundary. */
  private splitAt(pos: number): number {
    let acc = 0;
    for (let i = 0; i < this.pieces.length; i++) {
      const p = this.pieces[i];
      if (pos === acc) return i;
      if (pos < acc + p.len) {
        const left: Piece = { buf: p.buf, start: p.start, len: pos - acc };
        const right: Piece = { buf: p.buf, start: p.start + left.len, len: p.len - left.len };
        this.pieces.splice(i, 1, left, right);
        return i + 1;
      }
      acc += p.len;
    }
    return this.pieces.length;
  }

  insert(pos: number, text: string) {
    if (!text.length) return;
    const start = this.add.length;
    this.add += text;
    const idx = this.splitAt(pos);
    const prev = this.pieces[idx - 1];
    // coalesce sequential appends
    if (prev && prev.buf === 1 && prev.start + prev.len === start) prev.len += text.length;
    else this.pieces.splice(idx, 0, { buf: 1, start, len: text.length });
    this._length += text.length;
    this.lineStartsCache = null;
  }

  delete(pos: number, len: number) {
    if (len <= 0) return;
    const a = this.splitAt(pos);
    const b = this.splitAt(pos + len);
    this.pieces.splice(a, b - a);
    this._length -= len;
    this.lineStartsCache = null;
  }

  replace(pos: number, len: number, text: string) {
    this.delete(pos, len);
    this.insert(pos, text);
  }

  text(): string {
    let out = "";
    for (const p of this.pieces) out += this.bufText(p);
    return out;
  }

  /** Iterate pieces (streaming save). */
  *chunks(): Generator<string> {
    for (const p of this.pieces) yield this.bufText(p);
  }

  lineStarts(): number[] {
    if (this.lineStartsCache) return this.lineStartsCache;
    const starts = [0];
    let acc = 0;
    for (const p of this.pieces) {
      const t = this.bufText(p);
      let i = t.indexOf("\n");
      while (i >= 0) {
        starts.push(acc + i + 1);
        i = t.indexOf("\n", i + 1);
      }
      acc += t.length;
    }
    this.lineStartsCache = starts;
    return starts;
  }

  lineCount() {
    return this.lineStarts().length;
  }

  getLine(n: number): string {
    const ls = this.lineStarts();
    if (n < 0 || n >= ls.length) return "";
    const start = ls[n];
    const end = n + 1 < ls.length ? ls[n + 1] - 1 : this._length;
    return this.slice(start, end).replace(/\r$/, "");
  }

  slice(from: number, to: number): string {
    let out = "";
    let acc = 0;
    for (const p of this.pieces) {
      const pEnd = acc + p.len;
      if (pEnd > from && acc < to) {
        const s = Math.max(from, acc) - acc;
        const e = Math.min(to, pEnd) - acc;
        const src = p.buf === 0 ? this.original : this.add;
        out += src.substr(p.start + s, e - s);
      }
      if (pEnd >= to) break;
      acc = pEnd;
    }
    return out;
  }

  setLine(n: number, text: string) {
    const ls = this.lineStarts();
    if (n < 0 || n >= ls.length) return;
    const start = ls[n];
    const end = n + 1 < ls.length ? ls[n + 1] - 1 : this._length;
    this.replace(start, end - start, text);
  }

  insertLineAfter(n: number, text: string) {
    const ls = this.lineStarts();
    const pos = n + 1 < ls.length ? ls[n + 1] : this._length;
    if (n + 1 < ls.length) this.insert(pos, text + "\n");
    else this.insert(pos, "\n" + text);
  }

  deleteLine(n: number) {
    const ls = this.lineStarts();
    if (ls.length <= 1) {
      this.delete(0, this._length);
      return;
    }
    const start = ls[n];
    const end = n + 1 < ls.length ? ls[n + 1] : this._length;
    if (n + 1 < ls.length) this.delete(start, end - start);
    else this.delete(start - 1, end - start + 1);
  }

  pieceCount() {
    return this.pieces.length;
  }
}
