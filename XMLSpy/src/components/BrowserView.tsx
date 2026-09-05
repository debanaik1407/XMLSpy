import { useEffect, useState } from "react";
import type { XmlDocument } from "../engine/document";

const DEFAULT_XSLT = `<?xml version="1.0"?>
<xsl:stylesheet version="1.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
  <xsl:output method="html" indent="yes"/>
  <xsl:template match="/">
    <html><body style="font-family:Segoe UI,system-ui,sans-serif;font-size:13px;margin:16px">
      <xsl:apply-templates select="*"/>
    </body></html>
  </xsl:template>
  <xsl:template match="*">
    <div style="margin:6px 0 6px 14px;border-left:2px solid #cbd5e1;padding-left:8px">
      <span style="color:#7c2d12;font-weight:600"><xsl:value-of select="local-name()"/></span>
      <xsl:for-each select="@*">
        <span style="color:#b45309;margin-left:8px"><xsl:value-of select="name()"/>=<span style="color:#1d4ed8">"<xsl:value-of select="."/>"</span></span>
      </xsl:for-each>
      <xsl:if test="not(*) and normalize-space(.)!=''">
        <span style="margin-left:10px;color:#111827"><xsl:value-of select="."/></span>
      </xsl:if>
      <xsl:apply-templates select="*"/>
    </div>
  </xsl:template>
</xsl:stylesheet>`;

/** Browser View: XSL Transformation (F10) using the browser's native XSLT 1.0 processor. */
export function BrowserView({ doc }: { doc: XmlDocument }) {
  const [html, setHtml] = useState<string>("");
  const [xslt, setXslt] = useState(DEFAULT_XSLT);
  const [err, setErr] = useState<string | null>(null);
  const [showXslt, setShowXslt] = useState(false);
  const [ms, setMs] = useState(0);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      if (doc.large) {
        setErr("Browser View renders in-memory documents (< 16 MiB) with the native XSLT 1.0 processor. Multi-GB documents are transformed by the streaming XSLT 3.0 engine (xmlspy-xslt, Phase 3) with xsl:stream / xsl:iterate.");
        return;
      }
      try {
        const t0 = performance.now();
        const text = await doc.fullText();
        const xml = new DOMParser().parseFromString(text, "application/xml");
        if (xml.querySelector("parsererror")) throw new Error("Document is not well-formed — fix the errors in Text View first (F7).");
        const xsl = new DOMParser().parseFromString(xslt, "application/xml");
        if (xsl.querySelector("parsererror")) throw new Error("Stylesheet is not well-formed.");
        const proc = new XSLTProcessor();
        proc.importStylesheet(xsl);
        const frag = proc.transformToDocument(xml);
        if (cancelled) return;
        setHtml(new XMLSerializer().serializeToString(frag));
        setMs(performance.now() - t0);
        setErr(null);
      } catch (e: any) {
        if (!cancelled) setErr(e.message || String(e));
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [doc, doc.version, xslt]);

  return (
    <div className="flex flex-col h-full" style={{ background: "var(--panel)" }}>
      <div className="flex items-center gap-2 px-2 py-1 text-[11px]" style={{ borderBottom: "1px solid var(--border)", background: "var(--panel-2)" }}>
        <b>XSL Transformation</b> <span style={{ color: "var(--fg-muted)" }}>native XSLT 1.0 · {ms.toFixed(1)} ms</span>
        <button className="btn ml-auto" onClick={() => setShowXslt(!showXslt)}>
          {showXslt ? "Hide" : "Edit"} stylesheet
        </button>
      </div>
      <div className="flex flex-1 min-h-0">
        {showXslt && <textarea className="mono w-[42%] p-2 outline-none resize-none" style={{ background: "var(--panel-2)", color: "var(--fg)", borderRight: "1px solid var(--border)" }} value={xslt} onChange={(e) => setXslt(e.target.value)} spellCheck={false} />}
        {err ? (
          <div className="p-4 text-[12px]" style={{ color: "var(--warn)" }}>
            {err}
          </div>
        ) : (
          <iframe title="Browser view" sandbox="" className="flex-1 border-0 bg-white" srcDoc={html} />
        )}
      </div>
    </div>
  );
}
