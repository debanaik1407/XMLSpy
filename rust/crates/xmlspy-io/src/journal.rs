//! The write-ahead journal for long index builds.
//!
//! Indexing a 10 GiB document takes minutes. If the process is killed — OOM, `Ctrl-C`,
//! a laptop lid — everything computed so far is lost unless it was written down. The
//! journal is that write-down:
//!
//! ```text
//! XSJ1 │ version │ cfg │ document fingerprint │ split plan │ header CRC
//! ├─ entry: Segment { idx, .xsi bytes }   one per finished segment
//! ├─ entry: Progress { idx, bytes }       optional heartbeat
//! └─ entry: Commit  { total }             the build finished
//! ```
//!
//! Every entry carries a CRC-32 ([`crate::crc`]), so recovery can tell "the process died
//! here" from "the disk lies". [`recover`] reads entries until the first torn or corrupt
//! one and returns what is usable; the builder then re-scans only the missing segments.
//!
//! # Recovery contract
//!
//! * a header that fails its CRC ⇒ the journal is not ours, start over;
//! * an entry that is short or fails its CRC ⇒ stop there, keep everything before it;
//! * a fingerprint that no longer matches the document ⇒ ignore the journal entirely;
//! * segments are deduplicated by index, last write wins, so a re-scanned segment
//!   supersedes the one recorded before the crash.
//!
//! The journal is a build artefact, not a database: it is deleted once the `.xsi` it
//! produced has been committed to the index cache.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::cache::Fingerprint;
use crate::crc::crc32;

/// Magic bytes at the start of a journal.
pub const MAGIC: [u8; 4] = *b"XSJ1";
/// Journal format version.
pub const VERSION: u16 = 1;
/// Entry kind: one segment's serialised index.
pub const KIND_SEGMENT: u8 = 1;
/// Entry kind: progress heartbeat.
pub const KIND_PROGRESS: u8 = 2;
/// Entry kind: the build completed.
pub const KIND_COMMIT: u8 = 3;

/// What a journal records about the build it belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalHeader {
    /// Scanner line-checkpoint stride.
    pub stride: u32,
    /// Scanner element budget.
    pub max_indexed: u32,
    /// Scanner diagnostic cap.
    pub max_errors: u32,
    /// Threads the build used.
    pub threads: u32,
    /// Document length in bytes.
    pub source_len: u64,
    /// Document fingerprint (validates the journal against the file on disk).
    pub source: Fingerprint,
    /// Segment boundaries: `splits.len() + 1` segments, `splits[0]` is the end of
    /// segment 0 and the start of segment 1.
    pub splits: Vec<u64>,
}

/// One journal entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    /// A finished segment and its serialised index (`.xsi`).
    Segment {
        /// Segment number, 0-based.
        idx: u32,
        /// `.xsi` bytes produced by scanning that segment.
        xsi: Vec<u8>,
    },
    /// Heartbeat: segment `idx` had consumed `bytes` of its range.
    Progress {
        /// Segment number.
        idx: u32,
        /// Bytes scanned so far.
        bytes: u64,
    },
    /// The build finished; the merged index covers `total` bytes.
    Commit {
        /// Document length.
        total: u64,
    },
}

impl Entry {
    fn kind(&self) -> u8 {
        match self {
            Entry::Segment { .. } => KIND_SEGMENT,
            Entry::Progress { .. } => KIND_PROGRESS,
            Entry::Commit { .. } => KIND_COMMIT,
        }
    }

    fn payload(&self) -> Vec<u8> {
        let mut v = Vec::new();
        match self {
            Entry::Segment { idx, xsi } => {
                v.extend_from_slice(&idx.to_le_bytes());
                v.extend_from_slice(&(xsi.len() as u32).to_le_bytes());
                v.extend_from_slice(xsi);
            }
            Entry::Progress { idx, bytes } => {
                v.extend_from_slice(&idx.to_le_bytes());
                v.extend_from_slice(&bytes.to_le_bytes());
            }
            Entry::Commit { total } => v.extend_from_slice(&total.to_le_bytes()),
        }
        v
    }

    fn parse(kind: u8, p: &[u8]) -> Option<Entry> {
        let u32_at = |at: usize| -> Option<u32> {
            let mut a = [0u8; 4];
            a.copy_from_slice(p.get(at..at + 4)?);
            Some(u32::from_le_bytes(a))
        };
        let u64_at = |at: usize| -> Option<u64> {
            let mut a = [0u8; 8];
            a.copy_from_slice(p.get(at..at + 8)?);
            Some(u64::from_le_bytes(a))
        };
        match kind {
            KIND_SEGMENT => {
                let idx = u32_at(0)?;
                let n = u32_at(4)? as usize;
                let xsi = p.get(8..8 + n)?.to_vec();
                Some(Entry::Segment { idx, xsi })
            }
            KIND_PROGRESS => Some(Entry::Progress {
                idx: u32_at(0)?,
                bytes: u64_at(4)?,
            }),
            KIND_COMMIT => Some(Entry::Commit { total: u64_at(0)? }),
            _ => None,
        }
    }
}

/// An append-only journal file.
pub struct Journal {
    file: File,
    path: PathBuf,
    bytes: u64,
    entries: u32,
    durable: bool,
}

impl Journal {
    /// Create (truncating) a journal for `header`.
    ///
    /// `durable` fsyncs after every entry: slower, but survives a machine crash and not
    /// just a process crash.
    pub fn create(path: &Path, header: &JournalHeader, durable: bool) -> io::Result<Journal> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .read(false)
            .open(path)?;
        let buf = encode_header(header);
        file.write_all(&buf)?;
        if durable {
            file.sync_all()?;
        }
        Ok(Journal {
            file,
            path: path.to_path_buf(),
            bytes: buf.len() as u64,
            entries: 0,
            durable,
        })
    }

    /// Append one entry.
    pub fn append(&mut self, e: &Entry) -> io::Result<()> {
        let payload = e.payload();
        let mut rec = Vec::with_capacity(payload.len() + 9);
        rec.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        rec.push(e.kind());
        rec.extend_from_slice(&payload);
        let mut crc_input = Vec::with_capacity(payload.len() + 1);
        crc_input.push(e.kind());
        crc_input.extend_from_slice(&payload);
        rec.extend_from_slice(&crc32(&crc_input).to_le_bytes());
        self.file.write_all(&rec)?;
        self.bytes += rec.len() as u64;
        self.entries += 1;
        if self.durable {
            self.file.sync_all()?;
        } else {
            self.file.flush()?;
        }
        Ok(())
    }

    /// Append a finished segment's index.
    pub fn append_segment(&mut self, idx: u32, xsi: &[u8]) -> io::Result<()> {
        self.append(&Entry::Segment {
            idx,
            xsi: xsi.to_vec(),
        })
    }

    /// Mark the build complete and make sure it is on disk.
    pub fn commit(&mut self, total: u64) -> io::Result<()> {
        self.append(&Entry::Commit { total })?;
        self.file.sync_all()?;
        Ok(())
    }

    /// Force everything written so far to stable storage.
    pub fn sync(&mut self) -> io::Result<()> {
        self.file.flush()?;
        self.file.sync_all()
    }

    /// Journal path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Bytes written.
    pub fn bytes_written(&self) -> u64 {
        self.bytes
    }

    /// Entries written.
    pub fn entries_written(&self) -> u32 {
        self.entries
    }
}

fn encode_header(h: &JournalHeader) -> Vec<u8> {
    let fp = h.source.encode();
    let mut v = Vec::with_capacity(64 + fp.len() + h.splits.len() * 8);
    v.extend_from_slice(&MAGIC);
    v.extend_from_slice(&VERSION.to_le_bytes());
    v.extend_from_slice(&0u16.to_le_bytes()); // reserved flags
    v.extend_from_slice(&h.stride.to_le_bytes());
    v.extend_from_slice(&h.max_indexed.to_le_bytes());
    v.extend_from_slice(&h.max_errors.to_le_bytes());
    v.extend_from_slice(&h.threads.to_le_bytes());
    v.extend_from_slice(&h.source_len.to_le_bytes());
    v.extend_from_slice(&(fp.len() as u32).to_le_bytes());
    v.extend_from_slice(&fp);
    v.extend_from_slice(&(h.splits.len() as u32).to_le_bytes());
    for s in &h.splits {
        v.extend_from_slice(&s.to_le_bytes());
    }
    let crc = crc32(&v);
    v.extend_from_slice(&crc.to_le_bytes());
    v
}

/// A segment recovered from a journal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveredSegment {
    /// Segment number.
    pub idx: u32,
    /// Its serialised index.
    pub xsi: Vec<u8>,
}

/// Everything a journal could still tell us after a crash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recovery {
    /// Build configuration and split plan.
    pub header: JournalHeader,
    /// Segments that finished, ascending by index.
    pub segments: Vec<RecoveredSegment>,
    /// `Some(total)` when the build had committed.
    pub committed: Option<u64>,
    /// Entries read before the journal ran out or turned corrupt.
    pub entries_read: u32,
    /// File offset where reading stopped, when it stopped early.
    pub stopped_at: Option<u64>,
    /// Why reading stopped, in human terms.
    pub reason: Option<String>,
}

impl Recovery {
    /// Segments still missing from a build of `segment_count` segments.
    pub fn missing(&self, segment_count: usize) -> Vec<u32> {
        let have: Vec<u32> = self.segments.iter().map(|s| s.idx).collect();
        (0..segment_count as u32).filter(|i| !have.contains(i)).collect()
    }

    /// True when nothing usable could be recovered.
    pub fn is_empty(&self) -> bool {
        self.segments.is_empty() && self.committed.is_none()
    }
}

/// Read a journal, tolerating a torn tail.
///
/// # Errors
/// Only when the file cannot be opened at all, or its header fails the CRC check —
/// a truncated tail is *not* an error, it is the normal shape of a crash.
pub fn recover(path: &Path) -> io::Result<Recovery> {
    let raw = fs::read(path)?;
    if raw.len() < 8 || raw[0..4] != MAGIC {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{path:?}: not a journal (bad magic)"),
        ));
    }
    const FP_LEN_AT: usize = 4 + 2 + 2 + 4 * 4 + 8; // magic, version, flags, cfg, source_len
    if raw.len() < FP_LEN_AT + 4 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "journal header truncated",
        ));
    }
    let u32_at = |at: usize| -> u32 {
        u32::from_le_bytes([raw[at], raw[at + 1], raw[at + 2], raw[at + 3]])
    };
    let u64_at = |at: usize| -> u64 {
        let mut a = [0u8; 8];
        a.copy_from_slice(&raw[at..at + 8]);
        u64::from_le_bytes(a)
    };
    let fp_len = u32_at(FP_LEN_AT) as usize;
    let fp_at = FP_LEN_AT + 4;
    let split_at = fp_at + fp_len;
    if raw.len() < split_at + 4 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "journal header truncated (fingerprint)",
        ));
    }
    let split_count = u32_at(split_at) as usize;
    let header_end = split_at + 4 + split_count * 8 + 4; // trailing CRC
    if raw.len() < header_end {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "journal header truncated (split plan)",
        ));
    }
    if crc32(&raw[..header_end - 4]) != u32_at(header_end - 4) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "journal header CRC mismatch - refusing to trust it",
        ));
    }
    let Some(source) = Fingerprint::decode(&raw[fp_at..split_at]) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "journal fingerprint unreadable",
        ));
    };
    let mut splits = Vec::with_capacity(split_count);
    for i in 0..split_count {
        splits.push(u64_at(split_at + 4 + i * 8));
    }
    let header = JournalHeader {
        stride: u32_at(8),
        max_indexed: u32_at(12),
        max_errors: u32_at(16),
        threads: u32_at(20),
        source_len: u64_at(24),
        source,
        splits,
    };

    let mut segments: Vec<RecoveredSegment> = Vec::new();
    let mut committed = None;
    let mut entries_read = 0u32;
    let mut pos = header_end;
    let mut stopped_at = None;
    let mut reason = None;

    while pos < raw.len() {
        let entry_start = pos as u64;
        if raw.len() < pos + 4 {
            stopped_at = Some(entry_start);
            reason = Some(String::from(
                "torn write: the entry length did not finish (the process died here)",
            ));
            break;
        }
        let plen = u32_at(pos) as usize;
        pos += 4;
        if plen > (1 << 30) {
            stopped_at = Some(entry_start);
            reason = Some(format!("entry claims {plen} bytes - treating that as corruption"));
            break;
        }
        let end = pos + 1 + plen + 4; // kind + payload + CRC
        if raw.len() < end {
            stopped_at = Some(entry_start);
            reason = Some(String::from(
                "torn write: entry started but did not finish (the process died here)",
            ));
            break;
        }
        let kind = raw[pos];
        let payload = &raw[pos + 1..pos + 1 + plen];
        let mut crc_in = Vec::with_capacity(plen + 1);
        crc_in.push(kind);
        crc_in.extend_from_slice(payload);
        if crc32(&crc_in) != u32_at(pos + 1 + plen) {
            stopped_at = Some(entry_start);
            reason = Some(String::from("entry CRC mismatch - stopping before it"));
            break;
        }
        pos = end;
        let Some(entry) = Entry::parse(kind, payload) else {
            stopped_at = Some(entry_start);
            reason = Some(format!("unknown entry kind {kind}"));
            break;
        };
        entries_read += 1;
        match entry {
            Entry::Segment { idx, xsi } => {
                if let Some(existing) = segments.iter_mut().find(|s| s.idx == idx) {
                    existing.xsi = xsi; // last write wins
                } else {
                    segments.push(RecoveredSegment { idx, xsi });
                }
            }
            Entry::Progress { .. } => {}
            Entry::Commit { total } => committed = Some(total),
        }
    }

    segments.sort_by_key(|s| s.idx);
    Ok(Recovery {
        header,
        segments,
        committed,
        entries_read,
        stopped_at,
        reason,
    })
}

/// Journal path for a document: `<path>.xsi.journal` next to the document, or inside the
/// cache directory when the document's directory is not writable.
pub fn journal_path_for(path: &Path) -> PathBuf {
    let mut p = path.as_os_str().to_os_string();
    p.push(".xsi.journal");
    PathBuf::from(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fp() -> Fingerprint {
        Fingerprint {
            path: String::from("/tmp/x.xml"),
            len: 1024,
            mtime_secs: 7,
            mtime_nanos: 3,
            edge_hash: 99,
        }
    }

    fn header() -> JournalHeader {
        JournalHeader {
            stride: 32,
            max_indexed: 300_000,
            max_errors: 200,
            threads: 4,
            source_len: 1024,
            source: fp(),
            splits: vec![256, 512, 768],
        }
    }

    #[test]
    fn header_layout_is_what_recover_expects() {
        let buf = encode_header(&header());
        assert_eq!(&buf[0..4], b"XSJ1");
        let fp_len_at = 4 + 2 + 2 + 16 + 8;
        let fp_len = u32::from_le_bytes([
            buf[fp_len_at],
            buf[fp_len_at + 1],
            buf[fp_len_at + 2],
            buf[fp_len_at + 3],
        ]) as usize;
        let split_at = fp_len_at + 4 + fp_len;
        let n = u32::from_le_bytes([
            buf[split_at],
            buf[split_at + 1],
            buf[split_at + 2],
            buf[split_at + 3],
        ]);
        assert_eq!(n, 3);
        assert_eq!(buf.len(), split_at + 4 + 3 * 8 + 4);
    }
}
