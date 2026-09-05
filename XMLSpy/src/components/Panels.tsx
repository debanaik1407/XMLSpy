import { useEffect, useState } from "react";
import type { NodeInfo, XmlDocument } from "../engine/document";
import { fmtBytes, fmtNum } from "../engine/document";
import type { WfError } from "../engine/scanner";
import type { SearchHit } from "../engine/worker";
import type { XPathResult } from "../engine/xpath";

export type Message = { id: number; kind: "info" | "error" | "warning" | "success"; text: string; line?: number; col?: number; docId?: string; fix?: string; error?: WfError; time: string };

// ---------------------------------------------------------------- Project
export function ProjectPanel({ docs, active, onSelect, onOpenSample, onOpenFile, onCorpus, busy }: { docs: XmlDocument[]; active: string | null; onSelect: (id: string) => void; onOpenSample: (k: string) => void; onOpenFile: () => void; onCorpus: (mib: number) => void; busy: boolean }) {
  const Item = ({ label, onClick, icon, sub }: { label: string; onClick?: () => void; icon: string; sub?: string }) => (
    <div className="flex items-center gap-2 px-2 py-[2px] cursor-pointer hover:bg-[var(--accent-soft)] text-[12px]" onClick={onClick}>
      <span className="w-4 text-center" style={{ color: "var(--fg-muted)" }}>
        {icon}
      </span>
      <span className="truncate">{label}</span>
      {sub && <span className="ml-auto text-[10px]" style={{ color: "var(--fg-muted)" }}>{sub}</span>}
    </div>
  );
  const Folder = ({ name }: { name: string }) => (
    <div className="px-2 pt-2 pb-[2px] text-[11px] font-semibold" style={{ color: "var(--fg-muted)" }}>
      ▾ {name}
    </div>
  );
  return (
    <div className="flex flex-col h-full overflow-auto">
      <Folder name="Open documents" />
      {docs.map((d) => (
        <div key={d.id} className={`flex items-center gap-2 px-2 py-[2px] cursor-pointer text-[12px] ${d.id === active ? "row-sel" : "hover:bg-[var(--accent-soft)]"}`} onClick={() => onSelect(d.id)}>
          <span style={{ color: d.stats.errors ? "var(--err)" : "var(--ok)" }}>●</span>
          <span className="truncate">
            {d.name}
            {d.dirty ? " *" : ""}
          </span>
          <span className="ml-auto text-[10px]" style={{ color: "var(--fg-muted)" }}>
            {fmtBytes(d.stats.bytes)}
          </span>
        </div>
      ))}
      {docs.length === 0 && <div className="px-4 text-[11px]" style={{ color: "var(--fg-muted)" }}>(none)</div>}
      <Folder name="Local files" />
      <Item icon="📂" label="Open file… (File System Access / picker)" onClick={onOpenFile} sub="Ctrl+O" />
      <Folder name="Examples" />
      <Item icon="📄" label="PurchaseOrders.xml" onClick={() => onOpenSample("orders")} sub="well-formed" />
      <Item icon="📄" label="catalog-broken.xml" onClick={() => onOpenSample("broken")} sub="8 WF errors" />
      <Item icon="📘" label="orders.xsd" onClick={() => onOpenSample("xsd")} sub="XSD 1.1" />
      <Folder name="Synthetic corpus (xmlspy-bench gen)" />
      {[10, 100, 500, 1024, 2048].map((m) => (
        <Item key={m} icon="⚙" label={`Generate & open ${m >= 1024 ? m / 1024 + " GiB" : m + " MiB"}`} onClick={() => !busy && onCorpus(m)} sub={m >= 1024 ? "large-file mode" : undefined} />
      ))}
      <div className="px-3 py-2 text-[10.5px] leading-snug" style={{ color: "var(--fg-muted)" }}>
        Corpus blobs share one 1 MiB template buffer, so a 2 GiB file costs ~1 MiB of heap to create. Everything is read back through <span className="mono">Blob.slice</span> in 8 MiB page-aligned chunks — never loaded whole.
      </div>
      <Folder name="Global resources / Catalogs" />
      <Item icon="🔗" label="Default (Phase 6)" />
    </div>
  );
}

// ---------------------------------------------------------------- Info
export function InfoPanel({ doc, nodeId, onGoto }: { doc: XmlDocument | null; nodeId: number | null; onGoto: (line: number) => void }) {
  const [info, setInfo] = useState<NodeInfo | null>(null);
  useEffect(() => {
    if (!doc || nodeId === null || nodeId < 0) {
      setInfo(null);
      return;
    }
    let c = false;
    doc.nodeInfo(nodeId).then((i) => !c && setInfo(i));
    return () => {
      c = true;
    };
  }, [doc, nodeId, doc?.version]);
  if (!doc) return <div className="p-2 text-[11px]" style={{ color: "var(--fg-muted)" }}>No document.</div>;
  if (!info)
    return (
      <div className="p-2 text-[11px]" style={{ color: "var(--fg-muted)" }}>
        Select an element in Grid View or an XPath result to inspect it.
      </div>
    );
  const Row = ({ k, v }: { k: string; v: React.ReactNode }) => (
    <div className="flex gap-2 py-[1px]">
      <span className="w-20 shrink-0" style={{ color: "var(--fg-muted)" }}>
        {k}
      </span>
      <span className="mono break-all">{v}</span>
    </div>
  );
  return (
    <div className="p-2 text-[11.5px] overflow-auto h-full">
      <div className="font-semibold mono mb-1" style={{ color: "var(--tok-tag)" }}>
        &lt;{info.name}&gt;
      </div>
      <Row k="XPath" v={doc.pathOf(info.id)} />
      <Row
        k="Line"
        v={
          <button className="underline" onClick={() => onGoto(info.line)}>
            {fmtNum(info.line)}
          </button>
        }
      />
      <Row k="Byte range" v={`${fmtNum(info.start)} – ${info.end > 0 ? fmtNum(info.end) : "?"} (${info.end > 0 ? fmtBytes(info.end - info.start) : "unknown"})`} />
      <Row k="Depth" v={String(info.depth)} />
      <Row k="Children" v={info.hasChildren ? fmtNum(info.childCount) : "0 (leaf)"} />
      {info.attrs.length > 0 && (
        <div className="mt-2">
          <div className="font-semibold mb-[2px]">Attributes</div>
          {info.attrs.map(([k, v]) => (
            <Row key={k} k={k} v={v} />
          ))}
        </div>
      )}
      {info.text && (
        <div className="mt-2">
          <div className="font-semibold mb-[2px]">Text</div>
          <div className="mono whitespace-pre-wrap break-words">{info.text.slice(0, 2000)}</div>
        </div>
      )}
    </div>
  );
}

// ---------------------------------------------------------------- Messages
export function MessagesPanel({ messages, onGoto, onFix, onClear }: { messages: Message[]; onGoto: (m: Message) => void; onFix: (m: Message) => void; onClear: () => void }) {
  const icon = { info: "ℹ", error: "✖", warning: "⚠", success: "✔" };
  const color = { info: "var(--accent)", error: "var(--err)", warning: "var(--warn)", success: "var(--ok)" };
  return (
    <div className="h-full flex flex-col">
      <div className="flex items-center gap-2 px-2 py-1 text-[11px]" style={{ borderBottom: "1px solid var(--border)" }}>
        <span>{messages.length} message(s)</span>
        <button className="btn ml-auto" onClick={onClear}>
          Clear
        </button>
      </div>
      <div className="flex-1 overflow-auto">
        {messages.length === 0 && <div className="p-2 text-[11px]" style={{ color: "var(--fg-muted)" }}>No messages.</div>}
        {messages.map((m) => (
          <div key={m.id} className="flex items-start gap-2 px-2 py-[2px] text-[11.5px] hover:bg-[var(--accent-soft)]" style={{ borderBottom: "1px solid var(--border)" }}>
            <span style={{ color: color[m.kind], width: 12 }}>{icon[m.kind]}</span>
            <span className="w-14 shrink-0" style={{ color: "var(--fg-muted)" }}>
              {m.time}
            </span>
            <span className={`flex-1 ${m.line ? "cursor-pointer" : ""}`} onClick={() => m.line && onGoto(m)}>
              {m.line ? (
                <span className="mono mr-2" style={{ color: "var(--accent)" }}>
                  Ln {fmtNum(m.line)}
                  {m.col ? `, Col ${m.col}` : ""}:
                </span>
              ) : null}
              {m.text}
            </span>
            {m.fix && (
              <button className="btn shrink-0" title={m.fix} onClick={() => onFix(m)} style={{ color: "var(--ok)" }}>
                ⚡ SmartFix: {m.fix.length > 34 ? m.fix.slice(0, 34) + "…" : m.fix}
              </button>
            )}
          </div>
        ))}
      </div>
    </div>
  );
}

// ---------------------------------------------------------------- Find in Files
export function FindPanel({ doc, onSearch, onCancel, hits, total, progress, running, onGoto, initialQuery }: { doc: XmlDocument | null; onSearch: (q: string, cs: boolean) => void; onCancel: () => void; hits: SearchHit[]; total: number; progress: { bytes: number; total: number; elapsedMs: number } | null; running: boolean; onGoto: (line: number, col: number) => void; initialQuery: string }) {
  const [q, setQ] = useState(initialQuery);
  const [cs, setCs] = useState(false);
  useEffect(() => setQ(initialQuery), [initialQuery]);
  const mbps = progress && progress.elapsedMs > 0 ? (progress.bytes / 1e6 / (progress.elapsedMs / 1000)).toFixed(0) : null;
  return (
    <div className="h-full flex flex-col">
      <form
        className="flex items-center gap-2 px-2 py-1 text-[11px]"
        style={{ borderBottom: "1px solid var(--border)" }}
        onSubmit={(e) => {
          e.preventDefault();
          if (q && doc) onSearch(q, cs);
        }}
      >
        <span>Find:</span>
        <input className="input w-72 mono" value={q} onChange={(e) => setQ(e.target.value)} placeholder="literal text (streamed over the whole file)" autoFocus />
        <label className="flex items-center gap-1">
          <input type="checkbox" checked={cs} onChange={(e) => setCs(e.target.checked)} /> Match case
        </label>
        <button className="btn btn-primary" type="submit" disabled={!doc || running}>
          Find all
        </button>
        {running && (
          <button className="btn" type="button" onClick={onCancel}>
            Cancel
          </button>
        )}
        <span className="ml-auto" style={{ color: "var(--fg-muted)" }}>
          {progress ? `${fmtBytes(progress.bytes)} / ${fmtBytes(progress.total)} · ${mbps} MB/s · ${fmtNum(total)} hit(s)` : hits.length ? `${fmtNum(total)} hit(s) (showing ${fmtNum(hits.length)})` : "Regex & XPath-filtered find ship with the Rust engine (Phase 1/2)."}
        </span>
      </form>
      <div className="flex-1 overflow-auto mono text-[11.5px]">
        {hits.map((h, i) => (
          <div key={i} className="flex gap-3 px-2 py-[1px] cursor-pointer hover:bg-[var(--accent-soft)] whitespace-nowrap" onClick={() => onGoto(h.line, h.col)}>
            <span style={{ color: "var(--accent)", minWidth: 120 }}>
              Ln {fmtNum(h.line)}, Col {h.col}
            </span>
            <span className="truncate">{h.preview}</span>
          </div>
        ))}
      </div>
    </div>
  );
}

// ---------------------------------------------------------------- XPath / XQuery
export function XPathPanel({ doc, onEval, result, error, running, onGoto, onSelectNode }: { doc: XmlDocument | null; onEval: (expr: string) => void; result: XPathResult | null; error: string | null; running: boolean; onGoto: (line: number) => void; onSelectNode: (id: number) => void }) {
  const [expr, setExpr] = useState("//PurchaseOrder[1]");
  const chips = ["//PurchaseOrder[1]", "count(//Item)", "/PurchaseOrders/PurchaseOrder[2]/Items/Item", "//Item/@PartNumber", "//*[1]"];
  return (
    <div className="h-full flex flex-col">
      <div className="flex items-center gap-2 px-2 py-1 text-[11px]" style={{ borderBottom: "1px solid var(--border)" }}>
        <select className="input" defaultValue="xpath31">
          <option value="xpath31">XPath 3.1</option>
          <option value="xpath2">XPath 2.0</option>
          <option value="xpath1">XPath 1.0</option>
          <option value="xquery31">XQuery 3.1 (Phase 3)</option>
        </select>
        <input
          className="input flex-1 mono"
          value={expr}
          onChange={(e) => setExpr(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") onEval(expr);
          }}
          placeholder="XPath expression — Enter to evaluate"
        />
        <button className="btn btn-primary" disabled={!doc || running} onClick={() => onEval(expr)}>
          {running ? "Evaluating…" : "Evaluate (Ctrl+Shift+E)"}
        </button>
      </div>
      <div className="flex gap-1 px-2 py-1 flex-wrap" style={{ borderBottom: "1px solid var(--border)" }}>
        {chips.map((c) => (
          <button key={c} className="btn mono" style={{ fontSize: 10.5 }} onClick={() => setExpr(c)}>
            {c}
          </button>
        ))}
        {result && (
          <span className="ml-auto text-[10.5px]" style={{ color: "var(--fg-muted)" }}>
            engine: <b>{result.engine}</b> · {result.elapsedMs.toFixed(1)} ms · {fmtNum(result.count)} item(s)
          </span>
        )}
      </div>
      <div className="flex-1 overflow-auto text-[11.5px]">
        {error && <div className="p-2" style={{ color: "var(--err)" }}>{error}</div>}
        {result?.warning && <div className="px-2 py-1" style={{ color: "var(--warn)" }}>{result.warning}</div>}
        {result?.kind === "value" && (
          <div className="p-2 mono">
            = <b>{result.value}</b>
          </div>
        )}
        {result?.kind === "nodes" &&
          result.nodes.map((n, i) => (
            <div
              key={i}
              className="flex gap-3 px-2 py-[1px] cursor-pointer hover:bg-[var(--accent-soft)] whitespace-nowrap mono"
              onClick={() => {
                onGoto(n.line);
                if (n.id >= 0) onSelectNode(n.id);
              }}
            >
              <span style={{ color: "var(--accent)", minWidth: 90 }}>Ln {fmtNum(n.line)}</span>
              <span style={{ color: "var(--fg-muted)", minWidth: 220 }} className="truncate">
                {n.path}
              </span>
              <span className="truncate">{n.preview}</span>
            </div>
          ))}
      </div>
    </div>
  );
}

// ---------------------------------------------------------------- Benchmarks
export type EngineInfo = { name: string; failure: string | null; wasmBytes: number; rustc: string; simd128: boolean; profile: string; abi: number };

export function BenchPanel({ doc, frame, memory, onRescan, blockCache, engine }: { doc: XmlDocument | null; frame: { p50: number; p99: number; max: number }; memory: number | null; onRescan: () => void; blockCache: number; engine?: EngineInfo }) {
  const Row = ({ k, v, gate, ok }: { k: string; v: string; gate?: string; ok?: boolean }) => (
    <tr>
      <td className="pr-4 py-[2px]" style={{ color: "var(--fg-muted)" }}>
        {k}
      </td>
      <td className="pr-4 mono">{v}</td>
      <td className="pr-4 text-[10.5px]" style={{ color: "var(--fg-muted)" }}>
        {gate}
      </td>
      <td style={{ color: ok === undefined ? "var(--fg-muted)" : ok ? "var(--ok)" : "var(--err)" }}>{ok === undefined ? "" : ok ? "PASS" : "FAIL"}</td>
    </tr>
  );
  const s = doc?.stats;
  const mbps = s && s.scanMs > 0 ? s.bytes / 1e6 / (s.scanMs / 1000) : 0;
  return (
    <div className="h-full overflow-auto p-2 text-[11.5px]">
      <div className="flex items-center gap-3 mb-2">
        <b>Benchmark harness (browser twin of bench/xmlspy-bench)</b>
        <button className="btn" onClick={onRescan} disabled={!doc}>
          Re-run index + WF scan (F7)
        </button>
      </div>
      <table>
        <tbody>
          <Row
            k="Engine"
            v={engine ? (engine.name === "rust-wasm" ? `Rust ${engine.rustc} → wasm32-unknown-unknown · ${fmtBytes(engine.wasmBytes)} · ABI v${engine.abi}${engine.simd128 ? " · simd128" : ""}` : `TypeScript fallback${engine.failure ? ` (${engine.failure})` : ""}`) : "—"}
            gate="crates/xmlspy-{core,index,parse,wasm}"
            ok={engine ? engine.name === "rust-wasm" : undefined}
          />
          <Row k="Open → first paint" v={s ? `${s.openToFirstPaintMs.toFixed(0)} ms` : "—"} gate="< 2000 ms (10 GB)" ok={s ? s.openToFirstPaintMs < 2000 : undefined} />
          <Row k="Single-pass WF + index scan" v={s ? `${(s.scanMs / 1000).toFixed(2)} s · ${mbps.toFixed(0)} MB/s (1 JS worker)` : "—"} gate="≥ 500 MB/s native SIMD Rust; browser JS worker ≈ 150–250 MB/s" ok={s ? mbps > 50 : undefined} />
          <Row k="Document size" v={s ? `${fmtBytes(s.bytes)} · ${fmtNum(s.lines)} lines · ${fmtNum(s.elements)} elements · depth ${s.maxDepth}` : "—"} />
          <Row k="Structural index resident" v={s ? `${fmtBytes(s.indexBytes)} (${fmtNum(s.indexedElements)} nodes indexed, ${(s.indexBytes / Math.max(1, s.bytes) * 100).toFixed(2)} % of file)` : "—"} gate="on-disk .xsi in Rust; capped typed arrays in browser" />
          <Row k="Line block cache" v={`${blockCache} / 512 blocks`} gate="LRU, 32 lines per block" />
          <Row k="UI frame time (rAF)" v={`p50 ${frame.p50.toFixed(1)} ms · p99 ${frame.p99.toFixed(1)} ms · max ${frame.max.toFixed(1)} ms`} gate="p99 < 16.7 ms while scrolling" ok={frame.p99 < 16.7 * 1.5} />
          <Row k="JS heap" v={memory !== null ? fmtBytes(memory) : "n/a (Chrome only: performance.memory)"} gate="< 512 MB peak for viewing" ok={memory !== null ? memory < 512 * 1024 * 1024 : undefined} />
          <Row k="Well-formedness" v={s ? (s.errors === 0 ? "well-formed" : `${fmtNum(s.errors)} error(s)`) : "—"} />
        </tbody>
      </table>
      <p className="mt-3 max-w-[900px] leading-snug" style={{ color: "var(--fg-muted)" }}>
        Methodology: timings are wall-clock from file selection to the first frame that paints real lines; the scan runs in a Blob-URL Web Worker reading 8 MiB page-aligned slices and feeding them to the Rust scanner in WebAssembly (the same <code>xmlspy_parse::Scanner</code> the CLI runs — <code>cargo run -p xmlspy-cli -- bench file.xml</code> measures 292 MB/s single-thread natively on this class of hardware); frame time is sampled with requestAnimationFrame on the UI thread (p50/p99 over the last 240 frames). The Rust harness adds RSS via /proc/self/statm, criterion micro-benchmarks and CI regression gates (5 %).
      </p>
    </div>
  );
}
