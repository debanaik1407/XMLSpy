//! Integration tests for the native I/O layer: byte sources, the index cache and the
//! write-ahead journal. Everything here touches the real filesystem, in a per-test
//! directory under the system temp dir.

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use xmlspy_core::ByteSource;
use xmlspy_io::{
    journal_path_for, open_byte_source, recover, stream_chunks, Backed, CacheEntry, Fingerprint,
    FileSource, IndexCache, Journal, JournalHeader, HAVE_MMAP,
};

fn dir(name: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("xmlspy-io-{}-{}", std::process::id(), name));
    let _ = fs::remove_dir_all(&p);
    fs::create_dir_all(&p).unwrap();
    p
}

fn write_file(path: &PathBuf, bytes: &[u8]) {
    let mut f = fs::File::create(path).unwrap();
    f.write_all(bytes).unwrap();
    f.flush().unwrap();
}

/// Deterministic pseudo-random bytes (xorshift64*), so a mismatch points at a real bug.
fn corpus(n: usize) -> Vec<u8> {
    let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut out = Vec::with_capacity(n);
    while out.len() < n {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        out.extend_from_slice(&state.to_le_bytes());
    }
    out.truncate(n);
    out
}

#[test]
fn buffered_source_reads_every_range() {
    let d = dir("filesource");
    let path = d.join("blob.bin");
    let data = corpus(300_000);
    write_file(&path, &data);

    let mut src = FileSource::open(&path).unwrap();
    assert_eq!(src.len(), data.len() as u64);
    assert_eq!(src.as_slice(), None, "a buffered source is not random access");
    assert_eq!(src.chunk(0, 8).unwrap(), &data[..8]);
    assert_eq!(src.chunk(8, 8).unwrap(), &data[8..16]);
    // A read inside the buffered range is served from the buffer…
    assert_eq!(src.buffered(), 8);
    assert_eq!(src.chunk(10, 4).unwrap(), &data[10..14]);
    assert_eq!(src.buffered(), 8, "still the same buffer");
    // …and a jump outside it re-reads.
    assert_eq!(src.chunk(200_000, 16).unwrap(), &data[200_000..200_016]);
    assert_eq!(src.buffered(), 16);
    // Clamped at EOF.
    assert_eq!(
        src.chunk(data.len() as u64 - 4, 999).unwrap(),
        &data[data.len() - 4..]
    );
    assert!(src.chunk(data.len() as u64, 4).unwrap().is_empty());
    assert!(src.chunk(data.len() as u64 + 1, 4).is_err());
    // Reopen gives an independent handle (one per thread).
    let mut twin = src.reopen().unwrap();
    assert_eq!(twin.chunk(0, 8).unwrap(), &data[..8]);
    fs::remove_dir_all(&d).ok();
}

#[test]
fn byte_source_backend_is_chosen_per_platform() {
    let d = dir("backend");
    let path = d.join("a.xml");
    write_file(&path, b"<a>backend</a>");
    let mut src = open_byte_source(&path).unwrap();
    assert_eq!(src.len(), 13);
    if HAVE_MMAP {
        assert!(matches!(src, Backed::Mapped(_)));
        assert_eq!(src.kind(), "mmap");
        assert!(src.is_mapped());
        assert_eq!(src.as_slice(), Some(&b"<a>backend</a>"[..]));
    } else {
        assert_eq!(src.kind(), "buffered");
        assert_eq!(src.as_slice(), None);
    }
    assert_eq!(src.chunk(3, 7).unwrap(), b"backend");

    // An empty document is legal on every backend.
    let empty = d.join("empty.xml");
    write_file(&empty, b"");
    let mut e = open_byte_source(&empty).unwrap();
    assert_eq!(e.len(), 0);
    assert!(e.chunk(0, 16).unwrap().is_empty());
    assert!(e.chunk(1, 16).is_err());
    fs::remove_dir_all(&d).ok();
}

#[test]
fn streaming_covers_every_byte_at_any_chunk_size() {
    let d = dir("stream");
    let path = d.join("blob.bin");
    let data = corpus(70_000);
    write_file(&path, &data);
    for chunk in [1usize, 4095, 4096, 8 * 1024 * 1024] {
        let mut seen = Vec::new();
        let mut offsets = Vec::new();
        let total = stream_chunks(&path, chunk, |b, off| {
            assert!(!b.is_empty());
            offsets.push((off, b.len()));
            seen.extend_from_slice(b);
        })
        .unwrap();
        assert_eq!(total, data.len() as u64);
        assert_eq!(seen, data, "chunk size {chunk}");
        // Offsets are contiguous and ascending.
        let mut next = 0u64;
        for (off, len) in offsets {
            assert_eq!(off, next);
            next += len as u64;
        }
    }
    fs::remove_dir_all(&d).ok();
}

fn fp(path: &str, len: u64, secs: u64) -> Fingerprint {
    Fingerprint {
        path: String::from(path),
        len,
        mtime_secs: secs,
        mtime_nanos: 0,
        edge_hash: len as u32,
    }
}

#[test]
fn cache_stores_validates_and_misses_on_edit() {
    let d = dir("cache");
    let cache = IndexCache::new(d.clone(), 1 << 20).unwrap();
    let f = fp("/tmp/doc.xml", 100, 5);
    assert!(cache.get(&f).is_none(), "cold cache misses");

    cache.put(&f, b"XSI-PAYLOAD").unwrap();
    assert_eq!(cache.get(&f).as_deref(), Some(&b"XSI-PAYLOAD"[..]));
    assert_eq!(cache.stats().entries, 1);
    assert_eq!(cache.stats().bytes, 11);

    // A moved file with identical content still hits: the path is not part of the key.
    assert_eq!(
        cache.get(&fp("/mnt/doc.xml", 100, 5)).as_deref(),
        Some(&b"XSI-PAYLOAD"[..])
    );
    // An edit or a new mtime is a different fingerprint, hence a different key: a miss.
    assert!(cache.get(&fp("/tmp/doc.xml", 101, 5)).is_none());
    assert!(cache.get(&fp("/tmp/doc.xml", 100, 6)).is_none());
    // The entry itself is untouched, so a second read of the *original* still hits.
    assert_eq!(cache.get(&f).as_deref(), Some(&b"XSI-PAYLOAD"[..]));

    // A sidecar that no longer describes the payload (hash collision, hand-edited cache,
    // a partially written file) must invalidate the entry rather than serve stale bytes.
    let sidecar = cache.sidecar_path(&f.key());
    fs::write(&sidecar, fp("/tmp/doc.xml", 100, 999).encode()).unwrap();
    assert!(cache.get(&f).is_none(), "sidecar mismatch is a miss");
    assert_eq!(cache.stats().entries, 0, "…and the stale payload is dropped");
    fs::remove_dir_all(&d).ok();
}

#[test]
fn cache_evicts_least_recently_used_and_keeps_the_newest() {
    let d = dir("cache-lru");
    let big = IndexCache::new(d.clone(), 1 << 20).unwrap();
    let payload = vec![7u8; 1000];
    let mut keys = Vec::new();
    for i in 0..3u64 {
        let f = fp(&format!("/tmp/{i}.xml"), 10 + i, 1_000 + i);
        big.put(&f, &payload).unwrap();
        keys.push(f.key());
    }
    assert_eq!(big.stats().entries, 3);
    // Give them distinct, ascending mtimes: keys[0] oldest, keys[2] newest.
    let now = SystemTime::now();
    for (i, k) in keys.iter().enumerate() {
        let p = big.payload_path(k);
        fs::OpenOptions::new()
            .write(true)
            .open(&p)
            .unwrap()
            .set_modified(now - Duration::from_secs(300 - i as u64 * 100))
            .unwrap();
    }
    let entries = big.entries();
    assert_eq!(
        entries.iter().map(|e| e.key.clone()).collect::<Vec<_>>(),
        keys,
        "entries are listed oldest first"
    );

    // Same directory, tighter budget: 2500 B fits two 1000 B entries.
    let tight = IndexCache::new(d.clone(), 2500).unwrap();
    let removed = tight.evict_to_budget().unwrap();
    assert_eq!(removed, 1);
    let left: Vec<String> = tight.entries().iter().map(|e| e.key.clone()).collect();
    assert_eq!(left, vec![keys[1].clone(), keys[2].clone()]);
    assert!(tight.stats().bytes <= 2500);

    // A budget smaller than one entry keeps the newest rather than emptying the cache.
    let tiny = IndexCache::new(d.clone(), 10).unwrap();
    tiny.evict_to_budget().unwrap();
    let left = tiny.entries();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].key, keys[2]);
    assert_eq!(tiny.clear().unwrap(), 1);
    assert_eq!(tiny.stats().entries, 0);
    fs::remove_dir_all(&d).ok();
}

#[test]
fn cache_entries_report_their_source() {
    let d = dir("cache-src");
    let c = IndexCache::new(d.clone(), 1 << 20).unwrap();
    let f = fp("/data/orders.xml", 42, 9);
    c.put(&f, b"abc").unwrap();
    let e: &CacheEntry = &c.entries()[0];
    assert_eq!(e.source.as_deref(), Some("/data/orders.xml"));
    assert_eq!(e.bytes, 3);
    fs::remove_dir_all(&d).ok();
}

fn header(splits: Vec<u64>) -> JournalHeader {
    JournalHeader {
        stride: 32,
        max_indexed: 300_000,
        max_errors: 200,
        threads: 4,
        source_len: 4096,
        source: fp("/tmp/big.xml", 4096, 11),
        splits,
    }
}

#[test]
fn journal_round_trips_a_committed_build() {
    let d = dir("journal");
    let path = d.join("big.xml.xsi.journal");
    assert_eq!(journal_path_for(&d.join("big.xml")), path);

    let h = header(vec![1024, 2048, 3072]);
    {
        let mut j = Journal::create(&path, &h, false).unwrap();
        j.append_segment(0, b"XSI0").unwrap();
        j.append_segment(1, b"XSI1").unwrap();
        j.append_segment(2, b"XSI2").unwrap();
        j.append_segment(3, b"XSI3").unwrap();
        assert_eq!(j.entries_written(), 4);
        j.commit(4096).unwrap();
    }

    let r = recover(&path).unwrap();
    assert_eq!(r.header, h, "the split plan and config survive");
    assert_eq!(r.committed, Some(4096));
    assert_eq!(r.entries_read, 5);
    assert_eq!(r.stopped_at, None);
    assert_eq!(r.segments.len(), 4);
    assert_eq!(r.segments[2].xsi, b"XSI2");
    assert!(r.missing(4).is_empty());
    assert!(!r.is_empty());
    fs::remove_dir_all(&d).ok();
}

#[test]
fn journal_recovers_from_a_torn_tail() {
    let d = dir("journal-torn");
    let path = d.join("torn.journal");
    let h = header(vec![1024]);
    {
        let mut j = Journal::create(&path, &h, false).unwrap();
        j.append_segment(0, b"XSI0").unwrap();
        j.append_segment(1, b"XSI1").unwrap();
    }
    // Simulate a crash mid-entry: a length that promises more than was written.
    {
        let mut f = fs::OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(&9999u32.to_le_bytes()).unwrap();
        f.write_all(&[1u8, 2, 3]).unwrap();
    }
    let r = recover(&path).unwrap();
    assert_eq!(r.header, h);
    assert_eq!(r.committed, None, "the build never committed");
    assert_eq!(r.segments.len(), 2, "both finished segments survived");
    assert_eq!(r.segments[0].xsi, b"XSI0");
    assert!(r.stopped_at.is_some());
    assert!(
        r.reason.as_deref().unwrap().contains("torn write"),
        "reason was {:?}",
        r.reason
    );
    assert_eq!(r.missing(4), vec![2, 3], "the builder knows what to redo");
    fs::remove_dir_all(&d).ok();
}

#[test]
fn journal_rewrites_a_segment_and_last_write_wins() {
    let d = dir("journal-rewrite");
    let path = d.join("r.journal");
    let h = header(vec![]);
    {
        let mut j = Journal::create(&path, &h, true).unwrap();
        j.append_segment(0, b"OLD").unwrap();
        j.append_segment(0, b"NEW").unwrap();
        j.sync().unwrap();
    }
    let r = recover(&path).unwrap();
    assert_eq!(r.segments.len(), 1);
    assert_eq!(r.segments[0].xsi, b"NEW");
    fs::remove_dir_all(&d).ok();
}

#[test]
fn journal_refuses_a_corrupt_header_and_a_foreign_file() {
    let d = dir("journal-bad");
    let path = d.join("bad.journal");
    let h = header(vec![10]);
    let mut j = Journal::create(&path, &h, false).unwrap();
    j.append_segment(0, b"X").unwrap();
    drop(j);

    let mut raw = fs::read(&path).unwrap();
    raw[12] ^= 0xFF; // inside the header, before the CRC
    let bad = d.join("bad2.journal");
    write_file(&bad, &raw);
    let err = recover(&bad).unwrap_err().to_string();
    assert!(err.contains("CRC"), "unexpected error: {err}");

    write_file(&d.join("nope.journal"), b"definitely not a journal");
    assert!(recover(&d.join("nope.journal")).is_err());
    assert!(recover(&d.join("missing.journal")).is_err());
    fs::remove_dir_all(&d).ok();
}

#[test]
fn journal_entries_are_crc_guarded() {
    let d = dir("journal-crc");
    let path = d.join("c.journal");
    let h = header(vec![]);
    {
        let mut j = Journal::create(&path, &h, false).unwrap();
        j.append_segment(0, b"GOOD").unwrap();
        j.append_segment(1, b"ALSO-GOOD").unwrap();
    }
    let mut raw = fs::read(&path).unwrap();
    // Flip a byte inside the second entry's payload (well past the header).
    let at = raw.len() - 6;
    raw[at] ^= 0x01;
    let p2 = d.join("c2.journal");
    write_file(&p2, &raw);
    let r = recover(&p2).unwrap();
    assert_eq!(r.segments.len(), 1, "the corrupt entry and everything after is dropped");
    assert_eq!(r.segments[0].xsi, b"GOOD");
    assert!(r.reason.as_deref().unwrap().contains("CRC"));
    fs::remove_dir_all(&d).ok();
}

#[test]
fn unix_epoch_is_where_we_think_it_is() {
    // Guards the fingerprint encoding against a surprising clock.
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    assert!(now > 1_700_000_000, "clock says {now}");
    let f = fp("/x", 1, now);
    assert_eq!(Fingerprint::decode(&f.encode()), Some(f));
}
