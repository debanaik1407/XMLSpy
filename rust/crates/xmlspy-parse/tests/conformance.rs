//! Well-formedness conformance tests: every diagnostic the SmartFix engine can act on,
//! plus the constructs a scanner must skip without tripping (PI, comment, CDATA, DOCTYPE
//! internal subset, UTF-8 names, BOM).

use xmlspy_parse::{Scanner, ScannerConfig, StructuralIndex};

fn scan(src: &str) -> StructuralIndex {
    Scanner::scan_all(ScannerConfig::default(), src.as_bytes())
}

fn msgs(ix: &StructuralIndex) -> Vec<String> {
    ix.errors.iter().map(|e| e.msg.clone()).collect()
}

fn assert_wf(src: &str) {
    let ix = scan(src);
    assert_eq!(
        ix.error_count,
        0,
        "expected well-formed, got {:?}",
        msgs(&ix)
    );
}

fn assert_err(src: &str, needle: &str) -> StructuralIndex {
    let ix = scan(src);
    assert!(
        ix.errors.iter().any(|e| e.msg.contains(needle)),
        "expected an error containing {needle:?}, got {:?}",
        msgs(&ix)
    );
    ix
}

#[test]
fn well_formed_documents() {
    assert_wf(r#"<?xml version="1.0" encoding="UTF-8"?><root/>"#);
    assert_wf("<a><b x=\"1\" y='2'>t</b><b/></a>");
    assert_wf("<!-- lead --><a>text &amp; more &#x41; &#65;</a><!-- trail -->");
    assert_wf("<a><![CDATA[ <not> & markup ]]></a>");
    assert_wf("<?pi target?><a/>");
    assert_wf("<!DOCTYPE a [ <!ELEMENT a (#PCDATA)> ]>\n<a>x</a>");
    assert_wf("\u{feff}<a/>");
    assert_wf("<Ünïcødé attr=\"vàl\">tëxt</Ünïcødé>");
    assert_wf("<a>]] ]]&gt; ]</a>");
}

#[test]
fn mismatched_end_tag_suggests_rename() {
    let ix = assert_err("<a><b></c></a>", "does not match start tag");
    let e = ix
        .errors
        .iter()
        .find(|e| e.msg.contains("does not match"))
        .unwrap();
    assert_eq!(e.fix.as_deref(), Some("Change to </b>"));
    assert_eq!(e.line, 1);
}

#[test]
fn skipped_end_tag_suggests_inserting_the_missing_closers() {
    let ix = assert_err("<a><b><c></a>", "does not match start tag");
    let e = ix.errors.iter().find(|e| e.fix.is_some()).unwrap();
    assert_eq!(e.fix.as_deref(), Some("Insert </c></b>"));
}

#[test]
fn stray_end_tag() {
    let ix = assert_err("<a/></b>", "no element is open");
    assert_eq!(ix.errors[0].fix.as_deref(), Some("Delete this end tag"));
}

#[test]
fn unclosed_elements_are_reported_at_eof() {
    let ix = assert_err("<a><b><c>", "element(s) not closed");
    let e = ix.errors.last().unwrap();
    assert!(e.msg.contains("<c>, <b>, <a>"), "{}", e.msg);
    assert_eq!(e.fix.as_deref(), Some("Append </c></b></a>"));
}

#[test]
fn duplicate_attribute() {
    let ix = assert_err(r#"<a x="1" x="2"/>"#, "specified more than once");
    assert_eq!(
        ix.errors[0].fix.as_deref(),
        Some("Remove the duplicate attribute")
    );
    assert_eq!(ix.total_attributes, 2);
}

#[test]
fn unquoted_attribute_value() {
    let ix = assert_err("<a x=1></a>", "Attribute value must be quoted");
    assert_eq!(
        ix.errors[0].fix.as_deref(),
        Some("Enclose the value in double quotes")
    );
}

#[test]
fn bare_ampersand_and_bad_reference() {
    let ix = assert_err("<a>Tom & Jerry</a>", "Unescaped '&'");
    assert_eq!(
        ix.errors[0].fix.as_deref(),
        Some("Replace '&' with '&amp;'")
    );
    assert_err("<a>&#zz;</a>", "Malformed entity/character reference");
    assert_wf("<a>&#x1F600; &apos; &custom-ent;</a>");
}

#[test]
fn double_hyphen_in_comment() {
    let ix = assert_err(
        "<a><!-- bad -- comment --></a>",
        "'--' is not allowed inside comments",
    );
    assert_eq!(ix.errors[0].fix.as_deref(), Some("Replace '--' with '- -'"));
}

#[test]
fn cdata_terminator_in_text() {
    let ix = assert_err(
        "<a>oops ]]> here</a>",
        "']]>' is not allowed in element content",
    );
    assert_eq!(ix.errors[0].fix.as_deref(), Some("Escape as ]]&gt;"));
}

#[test]
fn lt_in_text_and_in_attribute_value() {
    let ix = assert_err("<a>1 < 2</a>", "must be followed by an element name");
    assert_eq!(ix.errors[0].fix.as_deref(), Some("Escape the '<' as &lt;"));
    assert_err(r#"<a x="1 < 2"/>"#, "not allowed in attribute values");
}

#[test]
fn multiple_roots_and_stray_text() {
    assert_err("<a/><b/>", "more than one root element");
    assert_err("junk<a/>", "before the root element");
    assert_err("<a/>junk", "after the root element");
    assert_err("", "Document is empty or has no root element");
}

#[test]
fn truncated_markup_at_eof() {
    assert_err("<a><b attr=\"x", "Unexpected end of file inside markup");
    assert_err("<a><!-- unfinished", "Unexpected end of file inside markup");
}

#[test]
fn structural_index_contents() {
    let src = "<Orders>\n  <Order id=\"1\">\n    <Item/>\n  </Order>\n</Orders>\n";
    let ix = scan(src);
    assert_eq!(ix.error_count, 0);
    assert_eq!(ix.total_elements, 3);
    assert_eq!(ix.total_attributes, 1);
    assert_eq!(ix.max_depth, 3);
    assert_eq!(ix.indexed_elements, 3);
    assert_eq!(ix.line_count, 6);
    assert_eq!(ix.name_of(0), Some("Orders"));
    assert_eq!(ix.name_of(1), Some("Order"));
    assert_eq!(ix.name_of(2), Some("Item"));
    assert_eq!(ix.elem_parent, vec![-1, 0, 1]);
    assert_eq!(ix.elem_depth, vec![0, 1, 2]);
    assert_eq!(ix.elem_line, vec![1, 2, 3]);
    assert_eq!(ix.children_of(0), vec![1]);
    // byte ranges cover the whole element, end offsets are exclusive
    assert_eq!(ix.elem_start[0], 0);
    assert_eq!(ix.elem_end[0] as usize, src.len() - 1);
    assert_eq!(
        &src[ix.elem_start[2] as usize..ix.elem_end[2] as usize],
        "<Item/>"
    );
}

#[test]
fn checkpoints_are_sparse_line_starts() {
    let mut src = String::from("<a>\n");
    for i in 0..200 {
        src.push_str(&format!("  <b>{i}</b>\n"));
    }
    src.push_str("</a>\n");
    let cfg = ScannerConfig {
        stride: 32,
        ..Default::default()
    };
    let ix = Scanner::scan_all(cfg, src.as_bytes());
    assert_eq!(ix.error_count, 0);
    assert_eq!(ix.stride, 32);
    let lines: Vec<&str> = src.split('\n').collect();
    for (block, cp) in ix.checkpoints.iter().enumerate() {
        let line_no = block * 32;
        let expect: usize = lines[..line_no].iter().map(|l| l.len() + 1).sum();
        assert_eq!(*cp as usize, expect, "checkpoint {block}");
    }
    // seek_line maps a line to (checkpoint offset, lines to skip)
    assert_eq!(ix.seek_line(70), Some((ix.checkpoints[2], 6)));
}

#[test]
fn element_budget_prefers_the_top_of_the_tree() {
    let mut src = String::from("<root>\n");
    for i in 0..50 {
        src.push_str(&format!("<row><cell>{i}</cell></row>\n"));
    }
    src.push_str("</root>\n");
    let cfg = ScannerConfig {
        max_indexed: 40,
        ..Default::default()
    };
    let ix = Scanner::scan_all(cfg, src.as_bytes());
    assert_eq!(ix.error_count, 0);
    assert_eq!(ix.total_elements, 101, "every element is still counted");

    // Budget rule (kept bit-compatible with the TypeScript reference engine):
    //   index while `elem_count < max_indexed` OR the element is at depth <= 1,
    //   and stop entirely at `2 * max_indexed`.
    assert_eq!(
        ix.indexed_elements, 70,
        "40 budgeted records + the remaining depth<=1 rows"
    );
    // The first `max_indexed` records take everything, deeper elements included…
    assert!(ix.elem_depth[..40].contains(&2));
    // …afterwards only the navigable top of the tree is kept.
    assert!(ix.elem_depth[40..].iter().all(|d| *d <= 1));
}

#[test]
fn error_cap_counts_everything_but_stores_a_prefix() {
    let src = "<a>".to_string() + &"x & y ".repeat(50) + "</a>";
    let cfg = ScannerConfig {
        max_errors: 5,
        ..Default::default()
    };
    let ix = Scanner::scan_all(cfg, src.as_bytes());
    assert_eq!(ix.errors.len(), 5);
    assert_eq!(ix.error_count, 50);
}

/// The six diagnostics that only `rust/conformance/mini` exercises. The strings are
/// byte-identical to `cases/not-wf/*.xml` and the needles to column 4 of `manifest.tsv`, so
/// the file-based suite and this gate cannot drift apart: the suite is the report, this is the
/// build-time check. (`npm run test:conformance` from `XMLSpy/` runs the same 41 cases against
/// the TypeScript reference engine — that is the direction the parity contract runs in.)
#[test]
fn mini_suite_diagnostics() {
    // cases/not-wf/malformed-cdata-start.xml — [19] CDStart
    assert_err("<a><![CD[x]]></a>", "Malformed CDATA section start");
    // cases/not-wf/unknown-markup-declaration.xml — [29] markupdecl
    assert_err("<!FOO bar>\n<a/>", "Unknown markup declaration");
    // cases/not-wf/comment-bad-start.xml — [15] Comment
    assert_err("<a><!-x-></a>", "Comment must start with");
    // cases/not-wf/invalid-character-in-name.xml — [5] Name
    assert_err("<a$b/>", "Invalid character in element name");
    // cases/not-wf/end-tag-without-name.xml — [42] ETag
    assert_err("<a></ >", "End tag must start with a name");
    // cases/not-wf/junk-in-start-tag.xml — [39] element · [40] STag
    assert_err("<a x=\"1\",y=\"2\"/>", "Unexpected character in start tag");
}
