import { createScanner, type ScanIndex, type ScannerConfig } from "./scanner";
import { WASM_BASE64, WASM_BUILD } from "./wasmBinary";
import { bootWasmEngine, type WasmEngine, type WasmScanner } from "./wasmEngine";

/**
 * Engine selection.
 *
 * The Rust engine (`rust/crates/*` → `wasm32-unknown-unknown`) is the default: it is the
 * same `Scanner` state machine the CLI and the (future) server run, compiled to WASM and
 * inlined in the bundle. The TypeScript scanner in `engine/scanner.ts` stays as a
 * reference implementation and as the fallback when WebAssembly is unavailable or the
 * module fails to instantiate.
 */

export type EngineName = "rust-wasm" | "typescript";

let booting: Promise<WasmEngine | null> | null = null;
let current: EngineName = "typescript";
let failure: string | null = null;

/** Instantiate (once) the Rust engine; resolves to `null` if it is not usable here. */
export function loadEngine(): Promise<WasmEngine | null> {
  if (!booting) {
    booting =
      typeof WebAssembly === "undefined"
        ? Promise.resolve(null)
        : bootWasmEngine(WASM_BASE64)
            .then((e) => {
              current = "rust-wasm";
              return e;
            })
            .catch((err: unknown) => {
              failure = err instanceof Error ? err.message : String(err);
              current = "typescript";
              return null;
            });
  }
  return booting;
}

/** Which engine the last `createScannerAuto` used. */
export function engineName(): EngineName {
  return current;
}

/** Build metadata for the status bar / Benchmarks panel. */
export function engineInfo() {
  return {
    name: current,
    failure,
    wasmBytes: WASM_BUILD.bytes,
    rustc: WASM_BUILD.rustc,
    simd128: WASM_BUILD.simd128,
    profile: WASM_BUILD.profile,
    abi: WASM_BUILD.abi,
  };
}

/** Adapt the TypeScript reference scanner to the richer WASM scanner surface. */
function wrapTsScanner(cfg: ScannerConfig): WasmScanner {
  const s = createScanner(cfg);
  return {
    feed: (buf, base) => s.feed(buf, base),
    finish: (total) => s.finish(total),
    result: () => s.result(),
    snapshot: () => s.result(),
    progress: () => s.progress(),
    free: () => {},
  };
}

/** A scanner from the Rust engine when possible, otherwise the TypeScript one. */
export async function createScannerAuto(cfg: ScannerConfig): Promise<{ scanner: WasmScanner; engine: EngineName }> {
  const wasm = await loadEngine();
  if (wasm) return { scanner: wasm.createScanner(cfg), engine: "rust-wasm" };
  return { scanner: wrapTsScanner(cfg), engine: "typescript" };
}

export type { ScanIndex };
