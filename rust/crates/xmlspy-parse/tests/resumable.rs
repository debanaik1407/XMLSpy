//! The property the whole streaming design rests on: **the chunk boundaries must not
//! matter**. Scanning a document in one `feed` and scanning it in chunks of 1, 2, 3, …
//! bytes (or at pseudo-random boundaries) must produce a bit-identical index, identical
//! diagnostics and an identical `.xsi` serialisation.

use xmlspy_index::xsi;
use xmlspy_parse::{Scanner, ScannerConfig, StructuralIndex};

fn scan_chunked(src: &[u8], chunk: usize) -> StructuralIndex {
    let mut s = Scanner::new(ScannerConfig {
        stride: 8,
        max_errors: 1000,
        ..Default::default()
    });
    let mut off = 0usize;
    while off < src.len() {
        let end = (off + chunk).min(src.len());
        s.feed(&src[off..end], off as u64);
        off = end;
    }
    s.finish(src.len() as u64);
    s.into_index()
}

fn scan_at_splits(src: &[u8], splits: &[usize]) -> StructuralIndex {
    let mut s = Scanner::new(ScannerConfig {
        stride: 8,
        max_errors: 1000,
        ..Default::default()
    });
    let mut off = 0usize;
    for w in splits {
        let end = (off + w).min(src.len());
        if end > off {
            s.feed(&src[off..end], off as u64);
        }
        off = end;
    }
    if off < src.len() {
        s.feed(&src[off..], off as u64);
    }
    s.finish(src.len() as u64);
    s.into_index()
}

const CORPORA: &[&str] = &[
    "<a/>",
    "\u{feff}<?xml version=\"1.0\"?><a x=\"1\"><b>t</b></a>",
    "<root>\n  <e a='1' b=\"2\">text &amp; ref &#x41;</e>\n  <!-- c -->\n  <![CDATA[ ]] ]]> ]]>\n  <?pi x?>\n</root>\n",
    "<!DOCTYPE r [ <!ENTITY e \"v\"> <!ELEMENT r (#PCDATA)> ]>\n<r>&e;</r>",
    // malformed on purpose: every diagnostic path must also be resumable
    "<a><b x=1 x=\"2\"></c>Tom & Jerry<!-- -- --><d>]]>",
    "<Ünïcødé attr=\"vàlüe\">tëxt ünïcødé</Ünïcødé>",
];

fn big_corpus() -> String {
    let mut s = String::from("<PurchaseOrders xmlns=\"urn:xmlspy:orders\">\n");
    for i in 0..400 {
        s.push_str(&format!(
            "  <PurchaseOrder id=\"{i}\" date=\"2026-09-05\">\n    <Address type=\"Ship\"><Name>Näme {i}</Name><City>Zürich</City></Address>\n    <Items><Item PartNumber=\"P-{i}\"><Qty>{}</Qty><Price>{}.99</Price></Item></Items>\n  </PurchaseOrder>\n",
            i % 7 + 1,
            i * 3 % 97
        ));
    }
    s.push_str("</PurchaseOrders>\n");
    s
}

#[test]
fn every_chunk_size_produces_the_same_index() {
    for src in CORPORA {
        let bytes = src.as_bytes();
        let whole = scan_chunked(bytes, bytes.len().max(1));
        for chunk in 1..=bytes.len().min(64) {
            let got = scan_chunked(bytes, chunk);
            assert_eq!(whole, got, "chunk size {chunk} diverged for {src:?}");
        }
    }
}

#[test]
fn pseudo_random_boundaries_match_a_single_pass() {
    let src = big_corpus();
    let bytes = src.as_bytes();
    let whole = scan_chunked(bytes, bytes.len());
    assert_eq!(whole.error_count, 0);
    assert_eq!(whole.total_elements, 400 * 8 + 1);

    // xorshift64* — deterministic, no dev-dependencies.
    let mut state: u64 = 0x2545_F491_4F6C_DD1D;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for round in 0..24 {
        let splits: Vec<usize> = (0..600).map(|_| (next() % 137) as usize).collect();
        let got = scan_at_splits(bytes, &splits);
        assert_eq!(whole, got, "random split round {round} diverged");
    }
}

#[test]
fn chunked_scan_round_trips_through_xsi() {
    let src = big_corpus();
    let bytes = src.as_bytes();
    let whole = scan_chunked(bytes, bytes.len());
    let chunked = scan_chunked(bytes, 4096);
    assert_eq!(xsi::encode(&whole, false), xsi::encode(&chunked, false));
    let decoded = xsi::decode(&xsi::encode(&chunked, false)).unwrap();
    assert_eq!(decoded, whole);
    // The index cost is bounded *per element* and independent of document size:
    // 34 fixed bytes per record plus the interned names (this corpus is unusually
    // element-dense, so the ratio to the document size is deliberately not asserted).
    let per_element = xsi::encode(&whole, false).len() as f64 / whole.total_elements as f64;
    assert!(per_element < 64.0, "index costs {per_element:.1} B/element");
}

#[test]
fn progressive_open_snapshot_is_usable_before_the_end() {
    let src = big_corpus();
    let bytes = src.as_bytes();
    let mut s = Scanner::new(ScannerConfig::default());
    s.feed(&bytes[..2048], 0);
    let snap = s.index_snapshot();
    assert!(
        snap.indexed_elements > 0,
        "first screen must be navigable already"
    );
    assert!(snap.line_count > 1);
    assert_eq!(snap.file_len, 2048);
    // …and finishing the rest yields the complete index.
    s.feed(&bytes[2048..], 2048);
    s.finish(bytes.len() as u64);
    let full = s.into_index();
    assert_eq!(full.total_elements, 3201);
    assert_eq!(full.error_count, 0);
}

#[test]
fn cancelled_scan_leaves_a_partial_but_valid_index() {
    let src = big_corpus();
    let bytes = src.as_bytes();
    let mut s = Scanner::new(ScannerConfig::default());
    s.feed(&bytes[..10_000], 0);
    let partial = s.index_snapshot();
    let buf = xsi::encode(&partial, true);
    let back = xsi::decode(&buf).unwrap();
    assert_eq!(back.indexed_elements, partial.indexed_elements);
    assert_eq!(u16::from_le_bytes([buf[6], buf[7]]), xsi::FLAG_PARTIAL);
}
