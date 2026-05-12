use std::io::Read;
use std::path::{Path, PathBuf};

mod gzip;
mod qcow2;
mod raw;
mod vhd;
mod vhdx;
mod vmdk;
mod xz;
pub use gzip::GzipReaderFactory;
pub use qcow2::Qcow2ReaderFactory;
pub use raw::RawReaderFactory;
pub use vhd::VhdReaderFactory;
pub use vhdx::VhdxReaderFactory;
pub use vmdk::VmdkReaderFactory;
pub use xz::XzReaderFactory;

#[derive(Clone, Debug, serde::Serialize)]
pub struct ImageInfo {
    pub path: PathBuf,
    pub format_label: String,
    pub source_bytes: u64,
    pub uncompressed_bytes: u64,
}

pub trait ImageReader: Read + Send {
    fn info(&self) -> &ImageInfo;
}

pub trait ImageReaderFactory: Send + Sync {
    #[allow(dead_code)]
    fn name(&self) -> &'static str;
    fn probe(&self, path: &Path) -> Option<ImageInfo>;
    fn open(&self, path: &Path) -> std::io::Result<Box<dyn ImageReader>>;
}

pub struct ImageReaderRegistry {
    factories: Vec<Box<dyn ImageReaderFactory>>,
}

impl ImageReaderRegistry {
    pub fn with_defaults() -> Self {
        Self {
            factories: vec![
                Box::new(Qcow2ReaderFactory),
                Box::new(VhdReaderFactory),
                Box::new(VhdxReaderFactory),
                Box::new(VmdkReaderFactory),
                Box::new(GzipReaderFactory),
                Box::new(XzReaderFactory),
                Box::new(RawReaderFactory),
            ],
        }
    }

    pub fn probe(&self, path: &Path) -> Option<(ImageInfo, &dyn ImageReaderFactory)> {
        self.factories
            .iter()
            .find_map(|f| f.probe(path).map(|info| (info, f.as_ref())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn registry_with_defaults_probes_iso_extension() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("disc.iso");
        std::fs::write(&p, b"data").unwrap();
        let reg = ImageReaderRegistry::with_defaults();
        let (info, _f) = reg.probe(&p).expect("probe");
        assert_eq!(info.format_label, "ISO 9660 / RAW");
        assert_eq!(info.source_bytes, 4);
    }

    #[test]
    fn registry_returns_none_for_unknown_extension() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("file.unknownext");
        std::fs::write(&p, b"data").unwrap();
        let reg = ImageReaderRegistry::with_defaults();
        assert!(reg.probe(&p).is_none());
    }

    #[test]
    fn registry_returns_none_for_missing_file() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("ghost.iso");
        let reg = ImageReaderRegistry::with_defaults();
        assert!(reg.probe(&p).is_none());
    }
}
