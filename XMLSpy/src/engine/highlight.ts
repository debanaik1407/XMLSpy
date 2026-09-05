/**
 * Per-line XML tokenizer for the virtualized Text View. Stateless per line
 * (multi-line comments/CDATA are handled by a light carried flag) so any
 * visible window can be colored without scanning from the file start.
 */
export type Token = { t: "tag" | "name" | "attr" | "val" | "text" | "comment" | "cdata" | "pi" | "ent" | "punct"; s: string };

export function tokenizeLine(line: string, state: { inComment: boolean; inCdata: boolean }): Token[] {
  const out: Token[] = [];
  let i = 0;
  const n = line.length;
  const push = (t: Token["t"], s: string) => {
    if (!s) return;
    const last = out[out.length - 1];
    if (last && last.t === t) last.s += s;
    else out.push({ t, s });
  };
  while (i < n) {
    if (state.inComment) {
      const e = line.indexOf("-->", i);
      if (e < 0) {
        push("comment", line.slice(i));
        return out;
      }
      push("comment", line.slice(i, e + 3));
      i = e + 3;
      state.inComment = false;
      continue;
    }
    if (state.inCdata) {
      const e = line.indexOf("]]>", i);
      if (e < 0) {
        push("cdata", line.slice(i));
        return out;
      }
      push("cdata", line.slice(i, e + 3));
      i = e + 3;
      state.inCdata = false;
      continue;
    }
    const c = line[i];
    if (c === "<") {
      if (line.startsWith("<!--", i)) {
        state.inComment = true;
        continue;
      }
      if (line.startsWith("<![CDATA[", i)) {
        state.inCdata = true;
        continue;
      }
      if (line[i + 1] === "?" || line[i + 1] === "!") {
        const e = line.indexOf(">", i);
        const end = e < 0 ? n : e + 1;
        push("pi", line.slice(i, end));
        i = end;
        continue;
      }
      // tag
      let j = i + 1;
      if (line[j] === "/") j++;
      push("punct", line.slice(i, j));
      let k = j;
      while (k < n && !/[\s/>]/.test(line[k])) k++;
      push("name", line.slice(j, k));
      i = k;
      // attributes until '>'
      while (i < n) {
        const ch = line[i];
        if (ch === ">") {
          push("punct", ">");
          i++;
          break;
        }
        if (ch === "/" && line[i + 1] === ">") {
          push("punct", "/>");
          i += 2;
          break;
        }
        if (/\s/.test(ch)) {
          let w = i;
          while (w < n && /\s/.test(line[w])) w++;
          push("text", line.slice(i, w));
          i = w;
          continue;
        }
        if (ch === "=") {
          push("punct", "=");
          i++;
          continue;
        }
        if (ch === '"' || ch === "'") {
          const e = line.indexOf(ch, i + 1);
          const end = e < 0 ? n : e + 1;
          push("val", line.slice(i, end));
          i = end;
          continue;
        }
        let w = i;
        while (w < n && !/[\s=>/]/.test(line[w])) w++;
        if (w === i) w++;
        push("attr", line.slice(i, w));
        i = w;
      }
      continue;
    }
    if (c === "&") {
      const e = line.indexOf(";", i);
      if (e > 0 && e - i < 12) {
        push("ent", line.slice(i, e + 1));
        i = e + 1;
        continue;
      }
    }
    let w = i;
    while (w < n && line[w] !== "<" && line[w] !== "&") w++;
    if (w === i) w++;
    push("text", line.slice(i, w));
    i = w;
  }
  return out;
}

export const TOKEN_CLASS: Record<Token["t"], string> = {
  tag: "text-[var(--tok-tag)]",
  name: "text-[var(--tok-tag)]",
  attr: "text-[var(--tok-attr)]",
  val: "text-[var(--tok-val)]",
  text: "text-[var(--tok-text)]",
  comment: "text-[var(--tok-comment)] italic",
  cdata: "text-[var(--tok-cdata)]",
  pi: "text-[var(--tok-pi)]",
  ent: "text-[var(--tok-ent)]",
  punct: "text-[var(--tok-punct)]",
};

/** Pretty-print (XML ▸ Pretty-Print, Ctrl+Shift+P) — in-memory documents only. */
export function prettyPrint(xml: string, indent = "  "): string {
  const tokens = xml.replace(/>\s+</g, "><").trim().split(/(<[^>]+>)/g).filter((t) => t.length);
  let depth = 0;
  const out: string[] = [];
  let pendingText = "";
  for (let i = 0; i < tokens.length; i++) {
    const t = tokens[i];
    if (t.startsWith("<")) {
      if (t.startsWith("</")) {
        depth = Math.max(0, depth - 1);
        if (pendingText !== "") {
          out[out.length - 1] += pendingText + t;
          pendingText = "";
        } else out.push(indent.repeat(depth) + t);
      } else if (t.startsWith("<?") || t.startsWith("<!--") || t.startsWith("<!") || t.endsWith("/>")) {
        out.push(indent.repeat(depth) + t);
      } else {
        out.push(indent.repeat(depth) + t);
        const next = tokens[i + 1];
        if (next && !next.startsWith("<") && tokens[i + 2] && tokens[i + 2].startsWith("</")) {
          pendingText = next.trim();
          i++;
        }
        depth++;
      }
    } else {
      const tt = t.trim();
      if (tt) out.push(indent.repeat(depth) + tt);
    }
  }
  return out.join("\n") + "\n";
}
