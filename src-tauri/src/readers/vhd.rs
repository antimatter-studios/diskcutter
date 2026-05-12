use std::io::{Read, Result};
use std::path::Path;

use fs_core::BlockReadStreamer;
use vhd::VhdReader;

use super::{ImageInfo, ImageReader, ImageReaderFactory};

pub struct VhdReaderFactory;

impl ImageReaderFactory for VhdReaderFactory {
    fn name(&self) -> &'static str {
        "vhd"
    }

    fn probe(&self, path: &Path) -> Option<ImageInfo> {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|s| s.to_ascii_lowercase());
        let by_ext = ext.as_deref() == Some("vhd");
        let by_magic = super::magic::is_vhd_footer(&super::magic::read_tail(path, 512));
        if !by_ext && !by_magic {
            return None;
        }
        let source = std::fs::metadata(path).ok()?.len();
        let reader = VhdReader::open(path).ok()?;
        let virtual_size = reader.virtual_size();
        Some(ImageInfo {
            path: path.to_path_buf(),
            format_label: format!("VHD ({:.1} GB virtual)", virtual_size as f64 / 1e9),
            source_bytes: source,
            uncompressed_bytes: virtual_size,
        })
    }

    fn open(&self, path: &Path) -> Result<Box<dyn ImageReader>> {
        let info = self
            .probe(path)
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "not a vhd"))?;
        let reader =
            VhdReader::open(path).map_err(|e| std::io::Error::other(format!("vhd: {e:?}")))?;
        Ok(Box::new(VhdImageReader {
            info,
            stream: BlockReadStreamer::new(reader),
        }))
    }
}

pub struct VhdImageReader {
    info: ImageInfo,
    stream: BlockReadStreamer<VhdReader>,
}

impl Read for VhdImageReader {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        self.stream.read(buf)
    }
}

impl ImageReader for VhdImageReader {
    fn info(&self) -> &ImageInfo {
        &self.info
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn probe_rejects_non_vhd_extension() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("file.iso");
        std::fs::write(&p, b"data").unwrap();
        assert!(VhdReaderFactory.probe(&p).is_none());
    }

    #[test]
    fn probe_returns_none_for_vhd_extension_on_garbage_payload() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("fake.vhd");
        std::fs::write(&p, b"not really a vhd footer").unwrap();
        assert!(VhdReaderFactory.probe(&p).is_none());
    }

    #[test]
    fn probe_returns_none_for_missing_file() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("ghost.vhd");
        assert!(VhdReaderFactory.probe(&p).is_none());
    }

    #[test]
    fn open_streams_virtual_disk_when_qemu_img_available() {
        use std::process::Command;
        if Command::new("qemu-img").arg("--version").output().is_err() {
            return;
        }
        let dir = tempdir().unwrap();
        let p = dir.path().join("tiny.vhd");
        let status = Command::new("qemu-img")
            .args(["create", "-f", "vpc", p.to_str().unwrap(), "2M"])
            .status()
            .unwrap();
        assert!(status.success());
        let info = VhdReaderFactory.probe(&p).expect("probe");
        assert!(info.uncompressed_bytes >= 2 * 1024 * 1024);
        assert!(info.format_label.starts_with("VHD"));
        let mut r = VhdReaderFactory.open(&p).unwrap();
        let mut out = Vec::new();
        r.read_to_end(&mut out).unwrap();
        assert!(out.iter().all(|&b| b == 0));
    }
}
