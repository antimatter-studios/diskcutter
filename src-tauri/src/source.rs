//! Image-source dispatch: a single front door for "give me a streaming
//! reader for this file".
//!
//! Two flavours of image co-exist in the wild:
//!
//! 1. **Compressed or raw** — xz / gz / bz2 / zstd / plain image. These
//!    are streaming-only sources; the decoder chain (`DiskReader`) peels
//!    compression layers one-by-one and yields raw bytes. The bounded-
//!    prefix `BlockRead` view is enough to answer partition probes.
//!
//! 2. **Container** — qcow2 / vhd / vhdx / vmdk. These store a virtual
//!    disk in a metadata-rich on-disk layout that requires random access
//!    to traverse. The upstream crates already expose `BlockRead`, so we
//!    wrap them in `BlockReadStreamer` for the streaming Read interface
//!    the burn pipeline wants.
//!
//! Callers shouldn't care which flavour they have. `probe()` returns the
//! shared `SourceInfo`, and `open_streaming()` returns a Read paired with
//! that info. Dispatch is by extension first, magic second (for the
//! "user renamed `foo.iso.xz` to `foo.iso`" footgun).

use std::io::{self, Read};
use std::path::{Path, PathBuf};

use fs_core::BlockReadStreamer;

use crate::decoder_chain::DiskReader;
use crate::joblog::{JobLogger, NullLogger};
use crate::readers::magic;

/// Metadata about an image source that callers want before/while
/// streaming bytes.
#[derive(Clone, Debug)]
pub struct SourceInfo {
    pub path: PathBuf,
    /// Human-readable label for the format chain, e.g. `"RAW"`,
    /// `"XZ"`, `"QCOW2 v3 (64 KiB cluster)"`. Surfaces directly in the
    /// inspect UI's "Format" row.
    pub format_label: String,
    /// File size on disk.
    pub source_bytes: u64,
    /// Logical / uncompressed size. For containers this is the virtual
    /// disk size; for xz it's the parsed footer index sum; for other
    /// compression formats this falls back to `source_bytes` because the
    /// true uncompressed size isn't recoverable without a full scan.
    pub uncompressed_bytes: u64,
}

/// Probe a path. Returns `None` for unrecognised extensions / unreadable
/// files. Streaming is NOT opened here — this is the cheap inspect path.
pub fn probe(path: &Path) -> Option<SourceInfo> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase());
    let meta = std::fs::metadata(path).ok()?;
    let source_bytes = meta.len();

    match ext.as_deref() {
        Some("qcow2") | Some("qcow") => probe_qcow2(path, source_bytes),
        Some("vhd") => probe_vhd(path, source_bytes),
        Some("vhdx") => probe_vhdx(path, source_bytes),
        Some("vmdk") => probe_vmdk(path, source_bytes),
        Some("xz") | Some("gz") | Some("gzip") | Some("bz2") | Some("bzip2") | Some("zst")
        | Some("zstd") => probe_compressed(path, ext.as_deref(), source_bytes),
        Some("iso") | Some("img") | Some("bin") | Some("raw") => {
            // Could secretly be a compressed source renamed to .iso etc.
            // — defer to magic-by-head.
            if let Some(info) = probe_by_magic(path, source_bytes, ext.as_deref()) {
                return Some(info);
            }
            Some(SourceInfo {
                path: path.to_path_buf(),
                format_label: raw_label_for_ext(ext.as_deref()).to_string(),
                source_bytes,
                uncompressed_bytes: source_bytes,
            })
        }
        _ => {
            // Unknown extension. Last-ditch: magic sniff. Catches a
            // qcow2/vhdx renamed without an extension, or compression
            // hidden under e.g. `.dat`.
            probe_by_magic(path, source_bytes, ext.as_deref())
        }
    }
}

/// Open a streaming reader for `path`. The returned reader yields raw
/// disk-image bytes regardless of the underlying format; the paired
/// `SourceInfo` tells the caller what they're dealing with for display.
///
/// No per-item logging — see `open_streaming_with_log` for the burn path.
pub fn open_streaming(path: &Path) -> io::Result<(Box<dyn Read + Send>, SourceInfo)> {
    open_streaming_with_log(path, &NullLogger)
}

/// Like `open_streaming` but emits debug/info entries into the per-item
/// log via `log`. Used by the burn path so a row's log captures the
/// decoder chain's behaviour for that specific image.
pub fn open_streaming_with_log(
    path: &Path,
    log: &dyn JobLogger,
) -> io::Result<(Box<dyn Read + Send>, SourceInfo)> {
    let info = probe(path)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "unsupported image format"))?;
    log.info(&format!(
        "source: probed {} as {} ({} bytes on disk, {} bytes uncompressed)",
        path.display(),
        info.format_label,
        info.source_bytes,
        info.uncompressed_bytes,
    ));
    let reader: Box<dyn Read + Send> = match classify(&info.format_label) {
        Family::Qcow2 => {
            log.debug("source: family=qcow2, routing to Qcow2Reader (random-access)");
            let r = qcow2::Qcow2Reader::open(path)
                .map_err(|e| io::Error::other(format!("qcow2: {e:?}")))?;
            Box::new(BlockReadStreamer::new(r))
        }
        Family::Vhd => {
            log.debug("source: family=vhd, routing to VhdReader (random-access)");
            let r =
                vhd::VhdReader::open(path).map_err(|e| io::Error::other(format!("vhd: {e:?}")))?;
            Box::new(BlockReadStreamer::new(r))
        }
        Family::Vhdx => {
            log.debug("source: family=vhdx, routing to VhdxReader (random-access)");
            let r = vhdx::VhdxReader::open(path)
                .map_err(|e| io::Error::other(format!("vhdx: {e:?}")))?;
            Box::new(BlockReadStreamer::new(r))
        }
        Family::Vmdk => {
            log.debug("source: family=vmdk, routing to VmdkReader (random-access)");
            let r = vmdk::VmdkReader::open(path)
                .map_err(|e| io::Error::other(format!("vmdk: {e:?}")))?;
            Box::new(BlockReadStreamer::new(r))
        }
        Family::Streaming => {
            log.debug("source: family=streaming, routing to DiskReader (decoder chain)");
            Box::new(DiskReader::open_with_log(path, log)?)
        }
    };
    Ok((reader, info))
}

/// Classify a format label back to its container family. Container
/// labels begin with `QCOW2` / `VHD` / `VHDX` / `VMDK` (and only those —
/// see the probe helpers). Everything else flows through the decoder
/// chain.
enum Family {
    Qcow2,
    Vhd,
    Vhdx,
    Vmdk,
    Streaming,
}

fn classify(label: &str) -> Family {
    if label.starts_with("QCOW2") {
        Family::Qcow2
    } else if label.starts_with("VHDX") {
        Family::Vhdx
    } else if label.starts_with("VHD") {
        Family::Vhd
    } else if label.starts_with("VMDK") {
        Family::Vmdk
    } else {
        Family::Streaming
    }
}

fn raw_label_for_ext(ext: Option<&str>) -> &'static str {
    match ext {
        Some("iso") => "ISO 9660",
        Some("img") | Some("bin") | Some("raw") => "RAW",
        _ => "RAW",
    }
}

fn probe_qcow2(path: &Path, source_bytes: u64) -> Option<SourceInfo> {
    let reader = qcow2::Qcow2Reader::open(path).ok()?;
    let label = format!(
        "QCOW2 v{} ({} cluster)",
        reader.version(),
        format_cluster(reader.cluster_size()),
    );
    Some(SourceInfo {
        path: path.to_path_buf(),
        format_label: label,
        source_bytes,
        uncompressed_bytes: reader.virtual_size(),
    })
}

fn probe_vhd(path: &Path, source_bytes: u64) -> Option<SourceInfo> {
    let reader = vhd::VhdReader::open(path).ok()?;
    let vsize = reader.virtual_size();
    Some(SourceInfo {
        path: path.to_path_buf(),
        format_label: format!("VHD ({:.1} GB virtual)", vsize as f64 / 1e9),
        source_bytes,
        uncompressed_bytes: vsize,
    })
}

fn probe_vhdx(path: &Path, source_bytes: u64) -> Option<SourceInfo> {
    let reader = vhdx::VhdxReader::open(path).ok()?;
    let vsize = reader.virtual_size();
    Some(SourceInfo {
        path: path.to_path_buf(),
        format_label: format!("VHDX ({:.1} GB virtual)", vsize as f64 / 1e9),
        source_bytes,
        uncompressed_bytes: vsize,
    })
}

fn probe_vmdk(path: &Path, source_bytes: u64) -> Option<SourceInfo> {
    let reader = vmdk::VmdkReader::open(path).ok()?;
    let vsize = reader.virtual_size();
    Some(SourceInfo {
        path: path.to_path_buf(),
        format_label: format!("VMDK ({:.1} GB virtual)", vsize as f64 / 1e9),
        source_bytes,
        uncompressed_bytes: vsize,
    })
}

fn probe_compressed(path: &Path, ext: Option<&str>, source_bytes: u64) -> Option<SourceInfo> {
    let label = compressed_label_for_ext(ext);
    // xz carries total uncompressed size in its stream footer — recoverable
    // without scanning the body. Other formats (gzip's mtime-only footer,
    // bzip2's blocked stream, zstd's optional content-size) fall back to
    // source_bytes for the queue size column.
    let uncompressed = if ext == Some("xz") {
        crate::xz_footer::read_total_uncompressed(path).unwrap_or(source_bytes)
    } else {
        source_bytes
    };
    Some(SourceInfo {
        path: path.to_path_buf(),
        format_label: label.to_string(),
        source_bytes,
        uncompressed_bytes: uncompressed,
    })
}

fn compressed_label_for_ext(ext: Option<&str>) -> &'static str {
    match ext {
        Some("xz") => "XZ",
        Some("gz") | Some("gzip") => "GZIP",
        Some("bz2") | Some("bzip2") => "BZIP2",
        Some("zst") | Some("zstd") => "ZSTD",
        _ => "COMPRESSED",
    }
}

fn probe_by_magic(path: &Path, source_bytes: u64, ext: Option<&str>) -> Option<SourceInfo> {
    let head = magic::read_head(path, 16);
    if magic::is_qcow2(&head) {
        return probe_qcow2(path, source_bytes);
    }
    if magic::is_vhdx(&head) {
        return probe_vhdx(path, source_bytes);
    }
    if magic::is_vmdk(&head) {
        return probe_vmdk(path, source_bytes);
    }
    if magic::is_xz(&head) {
        return probe_compressed(path, Some("xz"), source_bytes);
    }
    if magic::is_gzip(&head) {
        return probe_compressed(path, Some("gz"), source_bytes);
    }
    if magic::is_bzip2(&head) {
        return probe_compressed(path, Some("bz2"), source_bytes);
    }
    if magic::is_zstd(&head) {
        return probe_compressed(path, Some("zst"), source_bytes);
    }
    let tail = magic::read_tail(path, 512);
    if magic::is_vhd_footer(&tail) {
        return probe_vhd(path, source_bytes);
    }
    let _ = ext;
    None
}

fn format_cluster(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{} MiB", bytes / (1024 * 1024))
    } else if bytes >= 1024 {
        format!("{} KiB", bytes / 1024)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::tempdir;

    #[test]
    fn probe_iso_returns_iso_label() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("boot.iso");
        std::fs::write(&p, b"data").unwrap();
        let info = probe(&p).unwrap();
        assert_eq!(info.format_label, "ISO 9660");
        assert_eq!(info.source_bytes, 4);
        assert_eq!(info.uncompressed_bytes, 4);
    }

    #[test]
    fn probe_img_returns_raw_label() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("disk.img");
        std::fs::write(&p, b"data").unwrap();
        let info = probe(&p).unwrap();
        assert_eq!(info.format_label, "RAW");
    }

    #[test]
    fn probe_gzipped_image_returns_gzip_label() {
        use flate2::write::GzEncoder;
        use flate2::Compression;
        let dir = tempdir().unwrap();
        let p = dir.path().join("blob.img.gz");
        let mut e = GzEncoder::new(Vec::new(), Compression::default());
        e.write_all(b"hello").unwrap();
        std::fs::write(&p, e.finish().unwrap()).unwrap();
        let info = probe(&p).unwrap();
        assert_eq!(info.format_label, "GZIP");
    }

    #[test]
    fn probe_renamed_xz_finds_format_via_magic() {
        use xz2::write::XzEncoder;
        let dir = tempdir().unwrap();
        let p = dir.path().join("disguised.iso");
        let mut e = XzEncoder::new(Vec::new(), 1);
        e.write_all(b"raw bytes").unwrap();
        std::fs::write(&p, e.finish().unwrap()).unwrap();
        let info = probe(&p).expect("magic should match xz");
        assert_eq!(info.format_label, "XZ");
    }

    #[test]
    fn probe_unknown_extension_returns_none() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("file.unknownext");
        std::fs::write(&p, b"garbage").unwrap();
        assert!(probe(&p).is_none());
    }

    #[test]
    fn probe_missing_file_returns_none() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("ghost.iso");
        assert!(probe(&p).is_none());
    }

    #[test]
    fn open_streaming_round_trips_raw_image() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("plain.img");
        let payload: Vec<u8> = (0..2048u32).map(|i| (i % 256) as u8).collect();
        std::fs::write(&p, &payload).unwrap();
        let (mut r, info) = open_streaming(&p).unwrap();
        let mut out = Vec::new();
        r.read_to_end(&mut out).unwrap();
        assert_eq!(out, payload);
        assert_eq!(info.uncompressed_bytes, payload.len() as u64);
    }

    #[test]
    fn open_streaming_decompresses_gzip() {
        use flate2::write::GzEncoder;
        use flate2::Compression;
        let dir = tempdir().unwrap();
        let p = dir.path().join("payload.img.gz");
        let payload: Vec<u8> = (0..4096u32).map(|i| (i % 256) as u8).collect();
        let mut e = GzEncoder::new(Vec::new(), Compression::default());
        e.write_all(&payload).unwrap();
        std::fs::write(&p, e.finish().unwrap()).unwrap();
        let (mut r, _info) = open_streaming(&p).unwrap();
        let mut out = Vec::new();
        r.read_to_end(&mut out).unwrap();
        assert_eq!(out, payload);
    }
}
