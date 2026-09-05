import { useEffect, useMemo, useRef, useState } from "react";
import type { NodeInfo, XmlDocument } from "../engine/document";
import { fmtNum } from "../engine/document";

const ROW = 24;
const MAX_CHILDREN_SHOWN = 5000;

type Row = { id: number; depth: number; more?: number };

export function GridView({ doc, onSelect, selected, onGoto }: { doc: XmlDocument; onSelect: (id: number) => void; selected: number | null; onGoto: (line: number) => void }) {
  const [expanded, setExpanded] = useState<Set<number>>(() => new Set(doc.rootIds()));
  const [limits, setLimits] = useState<Map<number, number>>(new Map());
  const [infos, setInfos] = useState<Map<number, NodeInfo>>(new Map());
  const [first, setFirst] = useState(0);
  const [viewportH, setViewportH] = useState(500);
  const [tableFor, setTableFor] = useState<number | null>(null);
  const ref = useRef<HTMLDivElement>(null);
  const infoCache = useRef(new Map<number, NodeInfo>());

  // Flatten expanded tree — this is the only structure proportional to *visible* state, not file size.
  const rows: Row[] = useMemo(() => {
    const out: Row[] = [];
    const walk = (id: number, depth: number) => {
      out.push({ id, depth });
      if (!expanded.has(id)) return;
      const kids = doc.childrenOf(id, 200_000);
      const lim = limits.get(id) ?? MAX_CHILDREN_SHOWN;
      for (let i = 0; i < Math.min(kids.length, lim); i++) walk(kids[i], depth + 1);
      if (kids.length > lim) out.push({ id, depth: depth + 1, more: kids.length - lim });
    };
    for (const r of doc.rootIds()) walk(r, 0);
    return out;
  }, [doc, expanded, limits, doc.version]);

  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const ro = new ResizeObserver(() => setViewportH(el.clientHeight));
    ro.observe(el);
    setViewportH(el.clientHeight);
    return () => ro.disconnect();
  }, []);

  const rowsVisible = Math.ceil(viewportH / ROW) + 2;
  const start = Math.max(0, first - 5);
  const end = Math.min(rows.length, first + rowsVisible + 5);

  // Materialize node details for the visible rows only (bounded reads via Blob.slice).
  useEffect(() => {
    let cancelled = false;
    const need: number[] = [];
    for (let i = start; i < end; i++) {
      const r = rows[i];
      if (r && r.more === undefined && !infoCache.current.has(r.id)) need.push(r.id);
    }
    if (!need.length) return;
    Promise.all(need.map((id) => doc.nodeInfo(id))).then((list) => {
      if (cancelled) return;
      for (const info of list) infoCache.current.set(info.id, info);
      if (infoCache.current.size > 20_000) {
        // LRU-ish eviction: drop oldest half
        const entries = Array.from(infoCache.current.entries());
        infoCache.current = new Map(entries.slice(entries.length / 2));
      }
      setInfos(new Map(infoCache.current));
    });
    return () => {
      cancelled = true;
    };
  }, [rows, start, end, doc, doc.version]);

  useEffect(() => {
    infoCache.current.clear();
    setInfos(new Map());
  }, [doc.version]);

  const toggle = (id: number) => {
    const s = new Set(expanded);
    if (s.has(id)) s.delete(id);
    else s.add(id);
    setExpanded(s);
  };

  const onKey = (e: React.KeyboardEvent) => {
    if (selected === null) return;
    const idx = rows.findIndex((r) => r.id === selected && r.more === undefined);
    if (e.key === "ArrowDown" && idx < rows.length - 1) onSelect(rows[idx + 1].id);
    else if (e.key === "ArrowUp" && idx > 0) onSelect(rows[idx - 1].id);
    else if (e.key === "ArrowRight" && doc.hasChildren(selected) && !expanded.has(selected)) toggle(selected);
    else if (e.key === "ArrowLeft" && expanded.has(selected)) toggle(selected);
    else if (e.key === "Enter") onGoto(doc.index.elemLine[selected]);
    else return;
    e.preventDefault();
  };

  // ---- Table mode for repeating siblings (XMLSpy "Display as Table") ----
  const table = useMemo(() => {
    if (tableFor === null) return null;
    const kids = doc.childrenOf(tableFor, 100_000);
    if (kids.length < 2) return null;
    const name = doc.nameOf(kids[0]);
    const same = kids.filter((k) => doc.nameOf(k) === name);
    return { parent: tableFor, name, ids: same };
  }, [tableFor, doc]);

  if (table) return <TableMode doc={doc} table={table} onBack={() => setTableFor(null)} onGoto={onGoto} onSelect={onSelect} />;

  const render: React.ReactNode[] = [];
  for (let i = start; i < end; i++) {
    const r = rows[i];
    if (!r) continue;
    if (r.more !== undefined) {
      render.push(
        <div key={`more-${r.id}`} className="absolute left-0 right-0 flex items-center px-2 text-[11px]" style={{ top: i * ROW, height: ROW, color: "var(--fg-muted)" }}>
          <span style={{ paddingLeft: r.depth * 18 }} />
          <button className="btn" onClick={() => setLimits(new Map(limits).set(r.id, (limits.get(r.id) ?? MAX_CHILDREN_SHOWN) + MAX_CHILDREN_SHOWN))}>
            … {fmtNum(r.more)} more siblings — show next {fmtNum(MAX_CHILDREN_SHOWN)}
          </button>
        </div>
      );
      continue;
    }
    const info = infos.get(r.id);
    const hasKids = doc.hasChildren(r.id);
    const open = expanded.has(r.id);
    const sel = selected === r.id;
    render.push(
      <div
        key={r.id}
        className={`absolute left-0 right-0 flex items-center text-[12px] cursor-default ${sel ? "row-sel" : ""}`}
        style={{ top: i * ROW, height: ROW, borderBottom: "1px solid var(--border)" }}
        role="row"
        aria-level={r.depth + 1}
        aria-expanded={hasKids ? open : undefined}
        onClick={() => onSelect(r.id)}
        onDoubleClick={() => onGoto(doc.index.elemLine[r.id])}
      >
        <span style={{ width: 8 + r.depth * 18 }} className="shrink-0" />
        <button
          className="w-4 h-4 mr-1 shrink-0 inline-flex items-center justify-center text-[10px] rounded-sm"
          style={{ border: hasKids ? "1px solid var(--border)" : "1px solid transparent", color: "var(--fg-muted)" }}
          onClick={(e) => {
            e.stopPropagation();
            if (hasKids) toggle(r.id);
          }}
        >
          {hasKids ? (open ? "−" : "+") : ""}
        </button>
        <span className="mono mr-2 shrink-0" style={{ color: "var(--tok-tag)" }}>
          {"<> "}
          {doc.nameOf(r.id)}
        </span>
        {info ? (
          <>
            <span className="mono truncate mr-2" style={{ color: "var(--tok-attr)" }}>
              {info.attrs.map(([k, v]) => (
                <span key={k} className="mr-2">
                  {"= "}
                  {k}
                  <span style={{ color: "var(--tok-val)" }}>{`="${v.length > 40 ? v.slice(0, 40) + "…" : v}"`}</span>
                </span>
              ))}
            </span>
            {info.text && (
              <span className="truncate" style={{ color: "var(--tok-text)" }}>
                {info.text.length > 120 ? info.text.slice(0, 120) + "…" : info.text}
              </span>
            )}
            {hasKids && (
              <span className="ml-auto mr-2 text-[10px] shrink-0 inline-flex items-center gap-2" style={{ color: "var(--fg-muted)" }}>
                {fmtNum(info.childCount)} children
                {info.childCount >= 2 && (
                  <button
                    className="btn"
                    style={{ padding: "0 5px" }}
                    onClick={(e) => {
                      e.stopPropagation();
                      setTableFor(r.id);
                    }}
                  >
                    ▦ table
                  </button>
                )}
              </span>
            )}
          </>
        ) : (
          <span className="text-[10px]" style={{ color: "var(--fg-muted)" }}>
            loading…
          </span>
        )}
      </div>
    );
  }

  return (
    <div ref={ref} tabIndex={0} role="tree" aria-rowcount={rows.length} className="relative h-full w-full overflow-auto outline-none" style={{ background: "var(--panel)" }} onScroll={() => setFirst(Math.floor(ref.current!.scrollTop / ROW))} onKeyDown={onKey}>
      <div style={{ height: rows.length * ROW, position: "relative" }}>{render}</div>
    </div>
  );
}

function TableMode({ doc, table, onBack, onGoto, onSelect }: { doc: XmlDocument; table: { parent: number; name: string; ids: number[] }; onBack: () => void; onGoto: (l: number) => void; onSelect: (id: number) => void }) {
  const [first, setFirst] = useState(0);
  const [viewportH, setViewportH] = useState(500);
  const [rowsData, setRowsData] = useState<Map<number, Record<string, string>>>(new Map());
  const [cols, setCols] = useState<string[]>([]);
  const [sortCol, setSortCol] = useState<string | null>(null);
  const [filter, setFilter] = useState("");
  const ref = useRef<HTMLDivElement>(null);
  const R = 22;

  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const ro = new ResizeObserver(() => setViewportH(el.clientHeight));
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  const sortedIds = useMemo(() => {
    let ids = table.ids;
    if (filter) {
      const f = filter.toLowerCase();
      ids = ids.filter((id) => {
        const r = rowsData.get(id);
        return r ? Object.values(r).some((v) => v.toLowerCase().includes(f)) : true;
      });
    }
    if (!sortCol) return ids;
    const known = ids.filter((id) => rowsData.has(id));
    const unknown = ids.filter((id) => !rowsData.has(id));
    known.sort((a, b) => {
      const va = rowsData.get(a)![sortCol] ?? "";
      const vb = rowsData.get(b)![sortCol] ?? "";
      const na = parseFloat(va),
        nb = parseFloat(vb);
      if (!isNaN(na) && !isNaN(nb)) return na - nb;
      return va.localeCompare(vb);
    });
    return [...known, ...unknown];
  }, [table.ids, sortCol, rowsData, filter]);

  const vis = Math.ceil(viewportH / R) + 2;
  const start = Math.max(0, first - 3);
  const end = Math.min(sortedIds.length, first + vis + 3);

  useEffect(() => {
    let cancelled = false;
    const need = sortedIds.slice(start, end).filter((id) => !rowsData.has(id));
    if (!need.length) return;
    (async () => {
      const next = new Map(rowsData);
      const colSet = new Set(cols);
      for (const id of need) {
        const info = await doc.nodeInfo(id);
        const rec: Record<string, string> = {};
        for (const [k, v] of info.attrs) {
          rec["@" + k] = v;
          colSet.add("@" + k);
        }
        if (info.text) {
          rec["#text"] = info.text;
          colSet.add("#text");
        }
        for (const c of doc.childrenOf(id, 50)) {
          const ci = await doc.nodeInfo(c);
          const key = ci.name;
          rec[key] = ci.hasChildren ? `(${ci.childCount} children)` : ci.text || ci.attrs.map(([k, v]) => `${k}=${v}`).join(" ");
          colSet.add(key);
        }
        next.set(id, rec);
      }
      if (cancelled) return;
      setRowsData(next);
      setCols(Array.from(colSet));
    })();
    return () => {
      cancelled = true;
    };
  }, [sortedIds, start, end]); // eslint-disable-line react-hooks/exhaustive-deps

  return (
    <div className="flex flex-col h-full" style={{ background: "var(--panel)" }}>
      <div className="flex items-center gap-2 px-2 py-1 text-[11px]" style={{ borderBottom: "1px solid var(--border)", background: "var(--panel-2)" }}>
        <button className="btn" onClick={onBack}>
          ◀ Grid
        </button>
        <span>
          Table: <b>{table.name}</b> × {fmtNum(table.ids.length)} rows under {doc.nameOf(table.parent)}
        </span>
        <input className="input ml-auto w-56" placeholder="Inline filter (loaded rows)…" value={filter} onChange={(e) => setFilter(e.target.value)} />
        {sortCol && (
          <button className="btn" onClick={() => setSortCol(null)}>
            clear sort
          </button>
        )}
      </div>
      <div className="flex text-[11px] font-semibold" style={{ borderBottom: "1px solid var(--border)", background: "var(--panel-2)" }}>
        <span className="w-14 px-2 shrink-0">#</span>
        {cols.map((c) => (
          <span key={c} className="w-40 px-2 truncate shrink-0 cursor-pointer hover:underline" onClick={() => setSortCol(c)} title="Click to sort (loaded rows)">
            {c}
            {sortCol === c ? " ▲" : ""}
          </span>
        ))}
      </div>
      <div ref={ref} className="relative flex-1 overflow-auto" onScroll={() => setFirst(Math.floor(ref.current!.scrollTop / R))}>
        <div style={{ height: sortedIds.length * R, position: "relative", minWidth: 56 + cols.length * 160 }}>
          {sortedIds.slice(start, end).map((id, k) => {
            const i = start + k;
            const rec = rowsData.get(id);
            return (
              <div key={id} className="absolute left-0 right-0 flex text-[11.5px] mono hover:bg-[var(--sel)]" style={{ top: i * R, height: R, lineHeight: `${R}px`, borderBottom: "1px solid var(--border)" }} onClick={() => onSelect(id)} onDoubleClick={() => onGoto(doc.index.elemLine[id])}>
                <span className="w-14 px-2 shrink-0" style={{ color: "var(--fg-muted)" }}>
                  {i + 1}
                </span>
                {cols.map((c) => (
                  <span key={c} className="w-40 px-2 truncate shrink-0" style={{ color: c.startsWith("@") ? "var(--tok-val)" : "var(--tok-text)" }}>
                    {rec ? rec[c] ?? "" : "…"}
                  </span>
                ))}
              </div>
            );
          })}
        </div>
      </div>
    </div>
  );
}
