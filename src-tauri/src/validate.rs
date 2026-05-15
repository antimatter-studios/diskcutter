//! Content validation for queued images — the safety gate that asks
//! "does this file actually contain a burnable disk?" rather than just
//! "does the file extension look right?".
//!
//! The hazard we're guarding against: a `.tar.gz` of photos passes the
//! gzip magic check (so the reader registry accepts it) but burning it
//! produces an unbootable USB. Same story for a `.qcow2` containing an
//! empty disk, or an `.iso` of random bytes.
//!
//! Two checks, in order:
//!
//! 1. **Partition-table probe.** If the device has a recognised MBR or
//!    GPT, we accept it. Covers all partitioned disks.
//! 2. **Whole-device filesystem sniff.** Reads the first ~33 KiB (covers
//!    FAT/NTFS/exFAT BPB at 0, ext/HFS+ superblock at 1024, ISO9660 at
//!    0x8001) and runs `partitions::sniff::classify`. Covers
//!    superfloppies — single-FS images with no partition table.
//!
//! Either check passing → valid. Both miss → invalid. As a softer
//! fallback we also accept "bare boot sector present" (`0x55 0xAA` at
//! offset 510) so we don't reject custom embedded images that the
//! sniffer doesn't recognise.
//!
//! For compressed sources (.gz / .xz / .bz2 / .zst) random-access reads
//! are unavailable, so we only do the filesystem-sniff check on the
//! decompressed prefix.

use std::path::Path;

use fs_core::{BlockRead, FileDevice};
use partitions::{
    probe::probe,
    sniff::{classify, FsKind},
};
use serde::Serialize;
use std::io::Read;

use crate::inspect::format_fs_kind;
use crate::readers::ImageReaderRegistry;

/// Largest offset any signature check needs (ISO 9660 'CD001' at
/// 0x8001 + 5 bytes). Round up to 0x8200 so we always have a small
/// margin past the highest signature.
pub const SNIFF_WINDOW_BYTES: usize = 0x8200;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum ValidationReport {
    /// File looks like a burnable disk image. `detail` is a short
    /// human-readable description ("partition table: GPT (3
    /// partitions)" / "filesystem: ext4" / etc.) for surfacing in the
    /// UI alongside the green check.
    Valid { detail: String },
    /// File does not look like a burnable disk image. `reason` is a
    /// short user-facing explanation ("compressed contents are not a
    /// recognised disk image" / "no partition table or filesystem
    /// signature found" / etc.).
    Invalid { reason: String },
}

impl ValidationReport {
    pub fn is_valid(&self) -> bool {
        matches!(self, ValidationReport::Valid { .. })
    }
}

/// Best-effort content validation. Never panics; opaque I/O failures
/// fold into `Invalid` with a generic reason — the burn surface gets
/// blocked, which is the conservative choice.
pub fn validate(path: &Path) -> ValidationReport {
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase());

    if matches!(
        ext.as_deref(),
        Some("gz") | Some("xz") | Some("bz2") | Some("bzip2") | Some("zst") | Some("zstd")
    ) {
        return validate_compressed(path);
    }

    let dev = match open_block_read(path, ext.as_deref()) {
        Ok(d) => d,
        Err(e) => {
            return ValidationReport::Invalid {
                reason: format!("could not open image: {e}"),
            }
        }
    };
    validate_block(dev.as_ref())
}

/// Validation against an already-opened `BlockRead`. Pure-ish — only
/// reads. Split out so tests can drive it with synthetic devices.
pub fn validate_block(dev: &dyn BlockRead) -> ValidationReport {
    if let Ok((table, parts)) = probe(dev) {
        let table_label = match table {
            partitions::probe::TableKind::Gpt => "GPT",
            partitions::probe::TableKind::Mbr => "MBR",
        };
        return ValidationReport::Valid {
            detail: format!(
                "partition table: {} ({} partition{})",
                table_label,
                parts.len(),
                if parts.len() == 1 { "" } else { "s" }
            ),
        };
    }

    let cap = dev.size_bytes();
    if cap == 0 {
        return ValidationReport::Invalid {
            reason: "image is empty".into(),
        };
    }
    let n = std::cmp::min(SNIFF_WINDOW_BYTES as u64, cap) as usize;
    let mut buf = vec![0u8; n];
    if let Err(e) = dev.read_at(0, &mut buf) {
        return ValidationReport::Invalid {
            reason: format!("could not read image prefix: {e:?}"),
        };
    }
    classify_buffer(&buf)
}

fn validate_compressed(path: &Path) -> ValidationReport {
    let registry = ImageReaderRegistry::with_defaults();
    let factory = match registry.probe(path) {
        Some((_, f)) => f,
        None => {
            return ValidationReport::Invalid {
                reason: "compressed format not recognised".into(),
            }
        }
    };
    let mut reader = match factory.open(path) {
        Ok(r) => r,
        Err(e) => {
            return ValidationReport::Invalid {
                reason: format!("could not open compressed image: {e}"),
            }
        }
    };
    let mut buf = vec![0u8; SNIFF_WINDOW_BYTES];
    let mut filled = 0usize;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) => {
                return ValidationReport::Invalid {
                    reason: format!("decompression failed: {e}"),
                }
            }
        }
    }
    buf.truncate(filled);
    if buf.is_empty() {
        return ValidationReport::Invalid {
            reason: "decompressed stream is empty".into(),
        };
    }
    match classify_buffer(&buf) {
        v @ ValidationReport::Valid { .. } => v,
        ValidationReport::Invalid { .. } => ValidationReport::Invalid {
            reason: "compressed contents are not a recognised disk image".into(),
        },
    }
}

pub(crate) fn classify_buffer(buf: &[u8]) -> ValidationReport {
    let kind = classify(buf);
    if !matches!(kind, FsKind::Unknown) {
        return ValidationReport::Valid {
            detail: format!("filesystem: {}", format_fs_kind(kind)),
        };
    }
    if buf.len() >= 512 && buf[510] == 0x55 && buf[511] == 0xAA {
        return ValidationReport::Valid {
            detail: "boot sector (unrecognised filesystem)".into(),
        };
    }
    ValidationReport::Invalid {
        reason: "no partition table or filesystem signature found".into(),
    }
}

fn open_block_read(path: &Path, ext: Option<&str>) -> std::io::Result<Box<dyn BlockRead>> {
    match ext {
        Some("qcow2") | Some("qcow") => Ok(Box::new(
            qcow2::Qcow2Reader::open(path).map_err(std::io::Error::other)?,
        )),
        Some("vhd") => Ok(Box::new(
            vhd::VhdReader::open(path).map_err(std::io::Error::other)?,
        )),
        Some("vhdx") => Ok(Box::new(
            vhdx::VhdxReader::open(path).map_err(std::io::Error::other)?,
        )),
        Some("vmdk") => Ok(Box::new(
            vmdk::VmdkReader::open(path).map_err(std::io::Error::other)?,
        )),
        _ => Ok(Box::new(
            FileDevice::open(path).map_err(std::io::Error::other)?,
        )),
    }
}

/// Tauri command. Runs validation on a worker thread so the UI keeps
/// responding while xz decompression / large container probes finish,
/// then emits `image-validated { job_id, ... }`. The frontend kicks
/// this off right after `inspect_image` succeeds.
///
/// The command itself returns immediately after spawning the worker.
/// Errors during the spawn surface as a `Result::Err`; errors during
/// validation are folded into `ValidationReport::Invalid` and reach
/// the frontend through the event channel.
#[tauri::command]
pub fn validate_image_contents(
    app: tauri::AppHandle,
    job_id: String,
    path: String,
) -> Result<(), String> {
    use crate::image::DiskImage;
    use tauri::Emitter;
    std::thread::spawn(move || {
        let report = match DiskImage::open(Path::new(&path)) {
            Ok(img) => img.validate(),
            Err(e) => ValidationReport::Invalid {
                reason: e.to_string(),
            },
        };
        #[derive(serde::Serialize, Clone)]
        struct Payload {
            job_id: String,
            #[serde(flatten)]
            report: ValidationReport,
        }
        let _ = app.emit("disk-cutter://image-validated", Payload { job_id, report });
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// Synthetic BlockRead that hands back a fixed byte slice.
    struct MemDev(Vec<u8>);
    impl BlockRead for MemDev {
        fn read_at(&self, offset: u64, buf: &mut [u8]) -> fs_core::Result<()> {
            let off = offset as usize;
            let end = off + buf.len();
            if end > self.0.len() {
                return Err(fs_core::Error::OutOfBounds {
                    offset,
                    len: buf.len() as u64,
                    size: self.0.len() as u64,
                });
            }
            buf.copy_from_slice(&self.0[off..end]);
            Ok(())
        }
        fn size_bytes(&self) -> u64 {
            self.0.len() as u64
        }
    }

    fn fat32_buffer() -> Vec<u8> {
        let mut buf = vec![0u8; 0x8200];
        // 0x55 0xAA boot signature at 510/511.
        buf[510] = 0x55;
        buf[511] = 0xAA;
        // FAT32 tag at offset 0x52.
        buf[0x52..0x5A].copy_from_slice(b"FAT32   ");
        buf
    }

    fn ext4_buffer() -> Vec<u8> {
        let mut buf = vec![0u8; 0x8200];
        // ext superblock magic 0xEF53 at offset 1080.
        buf[1080] = 0x53;
        buf[1081] = 0xEF;
        // EXTENTS bit in s_feature_incompat → ext4.
        buf[1024 + 0x60] = 0x40;
        buf
    }

    fn mbr_only_buffer() -> Vec<u8> {
        // 0x55AA boot signature, no FS tag, no ext magic.
        let mut buf = vec![0u8; 0x8200];
        buf[510] = 0x55;
        buf[511] = 0xAA;
        buf
    }

    #[test]
    fn validate_block_accepts_fat32_superfloppy() {
        let dev = MemDev(fat32_buffer());
        assert!(validate_block(&dev).is_valid());
    }

    #[test]
    fn validate_block_accepts_ext4_superfloppy() {
        let dev = MemDev(ext4_buffer());
        let r = validate_block(&dev);
        assert!(r.is_valid(), "expected valid, got {r:?}");
    }

    #[test]
    fn validate_block_accepts_bare_boot_sector_as_softer_match() {
        let dev = MemDev(mbr_only_buffer());
        let r = validate_block(&dev);
        assert!(
            r.is_valid(),
            "expected valid (boot-sector fallback), got {r:?}"
        );
    }

    #[test]
    fn validate_block_rejects_random_bytes() {
        let mut buf = vec![0u8; 0x8200];
        // Random-ish bytes — no signatures.
        for (i, b) in buf.iter_mut().enumerate() {
            *b = ((i * 31) & 0xFF) as u8;
        }
        // Make sure offset 510/511 are NOT 0x55 0xAA.
        buf[510] = 0x00;
        buf[511] = 0x00;
        let dev = MemDev(buf);
        let r = validate_block(&dev);
        assert!(!r.is_valid(), "expected invalid, got {r:?}");
    }

    #[test]
    fn validate_block_rejects_empty_device() {
        let dev = MemDev(Vec::new());
        let r = validate_block(&dev);
        assert!(!r.is_valid());
    }

    #[test]
    fn validate_rejects_tarball_disguised_as_iso() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("not-a-disk.iso");
        std::fs::write(&p, b"hello world, not a disk image at all").unwrap();
        let r = validate(&p);
        assert!(
            !r.is_valid(),
            "expected invalid for garbage .iso, got {r:?}"
        );
    }

    #[test]
    fn validate_accepts_raw_fat32_image() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("super.img");
        std::fs::write(&p, fat32_buffer()).unwrap();
        let r = validate(&p);
        assert!(r.is_valid(), "expected valid, got {r:?}");
    }

    #[test]
    fn validate_rejects_gzipped_garbage() {
        use flate2::write::GzEncoder;
        use flate2::Compression;
        use std::io::Write;
        let dir = tempdir().unwrap();
        let p = dir.path().join("nope.iso.gz");
        let mut e = GzEncoder::new(Vec::new(), Compression::default());
        e.write_all(b"a bunch of text, not a disk image").unwrap();
        let body = e.finish().unwrap();
        std::fs::write(&p, body).unwrap();
        let r = validate(&p);
        assert!(!r.is_valid(), "expected invalid for garbage .gz, got {r:?}");
    }

    #[test]
    fn validate_accepts_gzipped_fat32() {
        use flate2::write::GzEncoder;
        use flate2::Compression;
        use std::io::Write;
        let dir = tempdir().unwrap();
        let p = dir.path().join("ok.img.gz");
        let mut e = GzEncoder::new(Vec::new(), Compression::default());
        e.write_all(&fat32_buffer()).unwrap();
        let body = e.finish().unwrap();
        std::fs::write(&p, body).unwrap();
        let r = validate(&p);
        assert!(r.is_valid(), "expected valid, got {r:?}");
    }
}
