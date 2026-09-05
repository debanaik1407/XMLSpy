//! The on-disk index cache: `.xsi` files keyed by a content fingerprint, with an
//! LRU eviction policy.
//!
//! This is the native half of "session restore": re-opening a document whose fingerprint
//! has not changed loads a cached structural index instead of re-scanning, which is what
//! turns a multi-GB re-open from seconds into a memory map. The browser half (IndexedDB)
//! is the same policy with a different store.
//!
//! # Eviction policy
//!
//! * The cache has a **byte budget** (`--cache-budget`, default 2 GiB).
//! * Entries are evicted **least-recently-used first**, where "used" is the file's mtime:
//!   both [`IndexCache::get`] and [`IndexCache::put`] touch the entry they read or wrote.
//! * The most recently used entry is never evicted, even if it alone exceeds the budget —
//!   evicting the thing you just built would make the cache useless. The budget is
//!   therefore a soft ceiling, and [`CacheStats`] reports the overshoot.
//! * An entry whose fingerprint no longer matches the document is a miss, not an error:
//!   the stale file is removed and the caller rebuilds.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Default cache budget: 2 GiB of `.xsi` files.
pub const DEFAULT_BUDGET: u64 = 2 << 30;

/// Bytes of the head and tail of a document folded into [`Fingerprint::edge_hash`].
pub const EDGE_BYTES: u64 = 4096;

/// FNV-1a, 64-bit — used for cache keys only (the CRC-32 in [`crate::crc`] guards the
/// journal, where a collision would mean silent corruption).
fn fnv1a64(mut h: u64, bytes: &[u8]) -> u64 {
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01B3);
    }
    h
}

/// Everything needed to decide whether a cached index still describes a document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fingerprint {
    /// Path the fingerprint was taken from (reported, not compared).
    pub path: String,
    /// File length in bytes.
    pub len: u64,
    /// Modification time, seconds since the unix epoch (0 if the platform has none).
    pub mtime_secs: u64,
    /// Modification time, nanosecond part.
    pub mtime_nanos: u32,
    /// CRC-32 over the first and last [`EDGE_BYTES`] bytes.
    pub edge_hash: u32,
}

impl Fingerprint {
    /// Fingerprint a document: stat it, then hash its two ends.
    ///
    /// Reading 8 KiB instead of the whole file is what makes cache validation cheap
    /// enough to run on every open; `len` + `mtime` + the two ends catch every edit that
    /// a real editor makes (an in-place edit of a byte in the middle of a 10 GiB file
    /// changes `mtime`, which is why it is part of the key).
    pub fn of(path: &Path) -> io::Result<Fingerprint> {
        let meta = fs::metadata(path)?;
        let len = meta.len();
        let (mtime_secs, mtime_nanos) = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| (d.as_secs(), d.subsec_nanos()))
            .unwrap_or((0, 0));

        let mut f = fs::File::open(path)?;
        let head_len = len.min(EDGE_BYTES) as usize;
        let mut head = vec![0u8; head_len];
        read_exact_or_eof(&mut f, &mut head)?;
        let mut tail: Vec<u8> = Vec::new();
        if len > 2 * EDGE_BYTES {
            use std::io::{Read, Seek, SeekFrom};
            let mut tf = fs::File::open(path)?;
            tf.seek(SeekFrom::End(-(EDGE_BYTES as i64)))?;
            tail.resize(EDGE_BYTES as usize, 0);
            read_exact_or_eof(&mut tf, &mut tail)?;
        }
        let mut h = crate::crc::crc32(&head);
        h = crate::crc::crc32_update(h ^ u32::MAX, &tail) ^ u32::MAX;

        Ok(Fingerprint {
            path: path.to_string_lossy().into_owned(),
            len,
            mtime_secs,
            mtime_nanos,
            edge_hash: h,
        })
    }

    /// True when two fingerprints describe the same bytes (the path is ignored, so a
    /// document that was moved or renamed still hits the cache).
    pub fn same_content(&self, other: &Fingerprint) -> bool {
        self.len == other.len
            && self.mtime_secs == other.mtime_secs
            && self.mtime_nanos == other.mtime_nanos
            && self.edge_hash == other.edge_hash
    }

    /// Cache key: 16 hex digits of the fingerprint fields, plus the length so that two
    /// documents cannot share a key by hashing alike.
    pub fn key(&self) -> String {
        let mut h = 0xCBF2_9CE4_8422_2325u64;
        h = fnv1a64(h, &self.len.to_le_bytes());
        h = fnv1a64(h, &self.mtime_secs.to_le_bytes());
        h = fnv1a64(h, &self.mtime_nanos.to_le_bytes());
        h = fnv1a64(h, &self.edge_hash.to_le_bytes());
        format!("{h:016x}-{}", self.len)
    }

    /// Sidecar encoding (`XFP1` + fixed fields + path).
    pub fn encode(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(64 + self.path.len());
        v.extend_from_slice(b"XFP1");
        v.extend_from_slice(&self.len.to_le_bytes());
        v.extend_from_slice(&self.mtime_secs.to_le_bytes());
        v.extend_from_slice(&self.mtime_nanos.to_le_bytes());
        v.extend_from_slice(&self.edge_hash.to_le_bytes());
        v.extend_from_slice(&(self.path.len() as u32).to_le_bytes());
        v.extend_from_slice(self.path.as_bytes());
        v
    }

    /// Inverse of [`Fingerprint::encode`]; `None` when the sidecar is short or corrupt.
    pub fn decode(buf: &[u8]) -> Option<Fingerprint> {
        if buf.len() < 32 || &buf[0..4] != b"XFP1" {
            return None;
        }
        let u64_at = |at: usize| -> Option<u64> {
            let mut a = [0u8; 8];
            a.copy_from_slice(buf.get(at..at + 8)?);
            Some(u64::from_le_bytes(a))
        };
        let u32_at = |at: usize| -> Option<u32> {
            let mut a = [0u8; 4];
            a.copy_from_slice(buf.get(at..at + 4)?);
            Some(u32::from_le_bytes(a))
        };
        let path_len = u32_at(28)? as usize;
        let path_bytes = buf.get(32..32 + path_len)?;
        Some(Fingerprint {
            path: String::from_utf8_lossy(path_bytes).into_owned(),
            len: u64_at(4)?,
            mtime_secs: u64_at(12)?,
            mtime_nanos: u32_at(20)?,
            edge_hash: u32_at(24)?,
        })
    }
}

/// Fill `buf`, accepting an early EOF (the file may have shrunk between `stat` and read).
fn read_exact_or_eof<R: io::Read>(r: &mut R, buf: &mut [u8]) -> io::Result<()> {
    let mut filled = 0usize;
    while filled < buf.len() {
        match r.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// One entry of the cache directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheEntry {
    /// Cache key (the file stem).
    pub key: String,
    /// Size of the `.xsi` payload in bytes.
    pub bytes: u64,
    /// Last use, seconds since the unix epoch.
    pub used: u64,
    /// Last use, nanosecond part — LRU ordering needs sub-second resolution, otherwise
    /// everything written in the same second looks equally old.
    pub used_nanos: u32,
    /// Path of the document it belongs to, if the sidecar is readable.
    pub source: Option<String>,
}

/// What the cache currently holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CacheStats {
    /// Number of `.xsi` entries.
    pub entries: usize,
    /// Total bytes they occupy.
    pub bytes: u64,
    /// Configured budget.
    pub budget: u64,
}

/// A byte-budgeted directory of cached `.xsi` indexes.
#[derive(Debug, Clone)]
pub struct IndexCache {
    dir: PathBuf,
    budget: u64,
}

impl IndexCache {
    /// Open (creating if needed) a cache directory with a byte budget.
    pub fn new(dir: PathBuf, budget: u64) -> io::Result<IndexCache> {
        fs::create_dir_all(&dir)?;
        Ok(IndexCache { dir, budget })
    }

    /// Where the cache lives by default: `$XMLSPY_CACHE`, else
    /// `$XDG_CACHE_HOME/xmlspy/index`, else `~/.cache/xmlspy/index`, else a temporary
    /// directory.
    pub fn default_dir() -> PathBuf {
        if let Ok(d) = std::env::var("XMLSPY_CACHE") {
            if !d.is_empty() {
                return PathBuf::from(d);
            }
        }
        if let Ok(xdg) = std::env::var("XDG_CACHE_HOME") {
            if !xdg.is_empty() {
                return PathBuf::from(xdg).join("xmlspy").join("index");
            }
        }
        if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
            return PathBuf::from(home)
                .join(".cache")
                .join("xmlspy")
                .join("index");
        }
        std::env::temp_dir().join("xmlspy-cache")
    }

    /// The cache directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The byte budget.
    pub fn budget(&self) -> u64 {
        self.budget
    }

    /// Path of the payload for `key`.
    pub fn payload_path(&self, key: &str) -> PathBuf {
        self.dir.join(format!("{key}.xsi"))
    }

    /// Path of the fingerprint sidecar for `key`.
    pub fn sidecar_path(&self, key: &str) -> PathBuf {
        self.dir.join(format!("{key}.fp"))
    }

    /// Look up a cached index for `fp`, validating the sidecar and touching the entry.
    pub fn get(&self, fp: &Fingerprint) -> Option<Vec<u8>> {
        let key = fp.key();
        let side = fs::read(self.sidecar_path(&key)).ok()?;
        let stored = Fingerprint::decode(&side)?;
        if !stored.same_content(fp) {
            // Stale: drop it so the budget is not spent on bytes nobody can use.
            let _ = self.remove(&key);
            return None;
        }
        let bytes = fs::read(self.payload_path(&key)).ok()?;
        touch(&self.payload_path(&key));
        touch(&self.sidecar_path(&key));
        Some(bytes)
    }

    /// Store an index, atomically (temporary file + rename), then enforce the budget.
    pub fn put(&self, fp: &Fingerprint, xsi: &[u8]) -> io::Result<PathBuf> {
        let key = fp.key();
        let final_path = self.payload_path(&key);
        let tmp = self.dir.join(format!("{key}.xsi.tmp-{}", std::process::id()));
        {
            let mut f = fs::File::create(&tmp)?;
            f.write_all(xsi)?;
            f.flush()?;
            f.sync_all()?;
        }
        fs::rename(&tmp, &final_path)?;
        fs::write(self.sidecar_path(&key), fp.encode())?;
        self.evict_to_budget()?;
        Ok(final_path)
    }

    /// Remove one entry (payload + sidecar). Returns true when something was deleted.
    pub fn remove(&self, key: &str) -> io::Result<bool> {
        let a = fs::remove_file(self.payload_path(key));
        let b = fs::remove_file(self.sidecar_path(key));
        Ok(a.is_ok() || b.is_ok())
    }

    /// Every entry, oldest use first.
    pub fn entries(&self) -> Vec<CacheEntry> {
        let mut out = Vec::new();
        let Ok(rd) = fs::read_dir(&self.dir) else {
            return out;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|e| e.to_str()) != Some("xsi") {
                continue;
            }
            let Some(key) = p.file_stem().map(|s| s.to_string_lossy().into_owned()) else {
                continue;
            };
            let bytes = e.metadata().map(|m| m.len()).unwrap_or(0);
            let (used, used_nanos) = e
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| (d.as_secs(), d.subsec_nanos()))
                .unwrap_or((0, 0));
            let source = fs::read(self.sidecar_path(&key))
                .ok()
                .and_then(|b| Fingerprint::decode(&b))
                .map(|fp| fp.path);
            out.push(CacheEntry {
                key,
                bytes,
                used,
                used_nanos,
                source,
            });
        }
        out.sort_by_key(|e| (e.used, e.used_nanos, e.key.clone()));
        out
    }

    /// Current occupancy.
    pub fn stats(&self) -> CacheStats {
        let e = self.entries();
        CacheStats {
            bytes: e.iter().map(|x| x.bytes).sum(),
            entries: e.len(),
            budget: self.budget,
        }
    }

    /// Evict least-recently-used entries until the cache fits the budget.
    ///
    /// The newest entry always survives (see the policy note in the module docs).
    /// Returns how many entries were removed.
    pub fn evict_to_budget(&self) -> io::Result<usize> {
        let entries = self.entries(); // already oldest-first
        let mut total: u64 = entries.iter().map(|e| e.bytes).sum();
        let mut removed = 0usize;
        if entries.len() <= 1 || total <= self.budget {
            return Ok(0);
        }
        for e in &entries {
            if total <= self.budget || removed + 1 >= entries.len() {
                break;
            }
            if self.remove(&e.key)? {
                total = total.saturating_sub(e.bytes);
                removed += 1;
            }
        }
        Ok(removed)
    }

    /// Empty the cache. Returns how many entries were removed.
    pub fn clear(&self) -> io::Result<usize> {
        let entries = self.entries();
        let mut n = 0;
        for e in &entries {
            if self.remove(&e.key)? {
                n += 1;
            }
        }
        Ok(n)
    }
}

/// Bump a file's mtime so LRU ordering follows actual use.
///
/// Best effort: on a read-only cache directory this fails silently and the policy
/// degrades to insertion order, which is still a sane eviction order.
fn touch(path: &Path) {
    if let Ok(f) = fs::OpenOptions::new().write(true).open(path) {
        let _ = f.set_modified(SystemTime::now());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fp(path: &str, len: u64, secs: u64) -> Fingerprint {
        Fingerprint {
            path: String::from(path),
            len,
            mtime_secs: secs,
            mtime_nanos: 0,
            edge_hash: 0xDEAD_BEEF,
        }
    }

    #[test]
    fn fingerprint_sidecar_round_trips() {
        let f = fp("/tmp/po256.xml", 268_435_456, 1_788_000_000);
        let buf = f.encode();
        assert_eq!(&buf[0..4], b"XFP1");
        assert_eq!(Fingerprint::decode(&buf), Some(f.clone()));
        // A moved file with identical bytes still hits the cache…
        let moved = fp("/mnt/data/po256.xml", f.len, f.mtime_secs);
        assert!(f.same_content(&moved));
        assert_eq!(f.key(), moved.key());
        // …any content or mtime change misses.
        let edited = fp(f.path.as_str(), f.len + 1, f.mtime_secs);
        assert!(!f.same_content(&edited));
        assert_ne!(f.key(), edited.key());
        assert!(Fingerprint::decode(b"nope").is_none());
        assert!(Fingerprint::decode(&buf[..10]).is_none());
    }

    #[test]
    fn keys_are_hex_and_stable() {
        let f = fp("a.xml", 10, 1);
        let k = f.key();
        assert!(k.ends_with("-10"));
        assert!(k.chars().take(16).all(|c| c.is_ascii_hexdigit()));
        assert_eq!(k, f.key());
        assert_ne!(k, fp("a.xml", 11, 1).key());
    }
}
