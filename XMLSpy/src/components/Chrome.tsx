import { useEffect, useRef, useState } from "react";
import type { XmlDocument } from "../engine/document";
import { fmtBytes, fmtNum } from "../engine/document";

export type MenuItem = { label: string; shortcut?: string; cmd?: string; phase?: number; sep?: boolean; check?: boolean };
export type Menu = { label: string; items: MenuItem[] };

export const MENUS: Menu[] = [
  {
    label: "File",
    items: [
      { label: "New XML document", shortcut: "Ctrl+N", cmd: "new" },
      { label: "Open…", shortcut: "Ctrl+O", cmd: "open" },
      { label: "Open URL / WebDAV / FTP…", phase: 6 },
      { label: "Reload", shortcut: "F5", cmd: "reload" },
      { sep: true, label: "" },
      { label: "Save", shortcut: "Ctrl+S", cmd: "save" },
      { label: "Save As…", shortcut: "Ctrl+Shift+S", cmd: "save" },
      { label: "Save All", cmd: "save" },
      { label: "Close", shortcut: "Ctrl+F4", cmd: "close" },
      { sep: true, label: "" },
      { label: "Encoding…", phase: 6 },
      { label: "Print…", shortcut: "Ctrl+P", phase: 6 },
    ],
  },
  {
    label: "Edit",
    items: [
      { label: "Undo", shortcut: "Ctrl+Z", cmd: "undo" },
      { label: "Redo", shortcut: "Ctrl+Y", cmd: "redo" },
      { sep: true, label: "" },
      { label: "Find…", shortcut: "Ctrl+F", cmd: "find" },
      { label: "Find Next", shortcut: "F3", cmd: "findNext" },
      { label: "Replace…", shortcut: "Ctrl+H", cmd: "find" },
      { label: "Find in Files…", shortcut: "Ctrl+Shift+F", cmd: "find" },
      { label: "Go to Line/Char…", shortcut: "Ctrl+G", cmd: "goto" },
      { sep: true, label: "" },
      { label: "Insert Bookmark", shortcut: "Ctrl+F2", phase: 1 },
      { label: "Toggle Multi-cursor / Column selection", phase: 1 },
    ],
  },
  {
    label: "Project",
    items: [
      { label: "New Project", phase: 6 },
      { label: "Add Files to Project…", cmd: "open" },
      { label: "Add Global Resource…", phase: 6 },
      { label: "Add URL / External Folder…", phase: 6 },
      { label: "Source Control (Git status / diff HEAD)", phase: 6 },
    ],
  },
  {
    label: "XML",
    items: [
      { label: "Check Well-formedness", shortcut: "F7", cmd: "wf" },
      { label: "Validate XML", shortcut: "F8", cmd: "validate" },
      { label: "Validate on Server (RaptorXML-style engine)", shortcut: "Ctrl+F8", phase: 6 },
      { sep: true, label: "" },
      { label: "Pretty-Print", shortcut: "Ctrl+Shift+P", cmd: "pretty" },
      { label: "Insert / Evaluate XPath…", shortcut: "Ctrl+Shift+E", cmd: "xpath" },
      { label: "Namespace Prefix…", phase: 2 },
      { label: "Generate Sample XML from Schema", phase: 3 },
      { label: "Compare open file with…  (2-way structural diff)", phase: 3 },
      { label: "Compare directories…", phase: 3 },
      { label: "3-way merge…", phase: 3 },
    ],
  },
  {
    label: "DTD/Schema",
    items: [
      { label: "Assign DTD…", shortcut: "Alt+F8", phase: 2 },
      { label: "Assign Schema…", phase: 2 },
      { label: "Generate DTD/Schema from XML instance", cmd: "genSchema" },
      { label: "Convert DTD ⇄ Schema", phase: 3 },
      { label: "Flatten Schema", phase: 3 },
      { label: "Create Schema Subset", phase: 3 },
      { label: "Generate Schema Documentation (HTML/PDF/Word)", phase: 3 },
    ],
  },
  {
    label: "Schema design",
    items: [
      { label: "Schema Overview / Content Model view", cmd: "view:schema" },
      { label: "Schema Settings (XSD 1.0 / 1.1)…", phase: 3 },
      { label: "Configure View…", phase: 3 },
      { label: "Schema-aware Rename (refactor across instances)", phase: 3 },
    ],
  },
  {
    label: "XSL/XQuery",
    items: [
      { label: "XSL Transformation (Browser view, XSLT 1.0)", shortcut: "F10", cmd: "view:browser" },
      { label: "XSL Transformation 2.0/3.0 (streaming engine)", phase: 3 },
      { label: "XSL:FO Transformation", shortcut: "Ctrl+F10", phase: 3 },
      { label: "XQuery 3.1 Execution", shortcut: "Ctrl+Shift+F10", phase: 3 },
      { label: "XSLT/XQuery Debugger", shortcut: "Alt+F11", phase: 3 },
      { label: "XSLT/XQuery Profiler", phase: 3 },
      { label: "XSL Speed Optimizer", phase: 3 },
      { label: "XPath/XQuery Window", shortcut: "Ctrl+Shift+E", cmd: "xpath" },
    ],
  },
  { label: "Authentic", items: [{ label: "Authentic View (SPS eForms)", phase: 5 }, { label: "Assign StyleVision Power Stylesheet…", phase: 5 }] },
  {
    label: "DB",
    items: [
      { label: "Query Database (SQL Server, Oracle, PostgreSQL, MySQL, DB2, SQLite)…", phase: 4 },
      { label: "Import Database Data…", phase: 4 },
      { label: "Export to Database…", phase: 4 },
      { label: "Create XML Schema from DB Structure…", phase: 4 },
      { label: "Create DB Structure from XML Schema…", phase: 4 },
    ],
  },
  {
    label: "Convert",
    items: [
      { label: "Convert XML ⇄ JSON / YAML", phase: 4 },
      { label: "Convert JSON Schema ⇄ XML Schema", phase: 4 },
      { label: "Import Text / CSV file…", phase: 4 },
      { label: "Import Word document / Export to text", phase: 4 },
      { label: "Apache Avro: JSON ⇄ Avro binary", phase: 4 },
    ],
  },
  {
    label: "View",
    items: [
      { label: "Text View", shortcut: "Ctrl+1", cmd: "view:text" },
      { label: "Grid View", shortcut: "Ctrl+2", cmd: "view:grid" },
      { label: "Schema View", shortcut: "Ctrl+3", cmd: "view:schema" },
      { label: "Browser View", shortcut: "Ctrl+4", cmd: "view:browser" },
      { label: "Architecture / Rust Source", shortcut: "Ctrl+5", cmd: "view:docs" },
      { sep: true, label: "" },
      { label: "Project Window", cmd: "toggle:project" },
      { label: "Info Window", cmd: "toggle:info" },
      { label: "Messages", cmd: "bottom:messages" },
      { label: "Find in Files", cmd: "bottom:find" },
      { label: "XPath/XQuery", cmd: "bottom:xpath" },
      { label: "Benchmarks", cmd: "bottom:bench" },
      { sep: true, label: "" },
      { label: "Toggle Dark Theme", cmd: "theme" },
    ],
  },
  { label: "WSDL", items: [{ label: "WSDL Designer 1.1/2.0", phase: 5 }, { label: "Generate WSDL Documentation", phase: 5 }] },
  { label: "SOAP", items: [{ label: "Create New SOAP Request…", phase: 5 }, { label: "Send Request to Server", phase: 5 }, { label: "SOAP Debugger (proxy)", phase: 5 }] },
  {
    label: "XBRL",
    items: [
      { label: "Validate XBRL instance (2.1 + Dimensions)", phase: 5 },
      { label: "Taxonomy Editor", phase: 5 },
      { label: "Table Linkbase / Formula / XULE editors", phase: 5 },
      { label: "Inline XBRL viewer", phase: 5 },
      { label: "EDGAR / DQC rules", phase: 5 },
    ],
  },
  {
    label: "Tools",
    items: [
      { label: "HTTP / REST client", phase: 4 },
      { label: "Charts wizard", phase: 4 },
      { label: "Code Generation (C++ / C# / Java / Rust / TS)", phase: 4 },
      { label: "Digital Signature: Sign / Verify (XML-DSig)", phase: 5 },
      { label: "Scripting / Macros (Rhai)", phase: 6 },
      { label: "Spell check", phase: 6 },
      { label: "Global Resources / Catalogs", phase: 6 },
      { label: "Options…", cmd: "theme" },
    ],
  },
  { label: "Window", items: [{ label: "Cascade / Tile / Split", phase: 1 }, { label: "Close All", cmd: "closeAll" }] },
  { label: "Help", items: [{ label: "Keyboard Map", cmd: "keys" }, { label: "Architecture Document", cmd: "view:docs" }, { label: "About XMLSpy-rs", cmd: "about" }] },
];

export function MenuBar({ onCommand }: { onCommand: (item: MenuItem) => void }) {
  const [open, setOpen] = useState<number | null>(null);
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (open === null) return;
    const h = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) setOpen(null);
    };
    document.addEventListener("mousedown", h);
    return () => document.removeEventListener("mousedown", h);
  }, [open]);
  return (
    <div ref={ref} className="flex items-center gap-0 px-1 select-none relative" style={{ background: "var(--panel-2)", borderBottom: "1px solid var(--border)" }}>
      <div className="flex items-center gap-1 px-2 mr-1 font-bold text-[12px]" style={{ color: "var(--accent)" }}>
        <svg width="16" height="16" viewBox="0 0 16 16" fill="none">
          <rect x="1" y="1" width="14" height="14" rx="2" fill="var(--accent)" />
          <path d="M4 5l4 3-4 3M9 11h3" stroke="#fff" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round" />
        </svg>
        XMLSpy-rs
      </div>
      {MENUS.map((m, i) => (
        <div key={m.label} className="relative">
          <button
            className="px-2 py-1 text-[12px] rounded-sm"
            style={{ background: open === i ? "var(--accent-soft)" : "transparent" }}
            onClick={() => setOpen(open === i ? null : i)}
            onMouseEnter={() => open !== null && setOpen(i)}
          >
            {m.label}
          </button>
          {open === i && (
            <div className="absolute left-0 top-full z-50 min-w-[300px] py-1 shadow-lg" style={{ background: "var(--panel)", border: "1px solid var(--border)" }}>
              {m.items.map((it, j) =>
                it.sep ? (
                  <div key={j} className="my-1" style={{ borderTop: "1px solid var(--border)" }} />
                ) : (
                  <div
                    key={j}
                    className="menu-item flex justify-between gap-6 px-3 py-[3px] cursor-pointer"
                    onClick={() => {
                      setOpen(null);
                      onCommand(it);
                    }}
                  >
                    <span style={{ color: it.phase && !it.cmd ? "var(--fg-muted)" : "var(--fg)" }}>
                      {it.label}
                      {it.phase && !it.cmd ? <span className="ml-2 text-[10px] px-1 rounded" style={{ background: "var(--panel-2)", border: "1px solid var(--border)" }}>Phase {it.phase}</span> : null}
                    </span>
                    <span style={{ color: "var(--fg-muted)" }}>{it.shortcut}</span>
                  </div>
                )
              )}
            </div>
          )}
        </div>
      ))}
    </div>
  );
}

function Icon({ d, size = 14 }: { d: string; size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round">
      <path d={d} />
    </svg>
  );
}

const I = {
  new: "M4 1h6l3 3v11H4zM10 1v3h3",
  open: "M1 4h5l2 2h7v8H1zM1 8h14",
  save: "M2 2h10l2 2v10H2zM5 2v4h6V2M4 14v-4h8v4",
  undo: "M6 4L2 8l4 4M2 8h8a4 4 0 010 8h-1",
  redo: "M10 4l4 4-4 4M14 8H6a4 4 0 000 8h1",
  find: "M7 12A5 5 0 107 2a5 5 0 000 10zM11 11l4 4",
  wf: "M2 8l4 4 8-8",
  validate: "M3 2h10v12H3zM6 8l2 2 3-3",
  xpath: "M2 4l3 4-3 4M7 12h7",
  pretty: "M2 3h12M4 6h8M6 9h4M2 12h12",
  schema: "M2 2h4v4H2zM10 2h4v4h-4zM6 10h4v4H6zM4 6v2h8V6M8 8v2",
  grid: "M2 2h12v12H2zM2 6h12M2 10h12M6 2v12",
  text: "M3 3h10M3 6h7M3 9h10M3 12h6",
  theme: "M8 2a6 6 0 100 12V2z",
  cancel: "M4 4l8 8M12 4l-8 8",
};

export function Toolbar({ onCmd, canUndo, canRedo, hasDoc, busy }: { onCmd: (c: string) => void; canUndo: boolean; canRedo: boolean; hasDoc: boolean; busy: boolean }) {
  const B = ({ c, d, t, dis }: { c: string; d: string; t: string; dis?: boolean }) => (
    <button className="tbtn" title={t} disabled={dis} onClick={() => onCmd(c)}>
      <Icon d={d} />
    </button>
  );
  const Sep = () => <span className="mx-1 h-5 w-px" style={{ background: "var(--border)" }} />;
  return (
    <div className="flex items-center gap-[2px] px-1 py-[2px]" style={{ background: "var(--panel-2)", borderBottom: "1px solid var(--border)" }}>
      <B c="new" d={I.new} t="New (Ctrl+N)" />
      <B c="open" d={I.open} t="Open (Ctrl+O)" />
      <B c="save" d={I.save} t="Save (Ctrl+S)" dis={!hasDoc} />
      <Sep />
      <B c="undo" d={I.undo} t="Undo (Ctrl+Z)" dis={!canUndo} />
      <B c="redo" d={I.redo} t="Redo (Ctrl+Y)" dis={!canRedo} />
      <Sep />
      <B c="find" d={I.find} t="Find (Ctrl+F)" dis={!hasDoc} />
      <B c="wf" d={I.wf} t="Check well-formedness (F7)" dis={!hasDoc} />
      <B c="validate" d={I.validate} t="Validate (F8)" dis={!hasDoc} />
      <B c="pretty" d={I.pretty} t="Pretty-print (Ctrl+Shift+P)" dis={!hasDoc} />
      <B c="xpath" d={I.xpath} t="XPath/XQuery window (Ctrl+Shift+E)" dis={!hasDoc} />
      <B c="genSchema" d={I.schema} t="Generate schema from instance" dis={!hasDoc} />
      <Sep />
      <B c="view:text" d={I.text} t="Text View (Ctrl+1)" />
      <B c="view:grid" d={I.grid} t="Grid View (Ctrl+2)" />
      <B c="view:schema" d={I.schema} t="Schema View (Ctrl+3)" />
      <Sep />
      <B c="theme" d={I.theme} t="Toggle theme" />
      {busy && (
        <>
          <Sep />
          <button className="btn" onClick={() => onCmd("cancel")} title="Cancel running job (Esc)">
            <span className="inline-flex items-center gap-1">
              <Icon d={I.cancel} size={11} /> Cancel job
            </span>
          </button>
        </>
      )}
    </div>
  );
}

export type ViewKind = "text" | "grid" | "schema" | "browser" | "docs";

export function DocTabs({ docs, active, onSelect, onClose }: { docs: XmlDocument[]; active: string | null; onSelect: (id: string) => void; onClose: (id: string) => void }) {
  return (
    <div className="flex items-end gap-[2px] px-1 pt-1 overflow-x-auto" style={{ background: "var(--bg)", borderBottom: "1px solid var(--border)" }}>
      {docs.map((d) => (
        <div
          key={d.id}
          className="flex items-center gap-2 px-3 py-1 text-[12px] cursor-pointer rounded-t"
          style={{
            background: d.id === active ? "var(--panel)" : "var(--panel-2)",
            border: "1px solid var(--border)",
            borderBottom: d.id === active ? "1px solid var(--panel)" : "1px solid var(--border)",
            marginBottom: -1,
            fontWeight: d.id === active ? 600 : 400,
          }}
          onClick={() => onSelect(d.id)}
        >
          <span>
            {d.name}
            {d.dirty ? " *" : ""}
          </span>
          <span className="text-[10px]" style={{ color: "var(--fg-muted)" }}>
            {fmtBytes(d.stats.bytes)}
          </span>
          <button
            className="text-[11px] px-1 rounded hover:bg-[var(--accent-soft)]"
            onClick={(e) => {
              e.stopPropagation();
              onClose(d.id);
            }}
          >
            ×
          </button>
        </div>
      ))}
      {docs.length === 0 && <div className="px-3 py-1 text-[11px]" style={{ color: "var(--fg-muted)" }}>No documents open — use File ▸ Open, or pick an example in the Project window.</div>}
    </div>
  );
}

export function ViewTabs({ view, onView, hasDoc }: { view: ViewKind; onView: (v: ViewKind) => void; hasDoc: boolean }) {
  const tabs: { k: ViewKind; l: string; key: string }[] = [
    { k: "text", l: "Text", key: "Ctrl+1" },
    { k: "grid", l: "Grid", key: "Ctrl+2" },
    { k: "schema", l: "Schema", key: "Ctrl+3" },
    { k: "browser", l: "Browser", key: "Ctrl+4" },
    { k: "docs", l: "Architecture & Rust Source", key: "Ctrl+5" },
  ];
  return (
    <div className="flex items-center gap-[2px] px-1" style={{ background: "var(--panel-2)", borderTop: "1px solid var(--border)" }}>
      {tabs.map((t) => (
        <button
          key={t.k}
          className="px-3 py-[3px] text-[11.5px] rounded-b"
          title={t.key}
          disabled={!hasDoc && t.k !== "docs"}
          style={{
            background: view === t.k ? "var(--panel)" : "transparent",
            border: view === t.k ? "1px solid var(--border)" : "1px solid transparent",
            borderTop: view === t.k ? "1px solid var(--panel)" : "1px solid transparent",
            marginTop: -1,
            fontWeight: view === t.k ? 600 : 400,
            opacity: !hasDoc && t.k !== "docs" ? 0.4 : 1,
          }}
          onClick={() => onView(t.k)}
        >
          {t.l}
        </button>
      ))}
    </div>
  );
}

export function StatusBar({ doc, cursor, job, frame, memory, view, engine }: { doc: XmlDocument | null; cursor: { line: number; col: number }; job: { label: string; pct: number; text: string } | null; frame: { p50: number; p99: number }; memory: number | null; view: ViewKind; engine?: "rust-wasm" | "typescript" }) {
  const cell = (t: string, title?: string) => (
    <span className="px-2 whitespace-nowrap" style={{ borderLeft: "1px solid var(--border)" }} title={title}>
      {t}
    </span>
  );
  return (
    <div className="flex items-center text-[11px] h-[22px] select-none overflow-hidden" style={{ background: "var(--panel-2)", borderTop: "1px solid var(--border)", color: "var(--fg)" }}>
      <span className="px-2 flex-1 truncate">{job ? job.text : doc ? `${doc.name} — ${doc.large ? "large-file mode (on-demand paging, streamed save)" : "in-memory piece-table mode"}` : "Ready"}</span>
      {job && (
        <span className="w-40 h-[8px] mx-2 rounded overflow-hidden" style={{ background: "var(--border)" }}>
          <span className="block h-full progress-anim" style={{ width: `${Math.max(2, job.pct * 100)}%`, background: "var(--accent)" }} />
        </span>
      )}
      {cell(`Ln ${fmtNum(cursor.line)}, Col ${cursor.col}`)}
      {doc && cell(`${fmtNum(doc.lineCount)} lines`)}
      {doc && cell(`${fmtNum(doc.stats.elements)} elements`, `${fmtNum(doc.stats.indexedElements)} indexed`)}
      {doc && cell(fmtBytes(doc.stats.bytes))}
      {doc && cell(doc.stats.errors === 0 ? "✓ well-formed" : `✗ ${fmtNum(doc.stats.errors)} error(s)`)}
      {cell(`frame p50 ${frame.p50.toFixed(1)} ms · p99 ${frame.p99.toFixed(1)} ms`, "UI thread frame time (rAF)")}
      {memory !== null && cell(`heap ${fmtBytes(memory)}`, "JS heap (performance.memory)")}
      {cell("UTF-8")}
      {engine && (
        <span
          className="px-2 whitespace-nowrap font-semibold"
          style={{ borderLeft: "1px solid var(--border)", color: engine === "rust-wasm" ? "var(--ok)" : "var(--warn)" }}
          title={engine === "rust-wasm" ? "Scanner, .xsi index and Finder run in the Rust engine compiled to WebAssembly" : "WebAssembly unavailable — using the TypeScript reference scanner"}
        >
          {engine === "rust-wasm" ? "⚙ Rust/WASM" : "⚙ TS fallback"}
        </span>
      )}
      {cell(view.toUpperCase())}
    </div>
  );
}
