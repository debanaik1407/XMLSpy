/**
 * Streaming, chunk-resumable XML well-formedness scanner + structural indexer.
 *
 * This function is intentionally SELF-CONTAINED (no imports, no closures over
 * module scope, no classes) because it is serialized with `Function.toString()`
 * and executed inside a Blob-URL Web Worker. It mirrors the Rust
 * `xmlspy-parse::Scanner` state machine described in the Architecture view.
 *
 * Single pass. Never materializes a DOM. Memory is O(index), not O(file).
 */
export type WfError = {
  offset: number;
  line: number;
  col: number;
  msg: string;
  severity: "error" | "warning";
  fix?: string;
};

export type ScannerConfig = {
  maxIndexed: number; // hard cap on fully indexed elements (browser memory budget)
  stride: number; // line checkpoint stride (every Nth line start offset is stored)
  maxErrors: number;
};

export function createScanner(cfg: ScannerConfig) {
  // ---- states -------------------------------------------------------------
  var S_TEXT = 0,
    S_LT = 1,
    S_STARTNAME = 2,
    S_INTAG = 3,
    S_ATTRNAME = 4,
    S_ATTREQ = 5,
    S_ATTRPREQ = 6,
    S_ATTRVAL = 7,
    S_EMPTY = 8,
    S_ENDNAME = 9,
    S_ENDTRAIL = 10,
    S_PI = 11,
    S_PIQ = 12,
    S_BANG = 13,
    S_COMMENT0 = 14,
    S_COMMENT = 15,
    S_COMMENTD1 = 16,
    S_COMMENTD2 = 17,
    S_CDATA0 = 18,
    S_CDATA = 19,
    S_CDATAB1 = 20,
    S_CDATAB2 = 21,
    S_DOCTYPE = 22,
    S_REF = 23,
    S_BOM = 24,
    S_BOM1 = 25,
    S_BOM2 = 26;

  var state = S_BOM;
  var refReturn = S_TEXT;
  var refBuf = "";
  var refStart = 0;
  var quote = 0;
  var line = 1;
  var lineStart = 0;
  var cdataMatch = 0;
  var doctypeBracket = 0;
  var textBracket = 0; // for ']]>' detection in content
  var sawNonWsOutsideRoot = false;
  var rootClosed = false;
  var rootSeen = false;
  var totalElements = 0;
  var totalAttributes = 0;
  var maxDepth = 0;
  var errorCount = 0;
  var errors: WfError[] = [];
  var tagStartOff = 0;

  // name accumulation
  var nameBytes = new Uint8Array(256);
  var nameLen = 0;
  var nameAscii = true;
  var curNameId = -1;
  var attrNames: string[] = [];
  var attrBytesStart = 0;

  // name interning
  var nameMap: Record<string, number> = Object.create(null);
  var names: string[] = [];
  var decoder = new TextDecoder("utf-8");

  // element stack
  var stackName: number[] = [];
  var stackIdx: number[] = [];
  var depth = 0;

  // ---- growable typed arrays ---------------------------------------------
  function growF64(a: Float64Array<ArrayBufferLike>): Float64Array<ArrayBuffer> {
    var b = new Float64Array(Math.max(1024, a.length * 2));
    b.set(a);
    return b;
  }
  function growI32(a: Int32Array<ArrayBufferLike>): Int32Array<ArrayBuffer> {
    var b = new Int32Array(Math.max(1024, a.length * 2));
    b.set(a);
    return b;
  }
  function growU16(a: Uint16Array<ArrayBufferLike>): Uint16Array<ArrayBuffer> {
    var b = new Uint16Array(Math.max(1024, a.length * 2));
    b.set(a);
    return b;
  }

  var checkpoints = new Float64Array(1024);
  var cpCount = 0;
  checkpoints[cpCount++] = 0;

  var elemStart = new Float64Array(1024);
  var elemEnd = new Float64Array(1024);
  var elemLine = new Float64Array(1024);
  var elemParent = new Int32Array(1024);
  var elemName = new Int32Array(1024);
  var elemDepth = new Uint16Array(1024);
  var elemCount = 0;

  function pushError(off: number, msg: string, fix?: string, warn?: boolean) {
    errorCount++;
    if (errors.length < cfg.maxErrors) {
      errors.push({
        offset: off,
        line: line,
        col: off - lineStart + 1,
        msg: msg,
        severity: warn ? "warning" : "error",
        fix: fix,
      });
    }
  }

  function resetName() {
    nameLen = 0;
    nameAscii = true;
  }
  function pushNameByte(b: number) {
    if (nameLen === nameBytes.length) {
      var nb = new Uint8Array(nameBytes.length * 2);
      nb.set(nameBytes);
      nameBytes = nb;
    }
    if (b >= 128) nameAscii = false;
    nameBytes[nameLen++] = b;
  }
  function takeName(): string {
    var s: string;
    if (nameAscii) {
      s = "";
      // fast path
      for (var i = 0; i < nameLen; i++) s += String.fromCharCode(nameBytes[i]);
    } else {
      s = decoder.decode(nameBytes.subarray(0, nameLen));
    }
    return s;
  }
  function internName(s: string): number {
    var id = nameMap[s];
    if (id === undefined) {
      id = names.length;
      names.push(s);
      nameMap[s] = id;
    }
    return id;
  }
  function isNameStart(b: number) {
    return (
      (b >= 65 && b <= 90) ||
      (b >= 97 && b <= 122) ||
      b === 95 ||
      b === 58 ||
      b >= 128
    );
  }
  function isNameChar(b: number) {
    return (
      isNameStart(b) || (b >= 48 && b <= 57) || b === 45 || b === 46 || b === 0xb7
    );
  }
  function isWs(b: number) {
    return b === 32 || b === 10 || b === 13 || b === 9;
  }

  function openElement(off: number) {
    var name = takeName();
    curNameId = internName(name);
    totalElements++;
    if (rootClosed && depth === 0) {
      pushError(
        tagStartOff,
        "Document contains more than one root element (<" + name + ">). XML 1.0 §2.1 [1] document ::= prolog element Misc*",
        "Wrap both elements in a single root element"
      );
    }
    rootSeen = true;
    var idx = -1;
    var index = elemCount < cfg.maxIndexed || depth <= 1;
    if (index && elemCount < cfg.maxIndexed * 2) {
      if (elemCount === elemStart.length) {
        elemStart = growF64(elemStart);
        elemEnd = growF64(elemEnd);
        elemLine = growF64(elemLine);
        elemParent = growI32(elemParent);
        elemName = growI32(elemName);
        elemDepth = growU16(elemDepth);
      }
      idx = elemCount++;
      elemStart[idx] = tagStartOff;
      elemEnd[idx] = -1;
      elemLine[idx] = line;
      elemParent[idx] = depth > 0 ? stackIdx[depth - 1] : -1;
      elemName[idx] = curNameId;
      elemDepth[idx] = depth;
    }
    stackName[depth] = curNameId;
    stackIdx[depth] = idx;
    depth++;
    if (depth > maxDepth) maxDepth = depth;
    attrNames.length = 0;
    void off;
  }

  function closeElement(off: number, endOff: number) {
    if (depth === 0) return;
    depth--;
    var idx = stackIdx[depth];
    if (idx >= 0) elemEnd[idx] = endOff;
    if (depth === 0) rootClosed = true;
    void off;
  }

  function validateRef(off: number) {
    var ok = false;
    if (refBuf.length > 0) {
      if (refBuf.charCodeAt(0) === 35) {
        ok = /^#(x[0-9a-fA-F]+|[0-9]+)$/.test(refBuf);
      } else {
        ok = /^[A-Za-z_:][A-Za-z0-9_:.\-]*$/.test(refBuf);
      }
    }
    if (!ok)
      pushError(
        refStart,
        "Malformed entity/character reference '&" + refBuf + ";'. XML 1.0 §4.1 [67] Reference",
        "Escape the ampersand as &amp;"
      );
    void off;
  }

  // ---- main feed ---------------------------------------------------------
  function feed(buf: Uint8Array, base: number) {
    var n = buf.length;
    var i = 0;
    var b = 0;
    var off = 0;
    var st = state;
    var reproc = false; // true when the current byte is being re-dispatched in a new state

    for (i = 0; i < n; i++) {
      b = buf[i];
      off = base + i;
      if (b === 10 && !reproc) {
        line++;
        lineStart = off + 1;
        if ((line - 1) % cfg.stride === 0) {
          if (cpCount === checkpoints.length) checkpoints = growF64(checkpoints);
          checkpoints[cpCount++] = lineStart;
        }
      }
      reproc = false;

      switch (st) {
        // A UTF-8 BOM may be split across chunk boundaries, so it is matched one byte
        // at a time rather than by peeking ahead into this buffer (matches the Rust
        // engine's St::Bom / Bom1 / Bom2).
        case S_BOM:
          if (b === 0xef) {
            st = S_BOM1;
          } else {
            st = S_TEXT;
            i--; // reprocess in S_TEXT
            reproc = true;
          }
          break;
        case S_BOM1:
          st = b === 0xbb ? S_BOM2 : S_TEXT;
          break;
        case S_BOM2:
          st = S_TEXT;
          if (b !== 0xbf) {
            i--;
            reproc = true;
          }
          break;
        case S_TEXT:
          if (b === 60) {
            st = S_LT;
            tagStartOff = off;
            textBracket = 0;
          } else if (b === 38) {
            refReturn = S_TEXT;
            refBuf = "";
            refStart = off;
            st = S_REF;
          } else if (b === 93) {
            textBracket++;
          } else if (b === 62 && textBracket >= 2) {
            pushError(off - 2, "The sequence ']]>' is not allowed in element content. XML 1.0 §2.4 [14] CharData", "Escape as ]]&gt;");
            textBracket = 0;
          } else {
            textBracket = 0;
            if (depth === 0 && !isWs(b) && !sawNonWsOutsideRoot) {
              sawNonWsOutsideRoot = true;
              pushError(
                off,
                rootSeen
                  ? "Non-whitespace character data after the root element. XML 1.0 §2.1 [27] Misc"
                  : "Non-whitespace character data before the root element (or missing root). XML 1.0 §2.1",
                "Remove the stray text or move it inside the root element"
              );
            }
          }
          break;

        case S_REF:
          if (b === 59) {
            validateRef(off);
            st = refReturn;
          } else if (isNameChar(b) || b === 35) {
            if (refBuf.length < 32) refBuf += String.fromCharCode(b);
            else {
              pushError(refStart, "Unterminated entity reference. XML 1.0 §4.1", "Escape the ampersand as &amp;");
              st = refReturn;
            }
          } else {
            pushError(refStart, "Unescaped '&' (entity reference must be terminated by ';'). XML 1.0 §4.1 [68] EntityRef", "Replace '&' with '&amp;'");
            st = refReturn;
            i--; // reprocess this byte in the previous state
            reproc = true;
          }
          break;

        case S_LT:
          if (b === 47) {
            st = S_ENDNAME;
            resetName();
          } else if (b === 63) {
            st = S_PI;
          } else if (b === 33) {
            st = S_BANG;
          } else if (isNameStart(b)) {
            st = S_STARTNAME;
            resetName();
            pushNameByte(b);
          } else {
            pushError(off, "'<' must be followed by an element name, '/', '?' or '!'. XML 1.0 §3.1 [40] STag", "Escape the '<' as &lt;");
            st = S_TEXT;
          }
          break;

        case S_STARTNAME:
          if (isNameChar(b)) pushNameByte(b);
          else if (isWs(b)) {
            openElement(off);
            st = S_INTAG;
          } else if (b === 62) {
            openElement(off);
            st = S_TEXT;
          } else if (b === 47) {
            openElement(off);
            st = S_EMPTY;
          } else {
            pushError(off, "Invalid character in element name. XML 1.0 §2.3 [5] Name");
            openElement(off);
            st = S_INTAG;
          }
          break;

        case S_INTAG:
          if (isWs(b)) break;
          if (b === 62) st = S_TEXT;
          else if (b === 47) st = S_EMPTY;
          else if (isNameStart(b)) {
            resetName();
            pushNameByte(b);
            attrBytesStart = off;
            st = S_ATTRNAME;
          } else {
            pushError(off, "Unexpected character in start tag; expected attribute name, '/' or '>'. XML 1.0 §3.1 [40]", "Quote attribute values and separate attributes with whitespace");
          }
          break;

        case S_ATTRNAME:
          if (isNameChar(b)) pushNameByte(b);
          else {
            var an = takeName();
            totalAttributes++;
            for (var k = 0; k < attrNames.length; k++) {
              if (attrNames[k] === an) {
                pushError(attrBytesStart, "Attribute '" + an + "' specified more than once. XML 1.0 §3.1 [WFC: Unique Att Spec]", "Remove the duplicate attribute");
                break;
              }
            }
            if (attrNames.length < 64) attrNames.push(an);
            if (b === 61) st = S_ATTRPREQ;
            else if (isWs(b)) st = S_ATTREQ;
            else {
              pushError(off, "Attribute '" + an + "' must be followed by '='. XML 1.0 §3.1 [41] Attribute", 'Insert =""');
              st = S_INTAG;
              i--;
              reproc = true;
            }
          }
          break;

        case S_ATTREQ:
          if (isWs(b)) break;
          if (b === 61) st = S_ATTRPREQ;
          else {
            pushError(off, "Expected '=' after attribute name. XML 1.0 §3.1 [41]", 'Insert =""');
            st = S_INTAG;
            i--;
            reproc = true;
          }
          break;

        case S_ATTRPREQ:
          if (isWs(b)) break;
          if (b === 34 || b === 39) {
            quote = b;
            st = S_ATTRVAL;
          } else {
            pushError(off, "Attribute value must be quoted. XML 1.0 §3.1 [10] AttValue", "Enclose the value in double quotes");
            st = S_INTAG;
          }
          break;

        case S_ATTRVAL:
          if (b === quote) st = S_INTAG;
          else if (b === 60) pushError(off, "'<' is not allowed in attribute values. XML 1.0 §3.1 [WFC: No < in Attribute Values]", "Escape as &lt;");
          else if (b === 38) {
            refReturn = S_ATTRVAL;
            refBuf = "";
            refStart = off;
            st = S_REF;
          }
          break;

        case S_EMPTY:
          if (b === 62) {
            closeElement(off, off + 1);
            st = S_TEXT;
          } else {
            pushError(off, "Expected '>' after '/' in empty-element tag. XML 1.0 §3.1 [44] EmptyElemTag");
            st = S_INTAG;
          }
          break;

        case S_ENDNAME:
          if (isNameChar(b)) pushNameByte(b);
          else if (nameLen === 0) {
            pushError(off, "End tag must start with a name (no whitespace after '</'). XML 1.0 §3.1 [42] ETag");
            st = S_ENDTRAIL;
          } else {
            var en = takeName();
            var eid = internName(en);
            if (depth === 0) {
              pushError(tagStartOff, "Unexpected end tag </" + en + "> — no element is open. XML 1.0 §3.1 [WFC: Element Type Match]", "Delete this end tag");
            } else if (stackName[depth - 1] !== eid) {
              var expected = names[stackName[depth - 1]];
              // recovery: pop until match if present in stack
              var found = -1;
              for (var d = depth - 1; d >= 0; d--) if (stackName[d] === eid) { found = d; break; }
              var fixText = "Change to </" + expected + ">";
              if (found >= 0) {
                fixText = "Insert ";
                for (var d2 = depth - 1; d2 > found; d2--) fixText += "</" + names[stackName[d2]] + ">";
              }
              pushError(tagStartOff, "End tag </" + en + "> does not match start tag <" + expected + ">. XML 1.0 §3.1 [WFC: Element Type Match]", fixText);
              if (found >= 0) while (depth > found) closeElement(off, off + 1);
              else closeElement(off, off + 1);
            } else {
              closeElement(off, -2); // patched when '>' seen
            }
            st = b === 62 ? S_TEXT : S_ENDTRAIL;
            if (b === 62 && stackIdx[depth] >= 0 && elemEnd[stackIdx[depth]] === -2) elemEnd[stackIdx[depth]] = off + 1;
          }
          break;

        case S_ENDTRAIL:
          if (b === 62) {
            if (stackIdx[depth] >= 0 && elemEnd[stackIdx[depth]] === -2) elemEnd[stackIdx[depth]] = off + 1;
            st = S_TEXT;
          } else if (!isWs(b)) {
            pushError(off, "Unexpected character in end tag. XML 1.0 §3.1 [42] ETag");
          }
          break;

        case S_PI:
          if (b === 63) st = S_PIQ;
          break;
        case S_PIQ:
          if (b === 62) st = S_TEXT;
          else if (b !== 63) st = S_PI;
          break;

        case S_BANG:
          if (b === 45) {
            st = S_COMMENT0;
          } else if (b === 91) {
            st = S_CDATA0;
            cdataMatch = 0;
          } else if (b === 68 || b === 100) {
            st = S_DOCTYPE;
            doctypeBracket = 0;
          } else {
            pushError(off, "Unknown markup declaration after '<!'. XML 1.0 §2.8 [29] markupdecl");
            st = S_TEXT;
          }
          break;
        case S_COMMENT0:
          if (b === 45) st = S_COMMENT;
          else {
            pushError(off, "Comment must start with '<!--'. XML 1.0 §2.5 [15] Comment");
            st = S_TEXT;
          }
          break;
        case S_COMMENT:
          if (b === 45) st = S_COMMENTD1;
          break;
        case S_COMMENTD1:
          st = b === 45 ? S_COMMENTD2 : S_COMMENT;
          break;
        case S_COMMENTD2:
          if (b === 62) st = S_TEXT;
          else {
            pushError(off - 2, "'--' is not allowed inside comments. XML 1.0 §2.5 [15]", "Replace '--' with '- -'");
            st = b === 45 ? S_COMMENTD2 : S_COMMENT;
          }
          break;
        case S_CDATA0:
          // expecting "CDATA["
          var expect = "CDATA[".charCodeAt(cdataMatch);
          if (b === expect) {
            cdataMatch++;
            if (cdataMatch === 6) st = S_CDATA;
          } else {
            pushError(off, "Malformed CDATA section start; expected '<![CDATA['. XML 1.0 §2.7 [19] CDStart");
            st = S_TEXT;
          }
          break;
        case S_CDATA:
          if (b === 93) st = S_CDATAB1;
          break;
        case S_CDATAB1:
          st = b === 93 ? S_CDATAB2 : S_CDATA;
          break;
        case S_CDATAB2:
          if (b === 62) st = S_TEXT;
          else if (b !== 93) st = S_CDATA;
          break;
        case S_DOCTYPE:
          if (b === 91) doctypeBracket++;
          else if (b === 93) doctypeBracket--;
          else if (b === 62 && doctypeBracket <= 0) st = S_TEXT;
          break;
      }
    }
    state = st;
  }

  function finish(totalBytes: number) {
    if (!rootSeen) {
      pushError(0, "Document is empty or has no root element. XML 1.0 §2.1 [1]", "Add a root element");
    }
    if (state !== S_TEXT && state !== S_BOM && state !== S_BOM1 && state !== S_BOM2) {
      pushError(totalBytes, "Unexpected end of file inside markup (unterminated tag, comment, CDATA or PI).");
    }
    if (depth > 0) {
      var open: string[] = [];
      for (var d = depth - 1; d >= 0 && open.length < 5; d--) open.push("<" + names[stackName[d]] + ">");
      pushError(
        totalBytes,
        "Unexpected end of file: " + depth + " element(s) not closed: " + open.join(", ") + (depth > 5 ? ", …" : ""),
        "Append " + open.map(function (s) { return s.replace("<", "</"); }).join("")
      );
      while (depth > 0) closeElement(totalBytes, totalBytes);
    }
  }

  function result() {
    return {
      checkpoints: checkpoints.slice(0, cpCount),
      stride: cfg.stride,
      lineCount: line,
      elemStart: elemStart.slice(0, elemCount),
      elemEnd: elemEnd.slice(0, elemCount),
      elemLine: elemLine.slice(0, elemCount),
      elemParent: elemParent.slice(0, elemCount),
      elemName: elemName.slice(0, elemCount),
      elemDepth: elemDepth.slice(0, elemCount),
      names: names,
      indexedElements: elemCount,
      totalElements: totalElements,
      totalAttributes: totalAttributes,
      maxDepth: maxDepth,
      errors: errors,
      errorCount: errorCount,
    };
  }

  function progress() {
    return { line: line, elements: totalElements, errors: errorCount };
  }

  return { feed: feed, finish: finish, result: result, progress: progress };
}

export type ScanIndex = ReturnType<ReturnType<typeof createScanner>["result"]>;
