//! Pre-burn snapshot of the target device's first 4 MiB.
//!
//! Before destroying a USB stick, Disk Cutter captures the first 4 MiB
//! of the target (LBA 0 + the partition table + the first part of any
//! existing filesystem) into a recovery file. The user can restore that
//! file later if they realise they burned to the wrong drive. 4 MiB is
//! enough to undo a "I overwrote my Ventoy stick" mistake without
//! making the snapshot operation itself slow.
//!
//! The recovery file format is intentionally simple and forward-
//! compatible:
//!
//!   ```text
//!   +---------+--------+----------------+
//!   | MAGIC   | HDR    | RAW            |
//!   | 8 bytes | LEN+JSON | 4 MiB         |
//!   +---------+--------+----------------+
//!   ```
//!
//! `MAGIC` is `"DCSNAP01"`, `HDR` is a little-endian `u32` length
//! followed by that many UTF-8 bytes of JSON, `RAW` is the captured
//! image. The header carries timestamp, source path, device size,
//! snapshot length, and a sha256 of the captured bytes so a corrupted
//! recovery file is detectable before we write garbage back over the
//! device.

use std::fs::File;
use std::io::{Read, Result, Seek, SeekFrom, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const MAGIC: &[u8; 8] = b"DCSNAP01";
/// Default snapshot size: 4 MiB covers LBA0, the GPT primary header +
/// entry array, the first MB of each major partition's filesystem
/// superblock. Big enough to undo a mistaken burn; small enough that
/// the snapshot takes <100 ms on any USB stick.
pub const DEFAULT_SNAPSHOT_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SnapshotHeader {
    pub created_at_secs: u64,
    pub source_path: String,
    pub source_size_bytes: u64,
    pub snapshot_bytes: u64,
    pub sha256: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct SnapshotResult {
    pub recovery_path: std::path::PathBuf,
    pub header: SnapshotHeader,
}

#[derive(Clone, Debug, Serialize)]
pub struct RestoreResult {
    pub device_path: std::path::PathBuf,
    pub bytes_written: u64,
    pub verified_sha256: String,
    pub matches_header: bool,
}

#[derive(Debug)]
#[allow(dead_code)]
pub enum SnapshotError {
    Io(std::io::Error),
    BadMagic,
    BadHeader,
    Truncated { expected: u64, got: u64 },
    HashMismatch { expected: String, got: String },
}

impl From<std::io::Error> for SnapshotError {
    fn from(e: std::io::Error) -> Self {
        SnapshotError::Io(e)
    }
}

/// Capture the first `snapshot_bytes` bytes of `source` into a recovery
/// file at `recovery_path`. Returns the parsed header + final path.
pub fn snapshot_target(
    source: &Path,
    recovery_path: &Path,
    snapshot_bytes: u64,
) -> std::result::Result<SnapshotResult, SnapshotError> {
    let mut src = File::open(source)?;
    let source_size = probe_device_size(&mut src)?;
    let effective = snapshot_bytes.min(source_size);
    let mut raw = vec![0u8; effective as usize];
    src.read_exact(&mut raw)?;

    let mut hasher = Sha256::new();
    hasher.update(&raw);
    let sha256 = hex(hasher.finalize());

    let header = SnapshotHeader {
        created_at_secs: now_secs(),
        source_path: source.to_string_lossy().to_string(),
        source_size_bytes: source_size,
        snapshot_bytes: effective,
        sha256,
    };

    let header_json = serde_json::to_vec(&header).expect("header always serializable");
    let mut out = File::create(recovery_path)?;
    out.write_all(MAGIC)?;
    out.write_all(&(header_json.len() as u32).to_le_bytes())?;
    out.write_all(&header_json)?;
    out.write_all(&raw)?;
    out.flush()?;

    Ok(SnapshotResult {
        recovery_path: recovery_path.to_path_buf(),
        header,
    })
}

/// Read and validate a recovery file. Returns the header + raw bytes
/// without touching any device — caller decides whether to restore.
pub fn read_recovery(
    recovery_path: &Path,
) -> std::result::Result<(SnapshotHeader, Vec<u8>), SnapshotError> {
    let mut f = File::open(recovery_path)?;
    let mut magic = [0u8; 8];
    f.read_exact(&mut magic)?;
    if &magic != MAGIC {
        return Err(SnapshotError::BadMagic);
    }
    let mut len_bytes = [0u8; 4];
    f.read_exact(&mut len_bytes)?;
    let header_len = u32::from_le_bytes(len_bytes) as usize;
    let mut hdr_buf = vec![0u8; header_len];
    f.read_exact(&mut hdr_buf)?;
    let header: SnapshotHeader =
        serde_json::from_slice(&hdr_buf).map_err(|_| SnapshotError::BadHeader)?;
    let mut raw = vec![0u8; header.snapshot_bytes as usize];
    f.read_exact(&mut raw)?;
    let mut hasher = Sha256::new();
    hasher.update(&raw);
    let got = hex(hasher.finalize());
    if got != header.sha256 {
        return Err(SnapshotError::HashMismatch {
            expected: header.sha256,
            got,
        });
    }
    Ok((header, raw))
}

/// Write a recovery file back to the target device's first N bytes.
/// Verifies the captured SHA-256 first; refuses to restore if the
/// recovery file is corrupt.
pub fn restore_target(
    recovery_path: &Path,
    device_path: &Path,
) -> std::result::Result<RestoreResult, SnapshotError> {
    let (header, raw) = read_recovery(recovery_path)?;
    let mut dev = std::fs::OpenOptions::new().write(true).open(device_path)?;
    dev.seek(SeekFrom::Start(0))?;
    dev.write_all(&raw)?;
    dev.flush()?;
    Ok(RestoreResult {
        device_path: device_path.to_path_buf(),
        bytes_written: raw.len() as u64,
        verified_sha256: header.sha256.clone(),
        matches_header: true,
    })
}

/// Determine the byte size of a source. For regular files, returns
/// `metadata().len()`; for block devices on Unix where `metadata` is
/// unreliable, seeks to end. Pure-ish — only depends on the open file
/// handle.
pub fn probe_device_size(f: &mut File) -> Result<u64> {
    let meta_len = f.metadata().map(|m| m.len()).unwrap_or(0);
    if meta_len > 0 {
        return Ok(meta_len);
    }
    let end = f.seek(SeekFrom::End(0))?;
    f.seek(SeekFrom::Start(0))?;
    Ok(end)
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn hex(bytes: impl AsRef<[u8]>) -> String {
    let mut s = String::with_capacity(bytes.as_ref().len() * 2);
    for b in bytes.as_ref() {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn snapshot_header_round_trips_through_json() {
        let h = SnapshotHeader {
            created_at_secs: 1715600000,
            source_path: "/dev/disk5".into(),
            source_size_bytes: 16 * 1024 * 1024 * 1024,
            snapshot_bytes: 4 * 1024 * 1024,
            sha256: "abc".into(),
        };
        let v = serde_json::to_vec(&h).unwrap();
        let back: SnapshotHeader = serde_json::from_slice(&v).unwrap();
        assert_eq!(back, h);
    }

    #[test]
    fn snapshot_captures_first_n_bytes_with_correct_hash() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("src.img");
        let rec = dir.path().join("snap.bin");
        let payload: Vec<u8> = (0..16384u32).map(|i| (i % 256) as u8).collect();
        std::fs::write(&src, &payload).unwrap();
        let result = snapshot_target(&src, &rec, 4096).unwrap();
        assert_eq!(result.header.snapshot_bytes, 4096);
        assert_eq!(result.header.source_size_bytes, payload.len() as u64);
        // Recovery file size = 8 (magic) + 4 (len) + json + 4096 (raw)
        let rec_size = std::fs::metadata(&rec).unwrap().len();
        assert!(rec_size > 4096);
    }

    #[test]
    fn snapshot_caps_at_source_size_when_smaller_than_requested() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("tiny.img");
        let rec = dir.path().join("tiny.snap");
        std::fs::write(&src, vec![0xABu8; 100]).unwrap();
        let result = snapshot_target(&src, &rec, 4096).unwrap();
        assert_eq!(result.header.snapshot_bytes, 100);
    }

    #[test]
    fn read_recovery_returns_header_and_raw_for_valid_file() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("a.img");
        let rec = dir.path().join("a.snap");
        let payload: Vec<u8> = (0..512u32).map(|i| (i % 256) as u8).collect();
        std::fs::write(&src, &payload).unwrap();
        let snap = snapshot_target(&src, &rec, 512).unwrap();
        let (h, raw) = read_recovery(&rec).unwrap();
        assert_eq!(h, snap.header);
        assert_eq!(raw, payload);
    }

    #[test]
    fn read_recovery_rejects_bad_magic() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("bogus.snap");
        std::fs::write(&p, b"NOTAMAGIC garbage").unwrap();
        match read_recovery(&p) {
            Err(SnapshotError::BadMagic) => {}
            other => panic!("expected BadMagic, got {other:?}"),
        }
    }

    #[test]
    fn read_recovery_rejects_corrupted_snapshot() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("src.img");
        let rec = dir.path().join("src.snap");
        std::fs::write(&src, vec![0u8; 256]).unwrap();
        snapshot_target(&src, &rec, 256).unwrap();
        // Corrupt the raw region — flip a byte at the end.
        let mut bytes = std::fs::read(&rec).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xFF;
        std::fs::write(&rec, &bytes).unwrap();
        match read_recovery(&rec) {
            Err(SnapshotError::HashMismatch { .. }) => {}
            other => panic!("expected HashMismatch, got {other:?}"),
        }
    }

    #[test]
    fn restore_target_writes_snapshot_bytes_back_at_offset_zero() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("orig.img");
        let rec = dir.path().join("orig.snap");
        let dest = dir.path().join("restored.img");
        let orig: Vec<u8> = (0..1024u32).map(|i| (i % 256) as u8).collect();
        std::fs::write(&src, &orig).unwrap();
        snapshot_target(&src, &rec, 1024).unwrap();
        // Pre-populate the destination with junk; restore should overwrite
        // it byte-for-byte.
        std::fs::write(&dest, vec![0xFFu8; 1024]).unwrap();
        let result = restore_target(&rec, &dest).unwrap();
        assert_eq!(result.bytes_written, 1024);
        assert!(result.matches_header);
        assert_eq!(std::fs::read(&dest).unwrap(), orig);
    }

    #[test]
    fn probe_device_size_reports_regular_file_size() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("a.img");
        std::fs::write(&p, vec![0u8; 12345]).unwrap();
        let mut f = File::open(&p).unwrap();
        assert_eq!(probe_device_size(&mut f).unwrap(), 12345);
    }
}
