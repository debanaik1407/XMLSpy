//! Property tests for the rope: deterministic pseudo-random operation sequences checked
//! against a `Vec<u8>` oracle after **every** operation.
//!
//! This is the "proptest round-trips" line of the Phase 1 plan, written without a
//! property-testing dependency — the workspace has none, and a xorshift64\* with a fixed
//! seed reproduces a failure exactly, which is the only property of proptest that matters
//! when a test fails in CI on someone else's machine.
//!
//! Invariants asserted, per operation:
//! * `to_vec() == oracle` (the document is exactly the bytes the oracle holds);
//! * `len() == oracle.len()` and `byte_at(i) == oracle[i]` for sampled `i`;
//! * `slice(a..b) == oracle[a..b]` for sampled ranges;
//! * `each_chunk` concatenation == `to_vec()`, and every piece's range lies inside its
//!   buffer (no piece can read out of bounds);
//! * `unchanged_ratio()` and `original_runs()` agree with the piece list;
//! * the piece list stays bounded — coalescing must actually merge.

use xmlspy_rope::{Rope, Source};

/// xorshift64* — the same generator the resumability tests use.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Rng {
        Rng(if seed == 0 { 0x2545_F491_4F6C_DD1D } else { seed })
    }
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next() % n as u64) as usize
        }
    }
}

/// Small payloads, deliberately full of newlines and XML-ish bytes.
const ALPHABET: &[u8] = b"<>/=\"'\n\r\t &;abz019\xc3\xa9";

fn payload(rng: &mut Rng, max: usize) -> Vec<u8> {
    let n = rng.below(max + 1);
    (0..n).map(|_| ALPHABET[rng.below(ALPHABET.len())]).collect()
}

/// `deep` also checks the line table, which costs O(lines × document) and is therefore
/// only run at the end of a sequence and every `DEEP_EVERY` operations.
const DEEP_EVERY: usize = 250;

fn check_invariants(rope: &Rope, oracle: &[u8], rng: &mut Rng, ctx: &str, deep: bool) {
    assert_eq!(rope.len(), oracle.len(), "{ctx}: length");
    assert_eq!(rope.to_vec(), oracle, "{ctx}: bytes");

    // Pieces must stay inside their buffers and cover the document exactly once.
    let mut covered = 0usize;
    for p in rope.pieces() {
        let buf = match p.src {
            Source::Original => rope.original(),
            Source::Add => rope.added(),
        };
        assert!(
            p.end() <= buf.len(),
            "{ctx}: piece {p:?} escapes its buffer ({} bytes)",
            buf.len()
        );
        assert!(p.len > 0, "{ctx}: empty piece survived coalescing");
        covered += p.len;
    }
    assert_eq!(covered, oracle.len(), "{ctx}: pieces cover the document once");

    // No two adjacent pieces could have been merged.
    let ps = rope.pieces();
    for w in ps.windows(2) {
        let mergeable = w[0].src == w[1].src && w[0].end() == w[1].start;
        assert!(!mergeable, "{ctx}: pieces {:?} and {:?} should have coalesced", w[0], w[1]);
    }

    // Streaming visits every byte in order.
    let mut streamed: Vec<u8> = Vec::with_capacity(rope.len());
    let mut chunks = 0usize;
    rope.try_each_chunk::<(), _>(|c| {
        streamed.extend_from_slice(c);
        chunks += 1;
        Ok(())
    })
    .unwrap();
    assert_eq!(streamed, oracle, "{ctx}: streamed save");
    assert_eq!(chunks, ps.len(), "{ctx}: one chunk per piece");

    // Random access agrees with the oracle.
    for _ in 0..16 {
        if oracle.is_empty() {
            break;
        }
        let i = rng.below(oracle.len());
        assert_eq!(rope.byte_at(i), Some(oracle[i]), "{ctx}: byte_at({i})");
        let a = rng.below(oracle.len());
        let b = rng.below(oracle.len());
        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
        assert_eq!(rope.slice(lo..hi), oracle[lo..hi], "{ctx}: slice({lo}..{hi})");
    }
    assert!(rope.byte_at(oracle.len()).is_none(), "{ctx}: byte past the end");

    // The "untouched original" accounting must match the piece list.
    let kept: usize = ps
        .iter()
        .filter(|p| p.src == Source::Original)
        .map(|p| p.len)
        .sum();
    let runs = rope.original_runs();
    assert_eq!(runs.len(), ps.iter().filter(|p| p.src == Source::Original).count());
    assert_eq!(runs.iter().map(|(_, l)| l).sum::<usize>(), kept);
    if oracle.is_empty() {
        assert_eq!(rope.unchanged_ratio(), 1.0);
    } else {
        let want = kept as f64 / oracle.len() as f64;
        assert!(
            (rope.unchanged_ratio() - want).abs() < 1e-12,
            "{ctx}: unchanged_ratio"
        );
    }

    if !deep {
        return;
    }

    // Line accounting.
    let want_lines = 1 + oracle.iter().filter(|b| **b == b'\n').count();
    assert_eq!(rope.line_count(), want_lines, "{ctx}: line_count");
    let starts = rope.line_starts(usize::MAX);
    assert_eq!(starts.len(), want_lines, "{ctx}: line_starts");
    assert_eq!(starts[0], 0);
    for (n, s) in starts.iter().enumerate().skip(1) {
        assert_eq!(oracle[*s - 1], b'\n', "{ctx}: line {n} does not start after a newline");
    }
    for n in 0..want_lines {
        let r = rope.line_range(n).unwrap_or_else(|| panic!("{ctx}: line_range({n})"));
        let bytes = rope.line(n).unwrap();
        assert_eq!(bytes, oracle[r.start..r.end].to_vec(), "{ctx}: line {n}");
        assert!(r.end == oracle.len() || oracle[r.end] == b'\n', "{ctx}: line {n} end");
    }
}

#[test]
fn random_operations_match_a_vec_oracle() {
    for seed in [1u64, 2, 3, 0xDEAD_BEEF, 0x5EED_0001] {
        let mut rng = Rng::new(seed);
        let base: Vec<u8> = (0..600).map(|i| ALPHABET[i % ALPHABET.len()]).collect();
        let mut rope = Rope::from_slice(&base);
        let mut oracle = base.clone();

        for op in 0..1500 {
            let kind = rng.below(10);
            let at = rng.below(oracle.len() + 1);
            match kind {
                0..=3 => {
                    let ins = payload(&mut rng, 12);
                    rope.insert(at, &ins);
                    oracle.splice(at..at, ins.iter().copied());
                }
                4..=6 => {
                    let to = (at + rng.below(40)).min(oracle.len());
                    rope.delete(at..to);
                    oracle.drain(at..to);
                }
                7 | 8 => {
                    let to = (at + rng.below(20)).min(oracle.len());
                    let rep = payload(&mut rng, 10);
                    rope.replace(at..to, &rep);
                    oracle.splice(at..to, rep.iter().copied());
                }
                _ => {
                    // Out-of-range operations must clamp, not panic.
                    let wild = oracle.len() + 1 + rng.below(1000);
                    let ins = payload(&mut rng, 4);
                    rope.insert(wild, &ins);
                    oracle.splice(oracle.len()..oracle.len(), ins.iter().copied());
                    rope.delete(wild..wild + 500);
                }
            }
            check_invariants(
                &rope,
                &oracle,
                &mut rng,
                &format!("seed {seed} op {op}"),
                op % DEEP_EVERY == 0,
            );
            assert!(
                rope.pieces().len() <= 2 * (op + 2),
                "seed {seed} op {op}: {} pieces is runaway fragmentation",
                rope.pieces().len()
            );
        }
        check_invariants(&rope, &oracle, &mut rng, &format!("seed {seed} final"), true);
    }
}

#[test]
fn delete_everything_then_rebuild_is_lossless() {
    let mut rng = Rng::new(99);
    let base: Vec<u8> = (0..2000).map(|i| ALPHABET[(i * 7) % ALPHABET.len()]).collect();
    let mut rope = Rope::from_slice(&base);
    let mut oracle = base.clone();
    for _ in 0..200 {
        let at = rng.below(oracle.len() + 1);
        let to = (at + rng.below(30)).min(oracle.len());
        if rng.below(2) == 0 {
            let ins = payload(&mut rng, 8);
            rope.insert(at, &ins);
            oracle.splice(at..at, ins.iter().copied());
        } else {
            rope.delete(at..to);
            oracle.drain(at..to);
        }
    }
    check_invariants(&rope, &oracle, &mut rng, "before wipe", true);

    // Wipe and rebuild: the rope must behave like a fresh one afterwards.
    rope.delete(0..rope.len());
    oracle.clear();
    check_invariants(&rope, &oracle, &mut rng, "after wipe", true);
    assert!(rope.is_empty());

    rope.insert(0, &base);
    oracle.extend_from_slice(&base);
    check_invariants(&rope, &oracle, &mut rng, "after rebuild", true);
    assert_eq!(rope.to_vec(), base);
    assert_eq!(rope.unchanged_ratio(), 0.0, "everything now lives in the add buffer");
    assert_eq!(rope.pieces().len(), 1);
}

#[test]
fn coalesce_never_changes_the_document() {
    let mut rng = Rng::new(4242);
    let mut rope = Rope::from_slice(b"<root>\n  <child id=\"1\">text</child>\n</root>\n");
    let mut oracle = rope.to_vec();
    for i in 0..300 {
        let at = rng.below(oracle.len() + 1);
        let ins = payload(&mut rng, 6);
        rope.insert(at, &ins);
        oracle.splice(at..at, ins.iter().copied());
        if i % 7 == 0 {
            let before = rope.to_vec();
            let merged = rope.coalesce();
            assert_eq!(rope.to_vec(), before, "coalesce changed the document");
            assert_eq!(merged, 0, "coalesce already runs after every mutation");
        }
    }
    check_invariants(&rope, &oracle, &mut rng, "coalesce", true);
}

#[test]
fn line_edits_match_a_line_oracle() {
    let mut rng = Rng::new(7);
    let mut lines: Vec<String> = (0..20).map(|i| format!("<row id=\"{i}\">{i}</row>")).collect();
    let mut rope = Rope::from_slice(lines.join("\n").as_bytes());
    for op in 0..400 {
        if rope.line_count() == 0 {
            break;
        }
        let n = rope.line_count();
        let at = rng.below(n);
        if rng.below(2) == 0 {
            let text = format!("<ins op=\"{op}\"/>");
            rope.insert_line_after(at, text.as_bytes());
            lines.insert(at + 1, text);
        } else {
            rope.delete_line(at);
            lines.remove(at);
        }
        let want = lines.join("\n");
        assert_eq!(
            String::from_utf8_lossy(&rope.to_vec()),
            want,
            "line op {op} diverged"
        );
        assert_eq!(rope.line_count(), lines.len());
    }
    // Deleting the last remaining line leaves an empty document.
    while rope.line_count() > 1 {
        rope.delete_line(0);
    }
    rope.delete_line(0);
    assert!(rope.is_empty(), "one line left: {:?}", rope.to_vec());
}

#[test]
fn a_three_byte_edit_in_a_large_document_stays_streamable() {
    // The Phase 1 gate: "save of a 3-byte edit in a 10 GiB file". Scaled down, but the
    // shape is what matters — the edit must not copy the document.
    let big: Vec<u8> = (0..4 * 1024 * 1024)
        .map(|i| ALPHABET[i % ALPHABET.len()])
        .collect();
    let mut rope = Rope::from_slice(&big);
    assert_eq!(rope.pieces().len(), 1);

    rope.insert(big.len() / 2, b"abc");

    assert_eq!(rope.pieces().len(), 3, "one split, one insert, no rewriting");
    assert_eq!(rope.original_runs(), vec![(0, big.len() / 2), (big.len() / 2, big.len() / 2)]);
    assert!(rope.unchanged_ratio() > 0.999_999);
    let stats = rope.stats();
    assert_eq!(stats.add_bytes, 3);
    assert_eq!(stats.original_bytes, big.len());
    assert_eq!(stats.longest_original_run, big.len() / 2);

    // A streamed save writes three runs and never materialises the document.
    let mut written = 0usize;
    let mut runs = 0usize;
    rope.try_each_chunk::<(), _>(|c| {
        written += c.len();
        runs += 1;
        Ok(())
    })
    .unwrap();
    assert_eq!(runs, 3);
    assert_eq!(written, big.len() + 3);

    // Deleting the edit again restores the single-piece shape.
    rope.delete(big.len() / 2..big.len() / 2 + 3);
    assert_eq!(rope.to_vec(), big);
    assert_eq!(rope.pieces().len(), 1);
    assert_eq!(rope.unchanged_ratio(), 1.0);
}
