import { useMemo, useState } from "react";
import { ARCHITECTURE_MD } from "../docs/architecture";
import { ROADMAP_MD } from "../docs/roadmap";
import { RUST_MD } from "../docs/rustCode";

/** Minimal markdown renderer (headings, fenced code with ~~~, lists, tables, bold, inline code). */
function renderMd(md: string): React.ReactNode[] {
  const lines = md.split("\n");
  const out: React.ReactNode[] = [];
  let i = 0;
  let key = 0;
  const inline = (s: string): React.ReactNode[] => {
    const parts: React.ReactNode[] = [];
    const re = /(\*\*[^*]+\*\*|`[^`]+`)/g;
    let last = 0;
    let m: RegExpExecArray | null;
    while ((m = re.exec(s))) {
      if (m.index > last) parts.push(s.slice(last, m.index));
      const t = m[0];
      if (t.startsWith("**")) parts.push(<b key={key++}>{t.slice(2, -2)}</b>);
      else parts.push(<code key={key++}>{t.slice(1, -1)}</code>);
      last = m.index + t.length;
    }
    if (last < s.length) parts.push(s.slice(last));
    return parts;
  };
  while (i < lines.length) {
    const l = lines[i];
    if (l.startsWith("~~~")) {
      const lang = l.slice(3).trim();
      const buf: string[] = [];
      i++;
      while (i < lines.length && !lines[i].startsWith("~~~")) buf.push(lines[i++]);
      i++;
      out.push(
        <pre key={key++} data-lang={lang}>
          <code>{buf.join("\n")}</code>
        </pre>
      );
      continue;
    }
    const h = /^(#{1,3})\s+(.*)$/.exec(l);
    if (h) {
      const T = (`h${h[1].length}`) as "h1" | "h2" | "h3";
      out.push(<T key={key++} id={h[2].toLowerCase().replace(/[^a-z0-9]+/g, "-")}>{inline(h[2])}</T>);
      i++;
      continue;
    }
    if (l.startsWith("|")) {
      const rows: string[][] = [];
      while (i < lines.length && lines[i].startsWith("|")) {
        const cells = lines[i].slice(1, lines[i].endsWith("|") ? -1 : undefined).split("|").map((c) => c.trim());
        if (!cells.every((c) => /^-+$/.test(c))) rows.push(cells);
        i++;
      }
      out.push(
        <table key={key++}>
          <thead>
            <tr>
              {rows[0].map((c, k) => (
                <th key={k}>{inline(c)}</th>
              ))}
            </tr>
          </thead>
          <tbody>
            {rows.slice(1).map((r, ri) => (
              <tr key={ri}>
                {r.map((c, k) => (
                  <td key={k}>{inline(c)}</td>
                ))}
              </tr>
            ))}
          </tbody>
        </table>
      );
      continue;
    }
    if (/^\s*[-*]\s+/.test(l) || /^\s*\d+\.\s+/.test(l)) {
      const ordered = /^\s*\d+\.\s+/.test(l);
      const items: string[] = [];
      while (i < lines.length && (/^\s*[-*]\s+/.test(lines[i]) || /^\s*\d+\.\s+/.test(lines[i]))) {
        items.push(lines[i].replace(/^\s*([-*]|\d+\.)\s+/, ""));
        i++;
      }
      const L = ordered ? "ol" : "ul";
      out.push(<L key={key++}>{items.map((it, k) => <li key={k}>{inline(it)}</li>)}</L>);
      continue;
    }
    if (l.trim() === "") {
      i++;
      continue;
    }
    const para: string[] = [l];
    i++;
    while (i < lines.length && lines[i].trim() !== "" && !/^(#|~~~|\||\s*[-*]\s|\s*\d+\.\s)/.test(lines[i])) para.push(lines[i++]);
    out.push(<p key={key++}>{inline(para.join(" "))}</p>);
  }
  return out;
}

export function DocsView() {
  const [tab, setTab] = useState<"arch" | "roadmap" | "rust">("arch");
  const src = tab === "arch" ? ARCHITECTURE_MD : tab === "roadmap" ? ROADMAP_MD : RUST_MD;
  const nodes = useMemo(() => renderMd(src), [src]);
  const toc = useMemo(() => src.split("\n").filter((l) => /^#{1,2}\s/.test(l)).map((l) => ({ level: l.startsWith("##") ? 2 : 1, text: l.replace(/^#+\s+/, "") })), [src]);
  return (
    <div className="flex h-full" style={{ background: "var(--panel)" }}>
      <div className="w-64 shrink-0 flex flex-col" style={{ borderRight: "1px solid var(--border)" }}>
        <div className="flex" style={{ borderBottom: "1px solid var(--border)" }}>
          {(
            [
              ["arch", "Architecture"],
              ["roadmap", "Roadmap"],
              ["rust", "Rust Source"],
            ] as const
          ).map(([k, l]) => (
            <button key={k} className="flex-1 py-1 text-[11px]" style={{ background: tab === k ? "var(--panel)" : "var(--panel-2)", fontWeight: tab === k ? 600 : 400, borderBottom: tab === k ? "2px solid var(--accent)" : "2px solid transparent" }} onClick={() => setTab(k)}>
              {l}
            </button>
          ))}
        </div>
        <div className="flex-1 overflow-auto py-1 text-[11px]">
          {toc.map((t, k) => (
            <a key={k} href={"#" + t.text.toLowerCase().replace(/[^a-z0-9]+/g, "-")} className="block px-2 py-[2px] truncate hover:bg-[var(--accent-soft)]" style={{ paddingLeft: t.level === 2 ? 18 : 8, color: t.level === 1 ? "var(--fg)" : "var(--fg-muted)" }}>
              {t.text}
            </a>
          ))}
        </div>
      </div>
      <div className="md flex-1 overflow-auto px-6 pb-10">{nodes}</div>
    </div>
  );
}
