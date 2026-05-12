use std::io::{Read, Result};
use std::path::Path;

use fs_core::BlockReadStreamer;
use qcow2::Qcow2Reader;

use super::{ImageInfo, ImageReader, ImageReaderFactory};

pub struct Qcow2ReaderFactory;

/// Probe by extension first (cheap), then trust the upstream parser on open.
/// `Qcow2Reader::open` parses the header + L1 table without scanning clusters,
/// so an inspect-only call is still cheap.
impl ImageReaderFactory for Qcow2ReaderFactory {
    fn name(&self) -> &'static str {
        "qcow2"
    }

    fn probe(&self, path: &Path) -> Option<ImageInfo> {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|s| s.to_ascii_lowercase());
        let by_ext = matches!(ext.as_deref(), Some("qcow2") | Some("qcow"));
        let by_magic = super::magic::is_qcow2(&super::magic::read_head(path, 4));
        if !by_ext && !by_magic {
            return None;
        }
        let source = std::fs::metadata(path).ok()?.len();
        let reader = Qcow2Reader::open(path).ok()?;
        let virtual_size = reader.virtual_size();
        let label = format!(
            "QCOW2 v{} ({} cluster)",
            reader.version(),
            format_cluster(reader.cluster_size()),
        );
        Some(ImageInfo {
            path: path.to_path_buf(),
            format_label: label,
            source_bytes: source,
            uncompressed_bytes: virtual_size,
        })
    }

    fn open(&self, path: &Path) -> Result<Box<dyn ImageReader>> {
        let info = self
            .probe(path)
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "not a qcow2"))?;
        let reader = Qcow2Reader::open(path).map_err(qcow2_err_to_io)?;
        Ok(Box::new(Qcow2ImageReader {
            info,
            stream: BlockReadStreamer::new(reader),
        }))
    }
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

fn qcow2_err_to_io(e: qcow2::Error) -> std::io::Error {
    std::io::Error::other(format!("qcow2: {e:?}"))
}

pub struct Qcow2ImageReader {
    info: ImageInfo,
    stream: BlockReadStreamer<Qcow2Reader>,
}

impl Read for Qcow2ImageReader {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        self.stream.read(buf)
    }
}

impl ImageReader for Qcow2ImageReader {
    fn info(&self) -> &ImageInfo {
        &self.info
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn probe_rejects_non_qcow2_extension() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("file.iso");
        std::fs::write(&p, b"data").unwrap();
        assert!(Qcow2ReaderFactory.probe(&p).is_none());
    }

    #[test]
    fn probe_returns_none_for_qcow2_extension_on_garbage_payload() {
        // Extension matches but content isn't a valid qcow2 — Qcow2Reader::open
        // refuses, so probe() returns None. Guarantees we never claim to handle
        // an image we can't actually parse.
        let dir = tempdir().unwrap();
        let p = dir.path().join("fake.qcow2");
        std::fs::write(&p, b"not really a qcow2 header").unwrap();
        assert!(Qcow2ReaderFactory.probe(&p).is_none());
    }

    #[test]
    fn probe_returns_none_for_missing_file() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("ghost.qcow2");
        assert!(Qcow2ReaderFactory.probe(&p).is_none());
    }

    #[test]
    fn format_cluster_picks_unit() {
        assert_eq!(format_cluster(512), "512 B");
        assert_eq!(format_cluster(64 * 1024), "64 KiB");
        assert_eq!(format_cluster(2 * 1024 * 1024), "2 MiB");
    }

    /// End-to-end against a real qcow2 file produced by `qemu-img`. Skipped
    /// when `qemu-img` isn't on PATH so the test suite stays portable.
    #[test]
    fn open_streams_virtual_disk_when_qemu_img_available() {
        use std::process::Command;
        if Command::new("qemu-img").arg("--version").output().is_err() {
            return;
        }
        let dir = tempdir().unwrap();
        let p = dir.path().join("tiny.qcow2");
        let status = Command::new("qemu-img")
            .args(["create", "-f", "qcow2", p.to_str().unwrap(), "1M"])
            .status()
            .unwrap();
        assert!(status.success());
        let info = Qcow2ReaderFactory.probe(&p).expect("probe");
        assert_eq!(info.uncompressed_bytes, 1024 * 1024);
        assert!(info.format_label.starts_with("QCOW2 v"));
        let mut r = Qcow2ReaderFactory.open(&p).unwrap();
        let mut out = Vec::new();
        r.read_to_end(&mut out).unwrap();
        assert_eq!(out.len() as u64, 1024 * 1024);
        // Freshly-created qcow2 is sparse — all zeros.
        assert!(out.iter().all(|&b| b == 0));
    }
}
