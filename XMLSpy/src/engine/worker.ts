import { createScanner, type ScanIndex, type ScannerConfig } from "./scanner";

/**
 * Worker body. SELF-CONTAINED (serialized via toString). Receives a Blob/File
 * and reads it in page-aligned chunks with `Blob.slice()` — the browser
 * analogue of memmap2 chunked reads — so the file is never fully in memory.
 */
function workerMain(self: any, makeScanner: typeof createScanner) {
  var cancelled = false;
  var busy = false;

  self.onmessage = function (ev: MessageEvent) {
    var m = ev.data;
    if (m.type === "cancel") {
      cancelled = true;
      return;
    }
    if (busy) {
      self.postMessage({ type: "error", id: m.id, message: "worker busy" });
      return;
    }
    if (m.type === "scan") runScan(m);
    else if (m.type === "search") runSearch(m);
  };

  async function runScan(m: any) {
    busy = true;
    cancelled = false;
    var file: Blob = m.file;
    var chunk: number = m.chunkSize;
    var size = file.size;
    var scanner = makeScanner({ maxIndexed: m.maxIndexed, stride: m.stride, maxErrors: m.maxErrors });
    var t0 = performance.now();
    var off = 0;
    var lastPost = 0;
    try {
      while (off < size) {
        if (cancelled) {
          self.postMessage({ type: "cancelled", id: m.id });
          busy = false;
          return;
        }
        var end = Math.min(size, off + chunk);
        var buf = new Uint8Array(await file.slice(off, end).arrayBuffer());
        scanner.feed(buf, off);
        off = end;
        var now = performance.now();
        if (now - lastPost > 80 || off === size) {
          lastPost = now;
          var p = scanner.progress();
          self.postMessage({
            type: "progress",
            id: m.id,
            bytes: off,
            total: size,
            elapsedMs: now - t0,
            line: p.line,
            elements: p.elements,
            errors: p.errors,
          });
        }
      }
      scanner.finish(size);
      var r = scanner.result();
      var elapsed = performance.now() - t0;
      self.postMessage(
        { type: "done", id: m.id, result: r, elapsedMs: elapsed, bytes: size },
        [
          r.checkpoints.buffer,
          r.elemStart.buffer,
          r.elemEnd.buffer,
          r.elemLine.buffer,
          r.elemParent.buffer,
          r.elemName.buffer,
          r.elemDepth.buffer,
        ]
      );
    } catch (e: any) {
      self.postMessage({ type: "error", id: m.id, message: String(e && e.message ? e.message : e) });
    }
    busy = false;
  }

  async function runSearch(m: any) {
    busy = true;
    cancelled = false;
    var file: Blob = m.file;
    var enc = new TextEncoder();
    var dec = new TextDecoder();
    var q: Uint8Array = enc.encode(m.caseSensitive ? m.query : m.query.toLowerCase());
    var qlen = q.length;
    var size = file.size;
    var chunk: number = m.chunkSize;
    var hits: any[] = [];
    var totalHits = 0;
    var carry = new Uint8Array(0);
    var carryBase = 0;
    var lines = 0; // newlines seen so far (0-based line index of current pos)
    var lineStart = 0;
    var off = 0;
    var t0 = performance.now();
    var lastPost = 0;
    var ci = !m.caseSensitive;
    try {
      while (off < size && qlen > 0) {
        if (cancelled) {
          self.postMessage({ type: "cancelled", id: m.id });
          busy = false;
          return;
        }
        var end = Math.min(size, off + chunk);
        var fresh = new Uint8Array(await file.slice(off, end).arrayBuffer());
        var buf: Uint8Array;
        var base: number;
        if (carry.length) {
          buf = new Uint8Array(carry.length + fresh.length);
          buf.set(carry);
          buf.set(fresh, carry.length);
          base = carryBase;
        } else {
          buf = fresh;
          base = off;
        }
        var carryLen = carry.length;
        var first = q[0];
        var n = buf.length;
        var scanFrom = carryLen; // newline counting resumes here
        var pos = 0;
        while (pos <= n - qlen) {
          var idx = -1;
          for (var j = pos; j < n; j++) {
            var bj = buf[j];
            if (ci && bj >= 65 && bj <= 90) bj += 32;
            if (bj === first) {
              idx = j;
              break;
            }
          }
          if (idx < 0 || idx > n - qlen) break;
          var ok = true;
          for (var k = 1; k < qlen; k++) {
            var bk = buf[idx + k];
            if (ci && bk >= 65 && bk <= 90) bk += 32;
            if (bk !== q[k]) {
              ok = false;
              break;
            }
          }
          if (ok && (idx >= carryLen || idx + qlen > carryLen)) {
            // count newlines from scanFrom to idx
            for (var c = scanFrom; c < idx; c++) if (buf[c] === 10) { lines++; lineStart = base + c + 1; }
            if (idx > scanFrom) scanFrom = idx;
            totalHits++;
            if (hits.length < m.maxHits) {
              var ps = Math.max(0, idx - 40);
              var pe = Math.min(n, idx + qlen + 60);
              hits.push({
                offset: base + idx,
                line: lines + 1,
                col: base + idx - lineStart + 1,
                preview: dec.decode(buf.subarray(ps, pe)).replace(/[\r\n\t]+/g, " "),
              });
            }
          }
          pos = idx + 1;
        }
        for (var c2 = scanFrom; c2 < n; c2++) if (buf[c2] === 10) { lines++; lineStart = base + c2 + 1; }
        // carry the last qlen-1 bytes
        var keep = Math.min(qlen - 1, n);
        carry = buf.slice(n - keep);
        carryBase = base + n - keep;
        // newline counting above already covered carry bytes; do not recount:
        scanFrom = 0;
        off = end;
        var now = performance.now();
        if (now - lastPost > 100) {
          lastPost = now;
          self.postMessage({ type: "progress", id: m.id, bytes: off, total: size, elapsedMs: now - t0, hits: totalHits });
        }
      }
      self.postMessage({ type: "searchDone", id: m.id, hits: hits, totalHits: totalHits, elapsedMs: performance.now() - t0, bytes: size });
    } catch (e: any) {
      self.postMessage({ type: "error", id: m.id, message: String(e && e.message ? e.message : e) });
    }
    busy = false;
  }
}

// --------------------------------------------------------------------------
// Main-thread client
// --------------------------------------------------------------------------
export type ScanProgress = {
  bytes: number;
  total: number;
  elapsedMs: number;
  line: number;
  elements: number;
  errors: number;
};
export type SearchHit = { offset: number; line: number; col: number; preview: string };
export type SearchProgress = { bytes: number; total: number; elapsedMs: number; hits: number };

export const CHUNK_SIZE = 8 * 1024 * 1024; // 8 MiB page-aligned chunks (2048 × 4 KiB pages)

let workerUrl: string | null = null;
function getWorkerUrl(): string {
  if (!workerUrl) {
    const src = `"use strict";\nconst createScanner = ${createScanner.toString()};\nconst main = ${workerMain.toString()};\nmain(self, createScanner);`;
    workerUrl = URL.createObjectURL(new Blob([src], { type: "text/javascript" }));
  }
  return workerUrl;
}

export class EngineWorker {
  private w: Worker;
  private seq = 0;
  private handlers = new Map<number, (m: any) => void>();
  constructor() {
    this.w = new Worker(getWorkerUrl());
    this.w.onmessage = (ev) => {
      const h = this.handlers.get(ev.data.id);
      if (h) h(ev.data);
    };
  }
  cancel() {
    this.w.postMessage({ type: "cancel" });
  }
  terminate() {
    this.w.terminate();
  }
  scan(
    file: Blob,
    onProgress: (p: ScanProgress) => void,
    opts: Partial<ScannerConfig> & { chunkSize?: number } = {}
  ): Promise<{ result: ScanIndex; elapsedMs: number; bytes: number } | null> {
    const id = ++this.seq;
    return new Promise((resolve, reject) => {
      this.handlers.set(id, (m) => {
        if (m.type === "progress") onProgress(m);
        else if (m.type === "done") {
          this.handlers.delete(id);
          resolve({ result: m.result, elapsedMs: m.elapsedMs, bytes: m.bytes });
        } else if (m.type === "cancelled") {
          this.handlers.delete(id);
          resolve(null);
        } else if (m.type === "error") {
          this.handlers.delete(id);
          reject(new Error(m.message));
        }
      });
      this.w.postMessage({
        type: "scan",
        id,
        file,
        chunkSize: opts.chunkSize ?? CHUNK_SIZE,
        maxIndexed: opts.maxIndexed ?? 3_000_000,
        stride: opts.stride ?? 32,
        maxErrors: opts.maxErrors ?? 2000,
      });
    });
  }
  search(
    file: Blob,
    query: string,
    caseSensitive: boolean,
    onProgress: (p: SearchProgress) => void,
    maxHits = 5000
  ): Promise<{ hits: SearchHit[]; totalHits: number; elapsedMs: number; bytes: number } | null> {
    const id = ++this.seq;
    return new Promise((resolve, reject) => {
      this.handlers.set(id, (m) => {
        if (m.type === "progress") onProgress(m);
        else if (m.type === "searchDone") {
          this.handlers.delete(id);
          resolve({ hits: m.hits, totalHits: m.totalHits, elapsedMs: m.elapsedMs, bytes: m.bytes });
        } else if (m.type === "cancelled") {
          this.handlers.delete(id);
          resolve(null);
        } else if (m.type === "error") {
          this.handlers.delete(id);
          reject(new Error(m.message));
        }
      });
      this.w.postMessage({ type: "search", id, file, query, caseSensitive, chunkSize: CHUNK_SIZE, maxHits });
    });
  }
}
