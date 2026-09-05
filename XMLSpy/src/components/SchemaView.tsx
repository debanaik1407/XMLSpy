import { useEffect, useMemo, useState } from "react";
import type { XmlDocument } from "../engine/document";
import { fmtNum } from "../engine/document";
import { inferSchema, schemaToXsd, type InferredElement } from "../engine/schemaInfer";

/**
 * Schema View: XMLSpy-style content-model diagram. The schema is inferred from the
 * instance's structural index (Phase 2/3 ships the full XSD 1.1 designer on top of
 * xmlspy-xsd's SchemaModel; the diagram renderer is the same).
 */
export function SchemaView({ doc, onOpenXsd }: { doc: XmlDocument; onOpenXsd: (name: string, text: string) => void }) {
  const [schema, setSchema] = useState<Map<string, InferredElement> | null>(null);
  const [sel, setSel] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    let cancelled = false;
    setBusy(true);
    inferSchema(doc).then((s) => {
      if (cancelled) return;
      setSchema(s);
      setBusy(false);
      const root = Array.from(s.values()).find((e) => e.parents.size === 0);
      setSel((prev) => prev ?? root?.name ?? null);
    });
    return () => {
      cancelled = true;
    };
  }, [doc, doc.version]);

  const el = sel && schema ? schema.get(sel) : null;
  const components = useMemo(() => (schema ? Array.from(schema.values()).sort((a, b) => a.name.localeCompare(b.name)) : []), [schema]);

  return (
    <div className="flex h-full" style={{ background: "var(--panel)" }}>
      <div className="w-60 shrink-0 flex flex-col" style={{ borderRight: "1px solid var(--border)" }}>
        <div className="panel-title">
          <span>Schema Components</span>
          <span style={{ color: "var(--fg-muted)" }}>{components.length}</span>
        </div>
        <div className="flex-1 overflow-auto text-[12px]">
          {busy && <div className="p-2" style={{ color: "var(--fg-muted)" }}>Inferring from structural index…</div>}
          {components.map((c) => (
            <div key={c.name} className={`px-2 py-[2px] cursor-pointer flex justify-between ${sel === c.name ? "row-sel" : ""}`} onClick={() => setSel(c.name)}>
              <span className="mono">
                <span style={{ color: "var(--fg-muted)" }}>{c.children.length || c.attrs.size ? "◈ " : "◇ "}</span>
                {c.name}
              </span>
              <span style={{ color: "var(--fg-muted)" }}>{fmtNum(c.count)}</span>
            </div>
          ))}
        </div>
        <div className="p-2 flex flex-col gap-1" style={{ borderTop: "1px solid var(--border)" }}>
          <button className="btn btn-primary" disabled={!schema} onClick={() => schema && onOpenXsd(doc.name.replace(/\.xml$/i, "") + ".xsd", schemaToXsd(schema, guessNs(doc)))}>
            Generate XSD document
          </button>
          <span className="text-[10px]" style={{ color: "var(--fg-muted)" }}>
            DTD/Schema ▸ Generate Schema from instance (Venetian Blind, sampled from ≤ 100k elements)
          </span>
        </div>
      </div>
      <div className="flex-1 overflow-auto p-4">{el && schema ? <Diagram el={el} schema={schema} onSelect={setSel} /> : <div style={{ color: "var(--fg-muted)" }}>Select a component.</div>}</div>
    </div>
  );
}

function guessNs(doc: XmlDocument): string | undefined {
  // cheap: look at the root's xmlns attribute if it was indexed
  void doc;
  return undefined;
}

function Diagram({ el, schema, onSelect }: { el: InferredElement; schema: Map<string, InferredElement>; onSelect: (n: string) => void }) {
  const boxW = 170,
    boxH = 34,
    gapY = 14,
    leftX = 20,
    seqX = leftX + boxW + 40,
    childX = seqX + 80;
  const kids = el.children;
  const totalH = Math.max(boxH + 60, kids.length * (boxH + gapY) + 40);
  const midY = totalH / 2;
  return (
    <div>
      <div className="mb-3 text-[12px] flex flex-wrap gap-4 items-center">
        <span>
          Element <b className="mono">{el.name}</b> · {fmtNum(el.count)} occurrences · {el.parents.size ? `parents: ${Array.from(el.parents).join(", ")}` : "global root"}
        </span>
        {el.hasText && (
          <span>
            content type <span className="mono">{el.textType}</span>
          </span>
        )}
      </div>
      <svg width={childX + boxW + 60} height={totalH} className="select-none">
        {/* element box */}
        <g>
          <rect x={leftX} y={midY - boxH / 2} width={boxW} height={boxH} rx={3} fill="var(--panel-2)" stroke="var(--fg)" strokeWidth={1.2} />
          <text x={leftX + boxW / 2} y={midY + 4} textAnchor="middle" fontSize={12} fontWeight={600} fill="var(--fg)" fontFamily="Consolas, monospace">
            {el.name}
          </text>
        </g>
        {/* attributes */}
        {el.attrs.size > 0 && (
          <g>
            <rect x={leftX} y={midY + boxH / 2 + 8} width={boxW} height={16 + el.attrs.size * 14} rx={2} fill="var(--panel)" stroke="var(--border)" />
            <text x={leftX + 6} y={midY + boxH / 2 + 20} fontSize={10} fill="var(--fg-muted)">
              attributes
            </text>
            {Array.from(el.attrs.entries()).map(([k, a], i) => (
              <text key={k} x={leftX + 10} y={midY + boxH / 2 + 34 + i * 14} fontSize={10.5} fill="var(--tok-attr)" fontFamily="Consolas, monospace">
                = {k} <tspan fill="var(--fg-muted)">{a.type}</tspan>
              </text>
            ))}
          </g>
        )}
        {kids.length > 0 && (
          <>
            {/* connector to sequence compositor */}
            <line x1={leftX + boxW} y1={midY} x2={seqX} y2={midY} stroke="var(--fg)" />
            <g>
              <rect x={seqX} y={midY - 12} width={48} height={24} rx={12} fill="var(--panel-2)" stroke="var(--fg)" />
              <circle cx={seqX + 12} cy={midY} r={2} fill="var(--fg)" />
              <circle cx={seqX + 24} cy={midY} r={2} fill="var(--fg)" />
              <circle cx={seqX + 36} cy={midY} r={2} fill="var(--fg)" />
              <text x={seqX + 24} y={midY + 24} textAnchor="middle" fontSize={9} fill="var(--fg-muted)">
                sequence
              </text>
            </g>
            {kids.map((k, i) => {
              const y = 20 + i * (boxH + gapY) + boxH / 2;
              const ce = schema.get(k.name);
              const optional = k.min === 0;
              const complex = ce && (ce.children.length > 0 || ce.attrs.size > 0);
              const occ = `${k.min}..${k.max > 5 ? "∞" : k.max}`;
              return (
                <g key={k.name} className="cursor-pointer" onClick={() => onSelect(k.name)}>
                  <path d={`M${seqX + 48},${midY} C${seqX + 64},${midY} ${childX - 16},${y} ${childX},${y}`} fill="none" stroke="var(--fg)" strokeDasharray={optional ? "4 3" : undefined} />
                  <rect x={childX} y={y - boxH / 2} width={boxW} height={boxH} rx={3} fill="var(--panel-2)" stroke="var(--fg)" strokeDasharray={optional ? "4 3" : undefined} />
                  <text x={childX + boxW / 2} y={y + 4} textAnchor="middle" fontSize={12} fill="var(--fg)" fontFamily="Consolas, monospace">
                    {k.name}
                  </text>
                  {complex && <text x={childX + boxW - 10} y={y + 4} fontSize={12} fill="var(--fg-muted)">⊞</text>}
                  {(k.min !== 1 || k.max !== 1) && (
                    <text x={childX + boxW / 2} y={y + boxH / 2 + 11} textAnchor="middle" fontSize={9.5} fill="var(--fg-muted)">
                      {occ}
                    </text>
                  )}
                  {ce && !complex && (
                    <text x={childX + boxW + 8} y={y + 4} fontSize={10} fill="var(--fg-muted)" fontFamily="Consolas, monospace">
                      {ce.hasText ? ce.textType : "empty"}
                    </text>
                  )}
                </g>
              );
            })}
          </>
        )}
      </svg>
    </div>
  );
}
