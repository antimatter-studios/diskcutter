use std::fs::File;
use std::io::{Read, Result};
use std::path::Path;

use super::{ImageInfo, ImageReader, ImageReaderFactory};

pub struct RawReaderFactory;

fn label_for_extension(ext: Option<&str>) -> Option<&'static str> {
    match ext {
        Some("iso") => Some("ISO 9660 / RAW"),
        Some("img") | Some("bin") | Some("raw") => Some("RAW DISK IMAGE"),
        _ => None,
    }
}

impl ImageReaderFactory for RawReaderFactory {
    fn name(&self) -> &'static str {
        "raw"
    }

    fn probe(&self, path: &Path) -> Option<ImageInfo> {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|s| s.to_ascii_lowercase());
        let format_label = label_for_extension(ext.as_deref())?.to_string();
        let meta = std::fs::metadata(path).ok()?;
        let bytes = meta.len();
        Some(ImageInfo {
            path: path.to_path_buf(),
            format_label,
            source_bytes: bytes,
            uncompressed_bytes: bytes,
        })
    }

    fn open(&self, path: &Path) -> Result<Box<dyn ImageReader>> {
        let info = self
            .probe(path)
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "unsupported"))?;
        let file = File::open(path)?;
        Ok(Box::new(RawReader::new(info, Box::new(file))))
    }
}

pub struct RawReader {
    info: ImageInfo,
    inner: Box<dyn Read + Send>,
}

impl RawReader {
    pub fn new(info: ImageInfo, inner: Box<dyn Read + Send>) -> Self {
        Self { info, inner }
    }
}

impl Read for RawReader {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        self.inner.read(buf)
    }
}

impl ImageReader for RawReader {
    fn info(&self) -> &ImageInfo {
        &self.info
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::path::PathBuf;
    use tempfile::tempdir;

    #[test]
    fn probe_iso_returns_iso_label() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("boot.iso");
        std::fs::write(&p, b"data").unwrap();
        let info = RawReaderFactory.probe(&p).unwrap();
        assert_eq!(info.format_label, "ISO 9660 / RAW");
        assert_eq!(info.source_bytes, 4);
        assert_eq!(info.uncompressed_bytes, 4);
    }

    #[test]
    fn probe_img_bin_raw_return_raw_disk_label() {
        let dir = tempdir().unwrap();
        for ext in ["img", "bin", "raw"] {
            let p = dir.path().join(format!("x.{ext}"));
            std::fs::write(&p, b"data").unwrap();
            let info = RawReaderFactory.probe(&p).expect(ext);
            assert_eq!(info.format_label, "RAW DISK IMAGE", "ext={ext}");
        }
    }

    #[test]
    fn probe_unknown_extension_returns_none() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("x.unknownext");
        std::fs::write(&p, b"data").unwrap();
        assert!(RawReaderFactory.probe(&p).is_none());
    }

    #[test]
    fn label_for_extension_maps_known_types() {
        assert_eq!(label_for_extension(Some("iso")), Some("ISO 9660 / RAW"));
        assert_eq!(label_for_extension(Some("img")), Some("RAW DISK IMAGE"));
        assert_eq!(label_for_extension(Some("bin")), Some("RAW DISK IMAGE"));
        assert_eq!(label_for_extension(Some("raw")), Some("RAW DISK IMAGE"));
    }

    #[test]
    fn label_for_extension_returns_none_for_unknown() {
        assert_eq!(label_for_extension(Some("txt")), None);
        assert_eq!(label_for_extension(Some("vhd")), None);
        assert_eq!(label_for_extension(Some("")), None);
        assert_eq!(label_for_extension(None), None);
    }

    #[test]
    fn label_for_extension_is_case_sensitive_against_lowercased_input() {
        // probe() pre-lowercases; the mapper itself sees only lowercase.
        assert_eq!(label_for_extension(Some("ISO")), None);
        assert_eq!(label_for_extension(Some("Img")), None);
    }

    #[test]
    fn probe_missing_file_returns_none() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("ghost.iso");
        assert!(RawReaderFactory.probe(&p).is_none());
    }

    #[test]
    fn open_reads_file_contents() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("data.img");
        std::fs::write(&p, b"hello").unwrap();
        let mut reader = RawReaderFactory.open(&p).unwrap();
        let mut out = Vec::new();
        Read::read_to_end(&mut reader, &mut out).unwrap();
        assert_eq!(out, b"hello");
        assert_eq!(reader.info().source_bytes, 5);
    }

    #[test]
    fn raw_reader_exposes_provided_info_and_forwards_reads() {
        let info = ImageInfo {
            path: PathBuf::from("/x"),
            format_label: "L".into(),
            source_bytes: 4,
            uncompressed_bytes: 4,
        };
        let mut r = RawReader::new(info, Box::new(Cursor::new(b"abcd".to_vec())));
        assert_eq!(r.info().source_bytes, 4);
        let mut out = Vec::new();
        Read::read_to_end(&mut r, &mut out).unwrap();
        assert_eq!(out, b"abcd");
    }
}
