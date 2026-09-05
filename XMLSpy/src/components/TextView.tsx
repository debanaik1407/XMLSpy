import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import type { XmlDocument } from "../engine/document";
import type { WfError } from "../engine/scanner";
import { TOKEN_CLASS, tokenizeLine } from "../engine/highlight";

const ROW = 20;
const OVERSCAN = 12;
const MAX_SCROLL_PX = 6_000_000; // browsers clamp element heights (~33M px); we scale beyond this.

export type GotoRequest = { line: number; col?: number; nonce: number } | null;

export function TextView({
  doc,
  goto,
  errors,
  onCursor,
  onEdit,
  version,
}: {
  doc: XmlDocument;
  goto: GotoRequest;
  errors: WfError[];
  onCursor: (line: number, col: number) => void;
  onEdit: (line: number, text: string) => void;
  version: number;
}) {
  const containerRef = useRef<HTMLDivElement>(null);
  const [viewportH, setViewportH] = useState(600);
  const [first, setFirst] = useState(0); // first visible line (0-based)
  const [lines, setLines] = useState<{ start: number; items: string[] }>({ start: 0, items: [] });
  const [cursorLine, setCursorLine] = useState(0);
  const [editing, setEditing] = useState<{ line: number; text: string } | null>(null);
  const programmatic = useRef(false);
  const fetchSeq = useRef(0);

  const lineCount = doc.lineCount;
  const totalPx = lineCount * ROW;
  const scaled = totalPx > MAX_SCROLL_PX;
  const scrollH = scaled ? MAX_SCROLL_PX : totalPx;
  const scale = scaled ? totalPx / MAX_SCROLL_PX : 1;
  const rows = Math.ceil(viewportH / ROW) + 1;

  const errorByLine = useMemo(() => {
    const m = new Map<number, WfError[]>();
    for (const e of errors) {
      const arr = m.get(e.line - 1) ?? [];
      arr.push(e);
      m.set(e.line - 1, arr);
    }
    return m;
  }, [errors]);

  useLayoutEffect(() => {
    const el = containerRef.current;
    if (!el) return;
    const ro = new ResizeObserver(() => setViewportH(el.clientHeight));
    ro.observe(el);
    setViewportH(el.clientHeight);
    return () => ro.disconnect();
  }, []);

  // Fetch the visible window (+ overscan). Only these lines exist in the DOM.
  useEffect(() => {
    const start = Math.max(0, first - OVERSCAN);
    const end = Math.min(lineCount, first + rows + OVERSCAN);
    const seq = ++fetchSeq.current;
    let cancelled = false;
    doc.getLines(start, end).then((items) => {
      if (cancelled || seq !== fetchSeq.current) return;
      setLines({ start, items });
    });
    return () => {
      cancelled = true;
    };
  }, [doc, first, rows, lineCount, version]);

  const scrollToLine = useCallback(
    (line: number, center = true) => {
      const el = containerRef.current;
      if (!el) return;
      const target = center ? Math.max(0, line - Math.floor(rows / 2)) : line;
      const clamped = Math.min(Math.max(0, lineCount - rows + 1), target);
      programmatic.current = true;
      el.scrollTop = (clamped * ROW) / scale;
      setFirst(clamped);
      requestAnimationFrame(() => (programmatic.current = false));
    },
    [rows, lineCount, scale]
  );

  useEffect(() => {
    if (!goto) return;
    const l = Math.max(0, Math.min(lineCount - 1, goto.line - 1));
    setCursorLine(l);
    onCursor(l + 1, goto.col ?? 1);
    scrollToLine(l);
    containerRef.current?.focus();
  }, [goto]); // eslint-disable-line react-hooks/exhaustive-deps

  const onScroll = () => {
    if (programmatic.current) return;
    const el = containerRef.current!;
    const f = Math.floor((el.scrollTop * scale) / ROW);
    setFirst(Math.max(0, Math.min(lineCount - 1, f)));
  };

  const onWheel = (e: React.WheelEvent) => {
    if (!scaled) return;
    e.preventDefault();
    const delta = Math.sign(e.deltaY) * Math.max(1, Math.round(Math.abs(e.deltaY) / 33)) * 3;
    scrollToLine(Math.max(0, Math.min(lineCount - 1, first + delta)), false);
  };

  const ensureVisible = (l: number) => {
    if (l < first) scrollToLine(l, false);
    else if (l >= first + rows - 1) scrollToLine(l - rows + 2, false);
  };

  const beginEdit = async (l: number) => {
    const [t] = await doc.getLines(l, l + 1);
    setEditing({ line: l, text: t ?? "" });
  };

  const onKeyDown = (e: React.KeyboardEvent) => {
    if (editing) return;
    let l = cursorLine;
    switch (e.key) {
      case "ArrowDown":
        l = Math.min(lineCount - 1, l + 1);
        break;
      case "ArrowUp":
        l = Math.max(0, l - 1);
        break;
      case "PageDown":
        l = Math.min(lineCount - 1, l + rows - 2);
        break;
      case "PageUp":
        l = Math.max(0, l - rows + 2);
        break;
      case "Home":
        if (e.ctrlKey) l = 0;
        break;
      case "End":
        if (e.ctrlKey) l = lineCount - 1;
        break;
      case "Enter":
      case "F2":
        e.preventDefault();
        beginEdit(l);
        return;
      default:
        return;
    }
    e.preventDefault();
    setCursorLine(l);
    onCursor(l + 1, 1);
    ensureVisible(l);
  };

  const commitEdit = () => {
    if (!editing) return;
    onEdit(editing.line, editing.text);
    setEditing(null);
    containerRef.current?.focus();
  };

  // Rows to render
  const rendered: React.ReactNode[] = [];
  const hlState = { inComment: false, inCdata: false };
  const gutterW = Math.max(4, String(lineCount).length) * 8 + 16;
  for (let i = 0; i < lines.items.length; i++) {
    const n = lines.start + i;
    if (n < first - OVERSCAN || n > first + rows + OVERSCAN) continue;
    const text = lines.items[i];
    const errs = errorByLine.get(n);
    const isCursor = n === cursorLine;
    const top = scaled ? (n - first) * ROW + (first * ROW) / scale : n * ROW;
    rendered.push(
      <div
        key={n}
        className={`absolute left-0 right-0 flex items-stretch mono ${errs ? "row-err" : ""} ${isCursor ? "row-sel" : ""}`}
        style={{ top, height: ROW, lineHeight: `${ROW}px` }}
        role="row"
        aria-rowindex={n + 1}
        onMouseDown={() => {
          setCursorLine(n);
          onCursor(n + 1, 1);
        }}
        onDoubleClick={() => beginEdit(n)}
        title={errs ? errs.map((e) => e.msg).join("\n") : undefined}
      >
        <span className="shrink-0 text-right pr-2 select-none" style={{ width: gutterW, background: "var(--gutter)", color: errs ? "var(--err)" : "var(--gutter-fg)", borderRight: "1px solid var(--border)" }}>
          {errs ? "●" : ""}
          {n + 1}
        </span>
        {editing && editing.line === n ? (
          <input
            autoFocus
            className="mono flex-1 px-2 outline-none"
            style={{ background: "var(--panel)", color: "var(--fg)", border: "1px solid var(--accent)", height: ROW - 2 }}
            value={editing.text}
            onChange={(e) => setEditing({ line: n, text: e.target.value })}
            onKeyDown={(e) => {
              if (e.key === "Enter") commitEdit();
              else if (e.key === "Escape") {
                setEditing(null);
                containerRef.current?.focus();
              }
              e.stopPropagation();
            }}
            onBlur={commitEdit}
          />
        ) : (
          <span className="whitespace-pre pl-2 overflow-hidden">
            {tokenizeLine(text, hlState).map((t, k) => (
              <span key={k} className={TOKEN_CLASS[t.t]}>
                {t.s}
              </span>
            ))}
          </span>
        )}
      </div>
    );
  }

  // Minimap: viewport indicator + error marks (positions are proportional to line number).
  const minimap = (
    <div className="relative w-[14px] shrink-0 select-none" style={{ background: "var(--panel-2)", borderLeft: "1px solid var(--border)" }} title="Minimap: viewport & error markers">
      {errors.slice(0, 300).map((e, i) => (
        <div key={i} className="absolute left-[2px] right-[2px] h-[2px]" style={{ top: `${((e.line - 1) / Math.max(1, lineCount)) * 100}%`, background: "var(--err)" }} />
      ))}
      <div className="absolute left-0 right-0" style={{ top: `${(first / Math.max(1, lineCount)) * 100}%`, height: `${Math.max(1, (rows / Math.max(1, lineCount)) * 100)}%`, background: "var(--accent)", opacity: 0.35 }} />
    </div>
  );

  return (
    <div className="flex h-full w-full" style={{ background: "var(--panel)" }}>
      <div
        ref={containerRef}
        tabIndex={0}
        role="grid"
        aria-rowcount={lineCount}
        className="relative flex-1 overflow-auto outline-none"
        onScroll={onScroll}
        onWheel={onWheel}
        onKeyDown={onKeyDown}
      >
        <div style={{ height: scrollH, position: "relative" }}>{rendered}</div>
      </div>
      {minimap}
    </div>
  );
}
