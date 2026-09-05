import type { WfError } from "./scanner";

/**
 * Loader and JS-side ABI for the **Rust engine** (`rust/crates/xmlspy-wasm`).
 *
 * `bootWasmEngine` is deliberately written as ONE self-contained function: the Web
 * Worker is created from a Blob URL whose source is produced with `Function.toString()`,
 * so nothing in here may reference module-scope imports. It is used unchanged on the main
 * thread (progressive first paint) and inside the worker (full scan + search).
 */

/** Index shape shared with the TypeScript reference scanner (`engine/scanner.ts`). */
export type ScanIndexLike = {
  checkpoints: Float64Array<ArrayBuffer>;
  stride: number;
  lineCount: number;
  elemStart: Float64Array<ArrayBuffer>;
  elemEnd: Float64Array<ArrayBuffer>;
  elemLine: Float64Array<ArrayBuffer>;
  elemParent: Int32Array<ArrayBuffer>;
  elemName: Int32Array<ArrayBuffer>;
  elemDepth: Uint16Array<ArrayBuffer>;
  names: string[];
  indexedElements: number;
  totalElements: number;
  totalAttributes: number;
  maxDepth: number;
  errors: WfError[];
  errorCount: number;
};

/** Same surface as the TypeScript scanner, backed by Rust. */
export type WasmScanner = {
  feed(buf: Uint8Array, base: number): void;
  finish(total: number): void;
  /** Serialised + decoded index after `finish()`. */
  result(): ScanIndexLike;
  /** Index of everything scanned so far (progressive open / cancelled scan). */
  snapshot(): ScanIndexLike;
  progress(): { line: number; elements: number; errors: number };
  free(): void;
};

/** Streaming literal finder backed by `xmlspy_parse::Finder`. */
export type WasmFinder = {
  feed(buf: Uint8Array, base: number): void;
  finish(): void;
  total(): number;
  hits(): { offset: number; line: number; col: number }[];
  clearHits(): void;
  free(): void;
};

export type WasmEngine = {
  name: "rust-wasm";
  abi: number;
  /** Bytes of the wasm module. */
  bytes: number;
  createScanner(cfg: { maxIndexed: number; stride: number; maxErrors: number }): WasmScanner;
  createFinder(needle: Uint8Array, caseInsensitive: boolean, maxHits: number): WasmFinder;
};

/**
 * Decode + instantiate the engine. Throws if the module is missing, refuses to
 * instantiate, or reports an ABI this build does not understand — callers fall back to
 * the TypeScript scanner in that case.
 */
export async function bootWasmEngine(base64: string): Promise<WasmEngine> {
  // ---- base64 → bytes (no imports: this function is stringified into a worker) ----
  const bin = atob(base64);
  const bytes = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);

  const { instance } = await WebAssembly.instantiate(bytes.buffer as ArrayBuffer, {});
  const ex = instance.exports as any;
  const ABI = 1;
  if (typeof ex.xs_abi_version !== "function" || ex.xs_abi_version() !== ABI) {
    throw new Error("xmlspy-wasm: unsupported ABI");
  }

  const HEADER_LEN = 112;
  const XSI_MAGIC = 0x31495358; // "XSI1" little-endian
  const decoder = new TextDecoder("utf-8");

  const mem = () => ex.memory.buffer as ArrayBuffer;
  const u64 = (dv: DataView, at: number) => dv.getUint32(at + 4, true) * 4294967296 + dv.getUint32(at, true);

  /** Copy a section out of wasm memory so the typed array owns aligned, transferable storage. */
  const section = (ptr: number, off: number, byteLen: number) => new Uint8Array(mem(), ptr + off, byteLen).slice();

  function decodeXsi(ptr: number, len: number): ScanIndexLike {
    if (len < HEADER_LEN) throw new Error("xmlspy-wasm: truncated .xsi");
    const dv = new DataView(mem(), ptr, len);
    if (dv.getUint32(0, true) !== XSI_MAGIC) throw new Error("xmlspy-wasm: bad .xsi magic");
    const stride = dv.getUint32(8, true);
    const n = dv.getUint32(12, true);
    const lineCount = u64(dv, 24);
    const totalElements = u64(dv, 32);
    const totalAttributes = u64(dv, 40);
    const maxDepth = dv.getUint32(48, true);
    const cpCount = dv.getUint32(52, true);
    const nameCount = dv.getUint32(56, true);
    const errorCount = dv.getUint32(60, true);
    const offCp = dv.getUint32(64, true);
    const offStart = dv.getUint32(68, true);
    const offEnd = dv.getUint32(72, true);
    const offLine = dv.getUint32(76, true);
    const offParent = dv.getUint32(80, true);
    const offName = dv.getUint32(84, true);
    const offDepth = dv.getUint32(88, true);
    const offNames = dv.getUint32(92, true);
    const offErrors = dv.getUint32(96, true);

    const checkpoints = new Float64Array(cpCount);
    for (let k = 0; k < cpCount; k++) checkpoints[k] = u64(dv, offCp + k * 8);

    const elemStart = new Float64Array(n);
    const elemEnd = new Float64Array(n);
    const elemLine = new Float64Array(n);
    for (let k = 0; k < n; k++) {
      elemStart[k] = u64(dv, offStart + k * 8);
      elemLine[k] = u64(dv, offLine + k * 8);
      const at = offEnd + k * 8;
      const lo = dv.getUint32(at, true);
      const hi = dv.getUint32(at + 4, true);
      // Sentinels from the Rust index: u64::MAX = never closed, u64::MAX-1 = '>' pending.
      elemEnd[k] = hi === 0xffffffff && lo === 0xffffffff ? -1 : hi === 0xffffffff && lo === 0xfffffffe ? -2 : hi * 4294967296 + lo;
    }

    const elemParent = new Int32Array(section(ptr, offParent, n * 4).buffer as ArrayBuffer);
    const elemName = new Int32Array(section(ptr, offName, n * 4).buffer as ArrayBuffer);
    const elemDepth = new Uint16Array(section(ptr, offDepth, n * 2).buffer as ArrayBuffer);

    const names: string[] = [];
    let p = offNames;
    for (let k = 0; k < nameCount; k++) {
      const l = dv.getUint32(p, true);
      p += 4;
      names.push(decoder.decode(new Uint8Array(mem(), ptr + p, l)));
      p += l;
    }

    const errors: WfError[] = [];
    let e = offErrors;
    const errRecords = dv.getUint32(e, true);
    e += 4;
    for (let k = 0; k < errRecords; k++) {
      const offset = u64(dv, e);
      const line = u64(dv, e + 8);
      const col = dv.getUint32(e + 16, true);
      const severity = dv.getUint8(e + 20) === 1 ? "warning" : "error";
      const hasFix = dv.getUint8(e + 21) !== 0;
      e += 22;
      const msgLen = dv.getUint32(e, true);
      e += 4;
      const msg = decoder.decode(new Uint8Array(mem(), ptr + e, msgLen));
      e += msgLen;
      const fixLen = dv.getUint32(e, true);
      e += 4;
      const fix = fixLen ? decoder.decode(new Uint8Array(mem(), ptr + e, fixLen)) : "";
      e += fixLen;
      errors.push({ offset, line, col, msg, severity: severity as WfError["severity"], fix: hasFix ? fix : undefined });
    }

    return {
      checkpoints,
      stride,
      lineCount,
      elemStart,
      elemEnd,
      elemLine,
      elemParent,
      elemName,
      elemDepth,
      names,
      indexedElements: n,
      totalElements,
      totalAttributes,
      maxDepth,
      errors,
      errorCount,
    };
  }

  // A single scratch buffer inside wasm memory, reused for every chunk.
  let scratch = 0;
  let scratchLen = 0;
  function upload(buf: Uint8Array): number {
    if (buf.length > scratchLen) {
      if (scratch) ex.xs_free(scratch, scratchLen);
      scratchLen = buf.length;
      scratch = ex.xs_alloc(scratchLen);
      if (!scratch) throw new Error("xmlspy-wasm: out of memory");
    }
    new Uint8Array(mem(), scratch, buf.length).set(buf);
    return scratch;
  }

  return {
    name: "rust-wasm",
    abi: ABI,
    bytes: bytes.length,

    createScanner(cfg) {
      let h = ex.xs_scanner_new(cfg.maxIndexed >>> 0, cfg.stride >>> 0, cfg.maxErrors >>> 0);
      const read = () => decodeXsi(ex.xs_scanner_xsi_ptr(h), ex.xs_scanner_xsi_len(h));
      return {
        feed(buf, base) {
          if (!h || buf.length === 0) return;
          ex.xs_scanner_feed(h, upload(buf), buf.length, base);
        },
        finish(total) {
          if (h) ex.xs_scanner_finish(h, total);
        },
        result: read,
        snapshot() {
          ex.xs_scanner_snapshot(h);
          return read();
        },
        progress: () => ({
          line: ex.xs_scanner_line(h),
          elements: ex.xs_scanner_elements(h),
          errors: ex.xs_scanner_errors(h),
        }),
        free() {
          if (h) ex.xs_scanner_free(h);
          h = 0;
        },
      };
    },

    createFinder(needle, caseInsensitive, maxHits) {
      const np = ex.xs_alloc(Math.max(1, needle.length));
      new Uint8Array(mem(), np, needle.length).set(needle);
      let h = ex.xs_finder_new(np, needle.length, caseInsensitive ? 1 : 0, maxHits >>> 0);
      ex.xs_free(np, Math.max(1, needle.length));
      return {
        feed(buf, base) {
          if (!h || buf.length === 0) return;
          ex.xs_finder_feed(h, upload(buf), buf.length, base);
        },
        finish() {
          if (h) ex.xs_finder_finish(h);
        },
        total: () => (h ? ex.xs_finder_total(h) : 0),
        hits() {
          if (!h) return [];
          const count = ex.xs_finder_snapshot(h);
          if (!count) return [];
          const raw = new Float64Array(new Uint8Array(mem(), ex.xs_finder_hits_ptr(h), count * 24).slice().buffer as ArrayBuffer);
          const out = new Array(count);
          for (let k = 0; k < count; k++) {
            out[k] = { offset: raw[k * 3], line: raw[k * 3 + 1], col: raw[k * 3 + 2] };
          }
          return out;
        },
        clearHits() {
          if (h) ex.xs_finder_clear_hits(h);
        },
        free() {
          if (h) ex.xs_finder_free(h);
          h = 0;
        },
      };
    },
  };
}
