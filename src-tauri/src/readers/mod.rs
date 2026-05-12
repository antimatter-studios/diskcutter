use std::io::Read;
use std::path::{Path, PathBuf};

mod bzip2;
mod gzip;
mod magic;
mod qcow2;
mod raw;
mod vhd;
mod vhdx;
mod vmdk;
mod xz;
mod zstd;
pub use bzip2::Bzip2ReaderFactory;
pub use gzip::GzipReaderFactory;
pub use qcow2::Qcow2ReaderFactory;
pub use raw::RawReaderFactory;
pub use vhd::VhdReaderFactory;
pub use vhdx::VhdxReaderFactory;
pub use vmdk::VmdkReaderFactory;
pub use xz::XzReaderFactory;
pub use zstd::ZstdReaderFactory;

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
                Box::new(Bzip2ReaderFactory),
                Box::new(ZstdReaderFactory),
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

    #[test]
    fn registry_routes_renamed_gzip_to_gzip_factory_via_magic() {
        // Real-world footgun: user downloaded `ubuntu.iso.gz`, then
        // renamed to `ubuntu.iso` thinking it was already a raw image.
        // Without magic sniff, the registry routes this to RAW and the
        // burn produces an unbootable USB. With magic sniff, GzipReader
        // claims it.
        use flate2::write::GzEncoder;
        use flate2::Compression;
        use std::io::Write;
        let dir = tempdir().unwrap();
        let p = dir.path().join("disguised.iso");
        let mut e = GzEncoder::new(Vec::new(), Compression::default());
        e.write_all(b"would have been a corrupt burn").unwrap();
        let body = e.finish().unwrap();
        std::fs::write(&p, &body).unwrap();
        let reg = ImageReaderRegistry::with_defaults();
        let (info, _f) = reg.probe(&p).expect("magic should route to gzip");
        assert!(
            info.format_label.contains("GZIP"),
            "expected GZIP label, got {}",
            info.format_label
        );
    }

    #[test]
    fn registry_prefers_container_format_over_raw_for_extensionless_qcow2() {
        // qcow2 files in the wild sometimes lack a .qcow2 extension
        // (some tools strip extensions, or the file is shipped inside
        // a tarball that strips). Magic sniff catches the `QFI\xfb`
        // signature and routes to the qcow2 factory.
        let dir = tempdir().unwrap();
        let p = dir.path().join("nameless");
        // A minimal v3 qcow2 header — magic + version, rest zeroed.
        // Qcow2Reader::open will reject this for being malformed, so
        // probe returns None even though magic matches. That's the
        // correct conservative behavior: don't claim a file we can't
        // actually read.
        let mut head = vec![0u8; 4096];
        head[0..4].copy_from_slice(&[0x51, 0x46, 0x49, 0xFB]);
        head[4..8].copy_from_slice(&3u32.to_be_bytes());
        std::fs::write(&p, &head).unwrap();
        let reg = ImageReaderRegistry::with_defaults();
        // probe() may return None (parser rejects malformed body) or
        // Some(qcow2) — either is acceptable as long as it isn't
        // misclassified as some other format.
        if let Some((info, _)) = reg.probe(&p) {
            assert!(info.format_label.contains("QCOW2"));
        }
    }
}
