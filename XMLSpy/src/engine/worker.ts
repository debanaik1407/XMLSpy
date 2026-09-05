import { createScanner, type ScanIndex, type ScannerConfig } from "./scanner";
import { WASM_BASE64 } from "./wasmBinary";
import { bootWasmEngine } from "./wasmEngine";

/**
 * Worker body. SELF-CONTAINED (serialized via toString). Receives a Blob/File
 * and reads it in page-aligned chunks with `Blob.slice()` — the browser
 * analogue of memmap2 chunked reads — so the file is never fully in memory.
 *
 * Both the scan and the search run inside the **Rust engine** compiled to WebAssembly
 * (`bootWasm`); the TypeScript scanner (`makeScanner`) is the fallback when WASM is
 * unavailable. Every message reports which engine produced it.
 */
function workerMain(self: any, makeScanner: typeof createScanner, bootWasm: typeof bootWasmEngine, wasmB64: string) {
  var cancelled = false;
  var busy = false;
  var enginePromise: Promise<any> | null = null;

  function engine(): Promise<any> {
    if (!enginePromise) {
      enginePromise =
        typeof WebAssembly === "undefined"
          ? Promise.resolve(null)
          : bootWasm(wasmB64).catch(function () {
              return null;
            });
    }
    return enginePromise;
  }

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
    var cfg = { maxIndexed: m.maxIndexed, stride: m.stride, maxErrors: m.maxErrors };
    var wasm = await engine();
    var scanner = wasm ? wasm.createScanner(cfg) : makeScanner(cfg);
    var engineName = wasm ? "rust-wasm" : "typescript";
    var t0 = performance.now();
    var off = 0;
    var lastPost = 0;
    try {
      while (off < size) {
        if (cancelled) {
          self.postMessage({ type: "cancelled", id: m.id });
          if (wasm) scanner.free();
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
            engine: engineName,
          });
        }
      }
      scanner.finish(size);
      var r = scanner.result();
      if (wasm) scanner.free();
      var elapsed = performance.now() - t0;
      self.postMessage(
        { type: "done", id: m.id, result: r, elapsedMs: elapsed, bytes: size, engine: engineName },
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
      if (wasm && scanner) scanner.free();
      self.postMessage({ type: "error", id: m.id, message: String(e && e.message ? e.message : e) });
    }
    busy = false;
  }

  async function runSearch(m: any) {
    busy = true;
    cancelled = false;
    var wasm = await engine();
    if (wasm) return runSearchWasm(m, wasm);
    return runSearchJs(m);
  }

  /** Rust `xmlspy_parse::Finder`: carry-over handled inside WASM, previews built here. */
  async function runSearchWasm(m: any, wasm: any) {
    var file: Blob = m.file;
    var enc = new TextEncoder();
    var dec = new TextDecoder();
    var needle: Uint8Array = enc.encode(m.query);
    var size = file.size;
    var chunk: number = m.chunkSize;
    var finder = wasm.createFinder(needle, !m.caseSensitive, m.maxHits);
    var t0 = performance.now();
    var lastPost = 0;
    var off = 0;
    var emitted = 0;
    var hits: any[] = [];
    // Keep a tail of the previous chunk so previews of boundary-spanning hits work.
    var tailLen = Math.max(256, needle.length + 128);
    var tail = new Uint8Array(0);
    var tailBase = 0;
    try {
      while (off < size) {
        if (cancelled) {
          self.postMessage({ type: "cancelled", id: m.id });
          finder.free();
          busy = false;
          return;
        }
        var end = Math.min(size, off + chunk);
        var fresh = new Uint8Array(await file.slice(off, end).arrayBuffer());
        finder.feed(fresh, off);

        var all = finder.hits();
        if (all.length > emitted) {
          var view: Uint8Array;
          var viewBase: number;
          if (tail.length) {
            view = new Uint8Array(tail.length + fresh.length);
            view.set(tail);
            view.set(fresh, tail.length);
            viewBase = tailBase;
          } else {
            view = fresh;
            viewBase = off;
          }
          for (var k = emitted; k < all.length; k++) {
            var h = all[k];
            var rel = h.offset - viewBase;
            var ps = Math.max(0, rel - 40);
            var pe = Math.min(view.length, rel + needle.length + 60);
            hits.push({
              offset: h.offset,
              line: h.line,
              col: h.col,
              preview: rel >= 0 && rel < view.length ? dec.decode(view.subarray(ps, pe)).replace(/[\r\n\t]+/g, " ") : "",
            });
          }
          emitted = all.length;
        }

        var keep = Math.min(tailLen, fresh.length);
        tail = fresh.slice(fresh.length - keep);
        tailBase = end - keep;
        off = end;

        var now = performance.now();
        if (now - lastPost > 100) {
          lastPost = now;
          self.postMessage({ type: "progress", id: m.id, bytes: off, total: size, elapsedMs: now - t0, hits: finder.total(), engine: "rust-wasm" });
        }
      }
      finder.finish();
      var total = finder.total();
      finder.free();
      self.postMessage({ type: "searchDone", id: m.id, hits: hits, totalHits: total, elapsedMs: performance.now() - t0, bytes: size, engine: "rust-wasm" });
    } catch (e: any) {
      finder.free();
      self.postMessage({ type: "error", id: m.id, message: String(e && e.message ? e.message : e) });
    }
    busy = false;
  }

  /** Fallback: the original JavaScript byte scanner. */
  async function runSearchJs(m: any) {
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
          self.postMessage({ type: "progress", id: m.id, bytes: off, total: size, elapsedMs: now - t0, hits: totalHits, engine: "typescript" });
        }
      }
      self.postMessage({ type: "searchDone", id: m.id, hits: hits, totalHits: totalHits, elapsedMs: performance.now() - t0, bytes: size, engine: "typescript" });
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
  engine?: string;
};
export type SearchHit = { offset: number; line: number; col: number; preview: string };
export type SearchProgress = { bytes: number; total: number; elapsedMs: number; hits: number; engine?: string };

export const CHUNK_SIZE = 8 * 1024 * 1024; // 8 MiB page-aligned chunks (2048 × 4 KiB pages)

/**
 * The exact JavaScript the Blob-URL worker executes: the scanner, the WASM loader and
 * the worker body, serialised with `Function.toString()` plus the base64 module. Nothing
 * here may close over module scope. Exported so `scripts/parity.mjs` can run the real
 * (minified) worker source in a Node VM and diff it against the TypeScript engine.
 */
export function workerSource(): string {
  return [
    '"use strict";',
    `const WASM_B64 = ${JSON.stringify(WASM_BASE64)};`,
    `const createScanner = ${createScanner.toString()};`,
    `const bootWasmEngine = ${bootWasmEngine.toString()};`,
    `const main = ${workerMain.toString()};`,
    "main(self, createScanner, bootWasmEngine, WASM_B64);",
  ].join("\n");
}

let workerUrl: string | null = null;
function getWorkerUrl(): string {
  if (!workerUrl) workerUrl = URL.createObjectURL(new Blob([workerSource()], { type: "text/javascript" }));
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
  ): Promise<{ result: ScanIndex; elapsedMs: number; bytes: number; engine: string } | null> {
    const id = ++this.seq;
    return new Promise((resolve, reject) => {
      this.handlers.set(id, (m) => {
        if (m.type === "progress") onProgress(m);
        else if (m.type === "done") {
          this.handlers.delete(id);
          resolve({ result: m.result, elapsedMs: m.elapsedMs, bytes: m.bytes, engine: m.engine });
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
  ): Promise<{ hits: SearchHit[]; totalHits: number; elapsedMs: number; bytes: number; engine: string } | null> {
    const id = ++this.seq;
    return new Promise((resolve, reject) => {
      this.handlers.set(id, (m) => {
        if (m.type === "progress") onProgress(m);
        else if (m.type === "searchDone") {
          this.handlers.delete(id);
          resolve({ hits: m.hits, totalHits: m.totalHits, elapsedMs: m.elapsedMs, bytes: m.bytes, engine: m.engine });
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
