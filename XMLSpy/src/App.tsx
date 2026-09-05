import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { DocTabs, MenuBar, StatusBar, Toolbar, ViewTabs, type MenuItem, type ViewKind } from "./components/Chrome";
import { TextView, type GotoRequest } from "./components/TextView";
import { GridView } from "./components/GridView";
import { SchemaView } from "./components/SchemaView";
import { BrowserView } from "./components/BrowserView";
import { DocsView } from "./components/DocsView";
import { BenchPanel, FindPanel, InfoPanel, MessagesPanel, ProjectPanel, XPathPanel, type Message } from "./components/Panels";
import { XmlDocument, fmtBytes, fmtNum } from "./engine/document";
import { createScannerAuto, engineInfo, engineName, loadEngine } from "./engine/engine";
import { EngineWorker, type SearchHit } from "./engine/worker";
import { SAMPLE_BROKEN, SAMPLE_ORDERS, SAMPLE_XSD, generateCorpus } from "./engine/corpus";
import { evaluateXPath, type XPathResult } from "./engine/xpath";
import { prettyPrint } from "./engine/highlight";
import { inferSchema, schemaToXsd } from "./engine/schemaInfer";

type BottomTab = "messages" | "find" | "xpath" | "bench";
type Job = { label: string; pct: number; text: string; worker: EngineWorker };

const HEAD_BYTES = 2 * 1024 * 1024; // provisional index window for instant first paint

export default function App() {
  const [docs, setDocs] = useState<XmlDocument[]>([]);
  const [activeId, setActiveId] = useState<string | null>(null);
  const [view, setView] = useState<ViewKind>("text");
  const [messages, setMessages] = useState<Message[]>([]);
  const [job, setJob] = useState<Job | null>(null);
  const [cursor, setCursor] = useState({ line: 1, col: 1 });
  const [goto, setGoto] = useState<GotoRequest>(null);
  const [selectedNode, setSelectedNode] = useState<number | null>(null);
  const [dark, setDark] = useState(() => window.matchMedia?.("(prefers-color-scheme: dark)").matches ?? false);
  const [bottomTab, setBottomTab] = useState<BottomTab>("messages");
  const [bottomOpen, setBottomOpen] = useState(true);
  const [showProject, setShowProject] = useState(true);
  const [showInfo, setShowInfo] = useState(true);
  const [tick, setTick] = useState(0);
  const [frame, setFrame] = useState({ p50: 0, p99: 0, max: 0 });
  const [memory, setMemory] = useState<number | null>(null);
  // search
  const [findQuery, setFindQuery] = useState("");
  const [hits, setHits] = useState<SearchHit[]>([]);
  const [hitTotal, setHitTotal] = useState(0);
  const [searchProg, setSearchProg] = useState<{ bytes: number; total: number; elapsedMs: number } | null>(null);
  const [searchWorker, setSearchWorker] = useState<EngineWorker | null>(null);
  // xpath
  const [xpResult, setXpResult] = useState<XPathResult | null>(null);
  const [xpError, setXpError] = useState<string | null>(null);
  const [xpRunning, setXpRunning] = useState(false);

  const msgId = useRef(0);
  const fileInput = useRef<HTMLInputElement>(null);
  const docsRef = useRef(docs);
  docsRef.current = docs;

  const active = useMemo(() => docs.find((d) => d.id === activeId) ?? null, [docs, activeId, tick]);
  const bump = () => setTick((t) => t + 1);

  const log = useCallback((kind: Message["kind"], text: string, extra: Partial<Message> = {}) => {
    const time = new Date().toLocaleTimeString([], { hour12: false });
    setMessages((m) => [{ id: ++msgId.current, kind, text, time, ...extra }, ...m].slice(0, 3000));
  }, []);

  // ---------------------------------------------------------------- theme
  useEffect(() => {
    document.documentElement.classList.toggle("dark", dark);
  }, [dark]);

  // ---------------------------------------------------------------- perf monitor
  useEffect(() => {
    let raf = 0;
    let last = performance.now();
    const samples: number[] = [];
    let lastReport = last;
    const loop = (t: number) => {
      const d = t - last;
      last = t;
      samples.push(d);
      if (samples.length > 240) samples.shift();
      if (t - lastReport > 700) {
        lastReport = t;
        const s = [...samples].sort((a, b) => a - b);
        setFrame({ p50: s[Math.floor(s.length * 0.5)] ?? 0, p99: s[Math.floor(s.length * 0.99)] ?? 0, max: s[s.length - 1] ?? 0 });
      }
      raf = requestAnimationFrame(loop);
    };
    raf = requestAnimationFrame(loop);
    const mem = setInterval(() => {
      const m = (performance as any).memory;
      setMemory(m ? m.usedJSHeapSize : null);
    }, 2000);
    return () => {
      cancelAnimationFrame(raf);
      clearInterval(mem);
    };
  }, []);

  // ---------------------------------------------------------------- open / index
  const indexDoc = useCallback(
    async (doc: XmlDocument, reason: string) => {
      const worker = new EngineWorker();
      const total = doc.file.size;
      setJob({ label: reason, pct: 0, text: `${reason} ${doc.name}…`, worker });
      try {
        const res = await worker.scan(doc.file, (p) => {
          const mbps = p.elapsedMs > 0 ? (p.bytes / 1e6 / (p.elapsedMs / 1000)).toFixed(0) : "…";
          setJob({ label: reason, pct: p.bytes / Math.max(1, p.total), text: `${reason} ${doc.name}: ${fmtBytes(p.bytes)} / ${fmtBytes(p.total)} · ${mbps} MB/s · ${fmtNum(p.elements)} elements · ${fmtNum(p.line)} lines · ${p.errors} error(s) — Esc to cancel`, worker });
        });
        if (!res) {
          log("warning", `${reason} of ${doc.name} cancelled after indexing ${fmtBytes(doc.indexedEnd)} of ${fmtBytes(total)} — the partial index stays usable (resumable build in the Rust engine).`);
          return;
        }
        if (!docsRef.current.includes(doc)) return; // closed meanwhile
        doc.replaceIndex(res.result, res.elapsedMs);
        bump();
        const mbps = res.elapsedMs > 0 ? (res.bytes / 1e6 / (res.elapsedMs / 1000)).toFixed(0) : "∞";
        const errs = res.result.errors;
        for (const e of errs.slice(0, 200).reverse()) log("error", e.msg, { line: e.line, col: e.col, docId: doc.id, fix: e.fix, error: e });
        const eng = res.engine === "rust-wasm" ? "Rust/WASM engine" : "TypeScript fallback engine";
        if (res.result.errorCount === 0) log("success", `${doc.name} is well-formed. Single-pass scan (${eng}): ${fmtBytes(res.bytes)} in ${(res.elapsedMs / 1000).toFixed(2)} s (${mbps} MB/s), ${fmtNum(res.result.totalElements)} elements, ${fmtNum(res.result.lineCount)} lines, index ${fmtBytes(doc.stats.indexBytes)}.`);
        else log("error", `${doc.name} is NOT well-formed: ${fmtNum(res.result.errorCount)} error(s) (${errs.length} listed). Scan ${fmtBytes(res.bytes)} in ${(res.elapsedMs / 1000).toFixed(2)} s (${mbps} MB/s, ${eng}).`);
      } catch (e: any) {
        log("error", `Indexing failed: ${e.message || e}`);
      } finally {
        worker.terminate();
        setJob((j) => (j && j.worker === worker ? null : j));
      }
    },
    [log]
  );

  const openBlob = useCallback(
    async (name: string, blob: Blob, text?: string, replaceId?: string) => {
      const t0 = performance.now();
      const headLen = Math.min(blob.size, HEAD_BYTES);
      const head = new Uint8Array(await blob.slice(0, headLen).arrayBuffer());
      const { scanner: sc } = await createScannerAuto({ maxIndexed: 300_000, stride: 32, maxErrors: 0 });
      sc.feed(head, 0);
      const prov = sc.snapshot();
      sc.free();
      const doc = new XmlDocument(name, blob, prov, 0, 0, text, headLen);
      doc.errors = [];
      setDocs((ds) => {
        if (replaceId) {
          const i = ds.findIndex((d) => d.id === replaceId);
          if (i >= 0) {
            const copy = [...ds];
            copy[i] = doc;
            return copy;
          }
        }
        return [...ds, doc];
      });
      setActiveId(doc.id);
      setSelectedNode(null);
      setView((v) => (v === "docs" ? "text" : v));
      requestAnimationFrame(() => {
        doc.stats.openToFirstPaintMs = performance.now() - t0;
        bump();
      });
      log("info", `Opened ${name} (${fmtBytes(blob.size)}) — first screen from a ${fmtBytes(headLen)} provisional index in ${(performance.now() - t0).toFixed(0)} ms [${engineName()}]; ${blob.size >= XmlDocument.MEMORY_LIMIT ? "large-file mode: pages read on demand via Blob.slice, edits as overlay, streamed save" : "in-memory piece-table mode"}.`);
      await indexDoc(doc, "Indexing");
      return doc;
    },
    [indexDoc, log]
  );

  const openText = useCallback((name: string, text: string, replaceId?: string) => openBlob(name, new Blob([text], { type: "application/xml" }), text, replaceId), [openBlob]);

  const booted = useRef(false);
  useEffect(() => {
    if (booted.current) return;
    booted.current = true;
    loadEngine().then(() => {
      const i = engineInfo();
      if (i.name === "rust-wasm") log("success", `Engine: Rust ${i.rustc} → wasm32-unknown-unknown (${fmtBytes(i.wasmBytes)}, ABI v${i.abi}, ${i.profile}${i.simd128 ? ", simd128" : ""}). Scanner, structural index (.xsi) and streaming Finder all run in WebAssembly.`);
      else log("warning", `Engine: TypeScript fallback${i.failure ? ` (WASM unavailable: ${i.failure})` : ""}.`);
      bump();
    });
    openText("PurchaseOrders.xml", SAMPLE_ORDERS);
    log("info", "XMLSpy-rs ready. Try: Project ▸ catalog-broken.xml (SmartFix), Project ▸ Generate 1 GiB corpus (large-file mode), Ctrl+2 Grid, Ctrl+3 Schema, Ctrl+Shift+E XPath, Ctrl+5 Architecture.");
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const openFiles = async (files: FileList | null) => {
    if (!files) return;
    for (const f of Array.from(files)) {
      const text = f.size < XmlDocument.MEMORY_LIMIT ? await f.text() : undefined;
      await openBlob(f.name, f, text);
    }
  };

  const openSample = (k: string) => {
    if (k === "orders") openText("PurchaseOrders.xml", SAMPLE_ORDERS);
    else if (k === "broken") openText("catalog-broken.xml", SAMPLE_BROKEN);
    else if (k === "xsd") openText("orders.xsd", SAMPLE_XSD);
  };

  const openCorpus = (mib: number) => {
    const t0 = performance.now();
    const { blob, name, records } = generateCorpus(mib);
    log("info", `Generated ${name}: ${fmtBytes(blob.size)}, ~${fmtNum(records)} PurchaseOrder records in ${(performance.now() - t0).toFixed(0)} ms (shared 1 MiB template, ~1 MiB heap).`);
    openBlob(name, blob);
  };

  // ---------------------------------------------------------------- editing
  const rescan = useCallback(
    async (doc: XmlDocument, reason = "Re-indexing") => {
      if (doc.editCount || doc.inMemory) await doc.commitEdits();
      await indexDoc(doc, reason);
    },
    [indexDoc]
  );

  const onEditLine = async (line: number, text: string) => {
    if (!active) return;
    const [before] = await active.getLines(line, line + 1);
    if (before === text) return;
    active.setLine(line, text);
    bump();
    if (active.inMemory) {
      await rescan(active, "Re-validating");
    } else {
      log("info", `Line ${fmtNum(line + 1)} edited in the overlay (${active.editCount} pending line edit(s)). Save streams untouched byte ranges zero-copy; press F7 to re-validate (Rust engine: incremental re-index of the enclosing element).`);
    }
  };

  const applyFix = async (m: Message) => {
    const doc = docs.find((d) => d.id === m.docId);
    const e = m.error;
    if (!doc || !e) return;
    const ln = e.line - 1;
    const [text] = await doc.getLines(ln, ln + 1);
    if (text === undefined) return;
    const col = Math.max(0, e.col - 1);
    let out: string | null = null;
    const fix = e.fix ?? "";
    let mm: RegExpMatchArray | null;
    if ((mm = /^Change to <\/(.+)>$/.exec(fix))) out = text.slice(0, col) + text.slice(col).replace(/^<\/[^>\s]+/, `</${mm[1]}`);
    else if ((mm = /^Insert (<\/.+)$/.exec(fix))) out = text.slice(0, col) + mm[1] + text.slice(col);
    else if (/&amp;/.test(fix)) out = text.slice(0, col) + text.slice(col).replace(/^&/, "&amp;");
    else if (/double quotes/.test(fix)) out = text.slice(0, col).replace(/=\s*$/, "=") + text.slice(col).replace(/^([^\s>/]+)/, '"$1"');
    else if (/'- -'/.test(fix)) out = text.slice(0, col) + text.slice(col).replace(/^--/, "- -");
    else if (/]]&gt;/.test(fix)) out = text.slice(0, col) + text.slice(col).replace(/^]]>/, "]]&gt;");
    else if (/Remove the duplicate attribute/.test(fix)) out = text.slice(0, col).replace(/\s+$/, "") + text.slice(col).replace(/^[^\s=]+\s*=\s*("[^"]*"|'[^']*')/, "");
    else if (/Delete this end tag/.test(fix)) out = text.slice(0, col) + text.slice(col).replace(/^<\/[^>]*>/, "");
    else if (/Escape the '<' as &lt;/.test(fix)) out = text.slice(0, col) + text.slice(col).replace(/^</, "&lt;");
    else if (/^Append (.+)$/.test(fix) && doc.inMemory) {
      const closers = /^Append (.+)$/.exec(fix)![1];
      const last = doc.lineCount - 1;
      const [lt] = await doc.getLines(last, last + 1);
      doc.setLine(last, (lt ?? "") + closers);
      out = null;
      log("success", `SmartFix applied: appended ${closers}`);
      await rescan(doc, "Re-validating");
      return;
    }
    if (out === null || out === text) {
      log("warning", `SmartFix "${fix}" needs the schema-aware fixer (Phase 2). Jumped to Ln ${e.line}.`, { line: e.line, col: e.col, docId: doc.id });
      setGoto({ line: e.line, col: e.col, nonce: Math.random() });
      return;
    }
    doc.setLine(ln, out);
    bump();
    log("success", `SmartFix applied at Ln ${e.line}: ${fix}`);
    setGoto({ line: e.line, col: e.col, nonce: Math.random() });
    if (doc.inMemory) await rescan(doc, "Re-validating");
  };

  const save = async () => {
    if (!active) return;
    if (active.provisional) {
      log("warning", "Wait for indexing to finish before saving.");
      return;
    }
    const t0 = performance.now();
    const blob = await active.toBlob();
    const w = window as any;
    try {
      if (w.showSaveFilePicker) {
        const h = await w.showSaveFilePicker({ suggestedName: active.name, types: [{ description: "XML", accept: { "application/xml": [".xml", ".xsd", ".xsl"] } }] });
        const ws = await h.createWritable();
        await blob.stream().pipeTo(ws); // streamed: untouched ranges are Blob slices, never in JS heap
      } else {
        const a = document.createElement("a");
        a.href = URL.createObjectURL(blob);
        a.download = active.name;
        a.click();
        setTimeout(() => URL.revokeObjectURL(a.href), 10_000);
      }
      active.dirty = false;
      bump();
      log("success", `Saved ${active.name} (${fmtBytes(blob.size)}) in ${(performance.now() - t0).toFixed(0)} ms — streamed diff-apply of ${active.editCount} edit(s).`);
    } catch (e: any) {
      if (e?.name !== "AbortError") log("error", `Save failed: ${e.message || e}`);
    }
  };

  // ---------------------------------------------------------------- search / xpath
  const runSearch = async (q: string, cs: boolean) => {
    if (!active) return;
    setFindQuery(q);
    if (active.editCount) await active.commitEdits();
    const w = new EngineWorker();
    setSearchWorker(w);
    setHits([]);
    setHitTotal(0);
    setSearchProg({ bytes: 0, total: active.file.size, elapsedMs: 0 });
    try {
      const r = await w.search(active.file, q, cs, (p) => {
        setSearchProg({ bytes: p.bytes, total: p.total, elapsedMs: p.elapsedMs });
        setHitTotal(p.hits);
      });
      if (r) {
        setHits(r.hits);
        setHitTotal(r.totalHits);
        const mbps = r.elapsedMs > 0 ? (r.bytes / 1e6 / (r.elapsedMs / 1000)).toFixed(0) : "∞";
        log("info", `Find "${q}": ${fmtNum(r.totalHits)} hit(s) in ${(r.elapsedMs / 1000).toFixed(2)} s (${mbps} MB/s streamed, ${r.engine === "rust-wasm" ? "Rust/WASM Finder" : "JS fallback"}).`);
        if (r.hits[0]) setGoto({ line: r.hits[0].line, col: r.hits[0].col, nonce: Math.random() });
      } else log("warning", "Search cancelled.");
    } catch (e: any) {
      log("error", `Search failed: ${e.message}`);
    } finally {
      w.terminate();
      setSearchWorker(null);
      setSearchProg(null);
    }
  };

  const runXPath = async (expr: string) => {
    if (!active) return;
    setXpRunning(true);
    setXpError(null);
    try {
      const r = await evaluateXPath(active, expr);
      setXpResult(r);
      log("info", `XPath ${expr} → ${r.kind === "value" ? r.value : fmtNum(r.count) + " node(s)"} in ${r.elapsedMs.toFixed(1)} ms [${r.engine}]`);
    } catch (e: any) {
      setXpResult(null);
      setXpError(e.message);
      log("error", `XPath error: ${e.message}`);
    } finally {
      setXpRunning(false);
    }
  };

  // ---------------------------------------------------------------- commands
  const command = useCallback(
    async (cmd: string, label?: string, phase?: number) => {
      if (cmd.startsWith("view:")) {
        setView(cmd.slice(5) as ViewKind);
        return;
      }
      if (cmd.startsWith("bottom:")) {
        setBottomTab(cmd.slice(7) as BottomTab);
        setBottomOpen(true);
        return;
      }
      switch (cmd) {
        case "new":
          return void openText(`Untitled${docs.length + 1}.xml`, `<?xml version="1.0" encoding="UTF-8"?>\n<root>\n  <item id="1">Hello XMLSpy-rs</item>\n</root>\n`);
        case "open":
          return fileInput.current?.click();
        case "reload":
        case "wf":
          if (active) await rescan(active, cmd === "wf" ? "Checking well-formedness of" : "Reloading");
          return;
        case "validate":
          if (!active) return;
          await rescan(active, "Validating");
          {
            const head = (await active.getLines(0, 40)).join("\n");
            const m = /(?:xsi:)?(?:noNamespace)?[sS]chemaLocation\s*=\s*"([^"]+)"/.exec(head);
            if (m) log("warning", `Schema validation against "${m[1].trim().split(/\s+/).pop()}" requires the XSD 1.0/1.1 validator (xmlspy-xsd, Phase 2). Well-formedness check completed above.`);
            else log("warning", "No schema assigned (DTD/Schema ▸ Assign Schema, Phase 2). Only well-formedness was checked.");
          }
          return;
        case "save":
          return save();
        case "close":
          if (active) {
            setDocs((ds) => ds.filter((d) => d !== active));
            setActiveId((id) => (id === active.id ? docs.find((d) => d !== active)?.id ?? null : id));
          }
          return;
        case "closeAll":
          setDocs([]);
          setActiveId(null);
          return;
        case "undo":
        case "redo": {
          if (!active) return;
          const l = cmd === "undo" ? active.undo() : active.redo();
          if (l === null) return log("info", `Nothing to ${cmd}.`);
          bump();
          setGoto({ line: l + 1, nonce: Math.random() });
          if (active.inMemory) await rescan(active, "Re-validating");
          return;
        }
        case "find":
          setBottomTab("find");
          setBottomOpen(true);
          return;
        case "findNext":
          if (hits.length) {
            const i = hits.findIndex((h) => h.line > cursor.line);
            const h = hits[i >= 0 ? i : 0];
            setGoto({ line: h.line, col: h.col, nonce: Math.random() });
          }
          return;
        case "goto": {
          if (!active) return;
          const v = prompt(`Go to line (1 – ${fmtNum(active.lineCount)}):`, String(cursor.line));
          const n = v ? parseInt(v.replace(/[^\d]/g, ""), 10) : NaN;
          if (!isNaN(n)) {
            setView("text");
            setGoto({ line: n, nonce: Math.random() });
          }
          return;
        }
        case "pretty":
          if (!active) return;
          if (!active.inMemory) return log("warning", "Pretty-print of multi-GB documents is a streaming rewrite job in the Rust engine (Phase 1); not run in the browser demo.");
          {
            const t = prettyPrint(await active.fullText());
            await openText(active.name, t, active.id);
            log("success", "Pretty-printed (XML ▸ Pretty-Print).");
          }
          return;
        case "genSchema":
          if (!active) return;
          {
            const s = await inferSchema(active);
            await openText(active.name.replace(/\.\w+$/, "") + ".xsd", schemaToXsd(s));
            log("success", `Generated XSD with ${s.size} element declarations from the structural index.`);
          }
          return;
        case "xpath":
          setBottomTab("xpath");
          setBottomOpen(true);
          return;
        case "theme":
          setDark((d) => !d);
          return;
        case "toggle:project":
          setShowProject((s) => !s);
          return;
        case "toggle:info":
          setShowInfo((s) => !s);
          return;
        case "cancel":
          job?.worker.cancel();
          searchWorker?.cancel();
          return;
        case "keys":
          setView("docs");
          log("info", "Keyboard map: see Architecture ▸ §7 UI/UX Specification. Ctrl+O open · Ctrl+S save · F7 well-formedness · F8 validate · Ctrl+F find · F3 next · Ctrl+G go to line · Ctrl+Shift+E XPath · Ctrl+1…5 views · Esc cancel job.");
          return;
        case "about":
          log("info", "XMLSpy-rs — browser-native XML IDE. Engine: single-pass resumable scanner + sparse structural index + LRU paging, Rust/WASM architecture documented under Ctrl+5.");
          return;
        default:
          log("info", `"${label ?? cmd}" is scheduled for Phase ${phase ?? "?"} (see Architecture ▸ Roadmap, Ctrl+5).`);
      }
    },
    [active, docs, hits, cursor.line, job, searchWorker, openText, rescan, log]
  );

  const onMenu = (it: MenuItem) => {
    if (it.cmd) command(it.cmd, it.label, it.phase);
    else command("__phase", it.label, it.phase);
  };

  // ---------------------------------------------------------------- keyboard map
  useEffect(() => {
    const h = (e: KeyboardEvent) => {
      const inInput = (e.target as HTMLElement)?.tagName === "INPUT" || (e.target as HTMLElement)?.tagName === "TEXTAREA";
      const k = e.key.toLowerCase();
      const ctrl = e.ctrlKey || e.metaKey;
      let c: string | null = null;
      if (ctrl && !e.shiftKey && k === "o") c = "open";
      else if (ctrl && k === "s") c = "save";
      else if (ctrl && !e.shiftKey && k === "n") c = "new";
      else if (ctrl && !e.shiftKey && k === "f") c = "find";
      else if (ctrl && e.shiftKey && k === "f") c = "find";
      else if (ctrl && k === "h") c = "find";
      else if (ctrl && k === "g") c = "goto";
      else if (ctrl && e.shiftKey && k === "e") c = "xpath";
      else if (ctrl && e.shiftKey && k === "p") c = "pretty";
      else if (ctrl && k === "z" && !inInput) c = "undo";
      else if (ctrl && k === "y" && !inInput) c = "redo";
      else if (e.key === "F7") c = "wf";
      else if (e.key === "F8") c = "validate";
      else if (e.key === "F10") c = "view:browser";
      else if (e.key === "F3") c = "findNext";
      else if (e.key === "F5") c = "reload";
      else if (e.key === "Escape") c = "cancel";
      else if (ctrl && !inInput && ["1", "2", "3", "4", "5"].includes(e.key)) c = "view:" + ["text", "grid", "schema", "browser", "docs"][parseInt(e.key) - 1];
      if (c) {
        e.preventDefault();
        command(c);
      }
    };
    window.addEventListener("keydown", h);
    return () => window.removeEventListener("keydown", h);
  }, [command]);

  const gotoLine = (line: number, col = 1) => {
    if (view !== "text" && view !== "grid") setView("text");
    if (view === "grid") setView("text");
    setGoto({ line, col, nonce: Math.random() });
  };

  // ---------------------------------------------------------------- render
  const hasDoc = !!active;
  const mainView = (() => {
    if (view === "docs") return <DocsView />;
    if (!active) return <DocsView />;
    if (view === "text") return <TextView key={active.id} doc={active} goto={goto} errors={active.errors} onCursor={(l, c) => setCursor({ line: l, col: c })} onEdit={onEditLine} version={active.version} />;
    if (view === "grid") return <GridView key={active.id + ":" + active.version} doc={active} selected={selectedNode} onSelect={setSelectedNode} onGoto={(l) => gotoLine(l)} />;
    if (view === "schema") return <SchemaView key={active.id} doc={active} onOpenXsd={(n, t) => openText(n, t)} />;
    return <BrowserView key={active.id} doc={active} />;
  })();

  const bottomTabs: { k: BottomTab; l: string }[] = [
    { k: "messages", l: `Messages${messages.length ? ` (${messages.length})` : ""}` },
    { k: "find", l: "Find in Files" },
    { k: "xpath", l: "XPath/XQuery" },
    { k: "bench", l: "Benchmarks" },
  ];

  return (
    <div className="h-full flex flex-col" style={{ background: "var(--bg)", color: "var(--fg)" }}>
      <input ref={fileInput} type="file" multiple accept=".xml,.xsd,.xsl,.xslt,.wsdl,.svg,.xhtml,.dtd,.rng,.sch,.json,.txt,*/*" className="hidden" onChange={(e) => openFiles(e.target.files).then(() => (e.target.value = ""))} />
      <MenuBar onCommand={onMenu} />
      <Toolbar onCmd={(c) => command(c)} canUndo={!!active && active.undoStack.length > 0} canRedo={!!active && active.redoStack.length > 0} hasDoc={hasDoc} busy={!!job || !!searchWorker} />
      <div className="flex-1 flex min-h-0">
        {showProject && (
          <aside className="w-64 shrink-0 flex flex-col" style={{ borderRight: "1px solid var(--border)", background: "var(--panel)" }}>
            <div className="panel-title">
              <span>Project</span>
              <button className="text-[11px]" onClick={() => setShowProject(false)} title="Hide (View ▸ Project Window)">
                ✕
              </button>
            </div>
            <div className="flex-1 min-h-0 overflow-auto">
              <ProjectPanel docs={docs} active={activeId} onSelect={setActiveId} onOpenSample={openSample} onOpenFile={() => fileInput.current?.click()} onCorpus={openCorpus} busy={!!job} />
            </div>
            {showInfo && (
              <div className="h-[38%] flex flex-col" style={{ borderTop: "1px solid var(--border)" }}>
                <div className="panel-title">
                  <span>Info</span>
                  <button className="text-[11px]" onClick={() => setShowInfo(false)}>
                    ✕
                  </button>
                </div>
                <div className="flex-1 min-h-0 overflow-auto">
                  <InfoPanel doc={active} nodeId={selectedNode} onGoto={(l) => gotoLine(l)} />
                </div>
              </div>
            )}
          </aside>
        )}
        <main className="flex-1 flex flex-col min-w-0 min-h-0">
          <DocTabs docs={docs} active={activeId} onSelect={setActiveId} onClose={(id) => {
            setDocs((ds) => ds.filter((d) => d.id !== id));
            setActiveId((a) => (a === id ? docs.find((d) => d.id !== id)?.id ?? null : a));
          }} />
          <div className="flex-1 min-h-0 relative">
            {mainView}
            {active?.provisional && view !== "docs" && (
              <div className="absolute top-2 right-6 text-[11px] px-2 py-1 rounded shadow" style={{ background: "var(--accent-soft)", border: "1px solid var(--accent)" }}>
                Showing first {fmtBytes(active.indexedEnd)} while the worker indexes the rest… ({fmtNum(active.lineCount)} lines so far)
              </div>
            )}
          </div>
          <ViewTabs view={view} onView={setView} hasDoc={hasDoc} />
          <div className="flex flex-col" style={{ height: bottomOpen ? 210 : 24, borderTop: "1px solid var(--border)", background: "var(--panel)" }}>
            <div className="flex items-center gap-[2px] px-1 h-6 shrink-0" style={{ background: "var(--panel-2)", borderBottom: "1px solid var(--border)" }}>
              {bottomTabs.map((t) => (
                <button key={t.k} className="px-3 h-full text-[11px]" style={{ background: bottomTab === t.k && bottomOpen ? "var(--panel)" : "transparent", fontWeight: bottomTab === t.k ? 600 : 400, borderBottom: bottomTab === t.k && bottomOpen ? "2px solid var(--accent)" : "2px solid transparent" }} onClick={() => {
                  setBottomTab(t.k);
                  setBottomOpen(true);
                }}>
                  {t.l}
                </button>
              ))}
              <button className="ml-auto px-2 text-[11px]" onClick={() => setBottomOpen(!bottomOpen)} title="Toggle bottom dock">
                {bottomOpen ? "▾" : "▴"}
              </button>
            </div>
            {bottomOpen && (
              <div className="flex-1 min-h-0">
                {bottomTab === "messages" && <MessagesPanel messages={messages} onGoto={(m) => {
                  if (m.docId && m.docId !== activeId) setActiveId(m.docId);
                  gotoLine(m.line!, m.col ?? 1);
                }} onFix={applyFix} onClear={() => setMessages([])} />}
                {bottomTab === "find" && <FindPanel doc={active} onSearch={runSearch} onCancel={() => searchWorker?.cancel()} hits={hits} total={hitTotal} progress={searchProg} running={!!searchWorker} onGoto={(l, c) => gotoLine(l, c)} initialQuery={findQuery} />}
                {bottomTab === "xpath" && <XPathPanel doc={active} onEval={runXPath} result={xpResult} error={xpError} running={xpRunning} onGoto={(l) => gotoLine(l)} onSelectNode={setSelectedNode} />}
                {bottomTab === "bench" && <BenchPanel doc={active} frame={frame} memory={memory} onRescan={() => active && rescan(active, "Benchmark scan of")} blockCache={active?.cacheSize ?? 0} engine={engineInfo()} />}
              </div>
            )}
          </div>
        </main>
      </div>
      <StatusBar doc={active} cursor={cursor} job={job ? { label: job.label, pct: job.pct, text: job.text } : null} frame={frame} memory={memory} view={view} engine={engineName()} />
    </div>
  );
}
