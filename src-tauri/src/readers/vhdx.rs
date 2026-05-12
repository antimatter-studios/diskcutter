use std::io::{Read, Result};
use std::path::Path;

use fs_core::BlockReadStreamer;
use vhdx::VhdxReader;

use super::{ImageInfo, ImageReader, ImageReaderFactory};

pub struct VhdxReaderFactory;

impl ImageReaderFactory for VhdxReaderFactory {
    fn name(&self) -> &'static str {
        "vhdx"
    }

    fn probe(&self, path: &Path) -> Option<ImageInfo> {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|s| s.to_ascii_lowercase());
        if ext.as_deref() != Some("vhdx") {
            return None;
        }
        let source = std::fs::metadata(path).ok()?.len();
        let reader = VhdxReader::open(path).ok()?;
        let virtual_size = reader.virtual_size();
        Some(ImageInfo {
            path: path.to_path_buf(),
            format_label: format!("VHDX ({:.1} GB virtual)", virtual_size as f64 / 1e9),
            source_bytes: source,
            uncompressed_bytes: virtual_size,
        })
    }

    fn open(&self, path: &Path) -> Result<Box<dyn ImageReader>> {
        let info = self
            .probe(path)
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "not a vhdx"))?;
        let reader =
            VhdxReader::open(path).map_err(|e| std::io::Error::other(format!("vhdx: {e:?}")))?;
        Ok(Box::new(VhdxImageReader {
            info,
            stream: BlockReadStreamer::new(reader),
        }))
    }
}

pub struct VhdxImageReader {
    info: ImageInfo,
    stream: BlockReadStreamer<VhdxReader>,
}

impl Read for VhdxImageReader {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        self.stream.read(buf)
    }
}

impl ImageReader for VhdxImageReader {
    fn info(&self) -> &ImageInfo {
        &self.info
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn probe_rejects_non_vhdx_extension() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("file.iso");
        std::fs::write(&p, b"data").unwrap();
        assert!(VhdxReaderFactory.probe(&p).is_none());
    }

    #[test]
    fn probe_returns_none_for_vhdx_extension_on_garbage_payload() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("fake.vhdx");
        std::fs::write(&p, b"not really a vhdx signature").unwrap();
        assert!(VhdxReaderFactory.probe(&p).is_none());
    }

    #[test]
    fn probe_returns_none_for_missing_file() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("ghost.vhdx");
        assert!(VhdxReaderFactory.probe(&p).is_none());
    }

    #[test]
    fn open_streams_virtual_disk_when_qemu_img_available() {
        use std::process::Command;
        if Command::new("qemu-img").arg("--version").output().is_err() {
            return;
        }
        let dir = tempdir().unwrap();
        let p = dir.path().join("tiny.vhdx");
        let status = Command::new("qemu-img")
            .args(["create", "-f", "vhdx", p.to_str().unwrap(), "2M"])
            .status()
            .unwrap();
        assert!(status.success());
        let info = VhdxReaderFactory.probe(&p).expect("probe");
        assert_eq!(info.uncompressed_bytes, 2 * 1024 * 1024);
        assert!(info.format_label.starts_with("VHDX"));
        let mut r = VhdxReaderFactory.open(&p).unwrap();
        let mut out = Vec::new();
        r.read_to_end(&mut out).unwrap();
        assert_eq!(out.len() as u64, 2 * 1024 * 1024);
        assert!(out.iter().all(|&b| b == 0));
    }
}
