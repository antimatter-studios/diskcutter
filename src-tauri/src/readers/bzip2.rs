use std::fs::File;
use std::io::{Read, Result};
use std::path::Path;

use bzip2::read::BzDecoder;

use super::{ImageInfo, ImageReader, ImageReaderFactory};

pub struct Bzip2ReaderFactory;

fn inner_label(inner_ext: Option<&str>) -> &'static str {
    match inner_ext {
        Some("iso") => "ISO 9660 / BZIP2",
        Some("img") | Some("bin") | Some("raw") => "RAW DISK IMAGE / BZIP2",
        _ => "COMPRESSED / BZIP2",
    }
}

fn inner_extension(stem: &Path) -> Option<String> {
    stem.extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
}

/// bzip2 has no equivalent of gzip's ISIZE or xz's index — the only way to
/// know the uncompressed size is to decompress. Falling back to the
/// compressed size keeps probe cheap and inspect snappy; the pipeline
/// tolerates a wrong total via `bytes_total.max(done)`, so the progress bar
/// just drifts during the burn instead of blocking it.
impl ImageReaderFactory for Bzip2ReaderFactory {
    fn name(&self) -> &'static str {
        "bzip2"
    }

    fn probe(&self, path: &Path) -> Option<ImageInfo> {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|s| s.to_ascii_lowercase());
        let by_ext = matches!(ext.as_deref(), Some("bz2") | Some("bzip2"));
        let by_magic = super::magic::is_bzip2(&super::magic::read_head(path, 3));
        if !by_ext && !by_magic {
            return None;
        }
        let meta = std::fs::metadata(path).ok()?;
        let source = meta.len();
        let inner_ext = path.file_stem().map(Path::new).and_then(inner_extension);
        Some(ImageInfo {
            path: path.to_path_buf(),
            format_label: inner_label(inner_ext.as_deref()).to_string(),
            source_bytes: source,
            uncompressed_bytes: source,
        })
    }

    fn open(&self, path: &Path) -> Result<Box<dyn ImageReader>> {
        let info = self
            .probe(path)
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "not a .bz2"))?;
        let file = File::open(path)?;
        let dec = BzDecoder::new(file);
        Ok(Box::new(Bzip2Reader { info, inner: dec }))
    }
}

pub struct Bzip2Reader {
    info: ImageInfo,
    inner: BzDecoder<File>,
}

impl Read for Bzip2Reader {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        self.inner.read(buf)
    }
}

impl ImageReader for Bzip2Reader {
    fn info(&self) -> &ImageInfo {
        &self.info
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bzip2::write::BzEncoder;
    use bzip2::Compression;
    use std::io::Write;
    use tempfile::tempdir;

    fn bz_bytes(payload: &[u8]) -> Vec<u8> {
        let mut e = BzEncoder::new(Vec::new(), Compression::default());
        e.write_all(payload).unwrap();
        e.finish().unwrap()
    }

    #[test]
    fn probe_recognises_iso_bz2_with_distro_label() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("boot.iso.bz2");
        std::fs::write(&p, bz_bytes(b"hello world")).unwrap();
        let info = Bzip2ReaderFactory.probe(&p).unwrap();
        assert_eq!(info.format_label, "ISO 9660 / BZIP2");
    }

    #[test]
    fn probe_recognises_img_bz2_with_raw_label() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("disk.img.bz2");
        std::fs::write(&p, bz_bytes(b"abcd")).unwrap();
        let info = Bzip2ReaderFactory.probe(&p).unwrap();
        assert_eq!(info.format_label, "RAW DISK IMAGE / BZIP2");
    }

    #[test]
    fn probe_recognises_bzip2_extension_alias() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("disk.img.bzip2");
        std::fs::write(&p, bz_bytes(b"abcd")).unwrap();
        let info = Bzip2ReaderFactory.probe(&p).unwrap();
        assert_eq!(info.format_label, "RAW DISK IMAGE / BZIP2");
    }

    #[test]
    fn probe_accepts_renamed_file_via_magic() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("renamed.iso");
        std::fs::write(&p, bz_bytes(b"xyz")).unwrap();
        let info = Bzip2ReaderFactory.probe(&p).expect("magic should match");
        assert!(info.format_label.contains("BZIP2"));
    }

    #[test]
    fn probe_rejects_when_neither_extension_nor_magic_match() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("file.iso");
        std::fs::write(&p, b"plain bytes").unwrap();
        assert!(Bzip2ReaderFactory.probe(&p).is_none());
    }

    #[test]
    fn probe_returns_none_for_missing_file() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("ghost.iso.bz2");
        assert!(Bzip2ReaderFactory.probe(&p).is_none());
    }

    #[test]
    fn open_streams_decompressed_payload() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("payload.img.bz2");
        let payload: Vec<u8> = (0..10_000u32).map(|i| (i % 256) as u8).collect();
        std::fs::write(&p, bz_bytes(&payload)).unwrap();
        let mut r = Bzip2ReaderFactory.open(&p).unwrap();
        let mut out = Vec::new();
        r.read_to_end(&mut out).unwrap();
        assert_eq!(out, payload);
    }

    #[test]
    fn inner_label_maps_known_inner_extensions() {
        assert_eq!(inner_label(Some("iso")), "ISO 9660 / BZIP2");
        assert_eq!(inner_label(Some("img")), "RAW DISK IMAGE / BZIP2");
        assert_eq!(inner_label(Some("bin")), "RAW DISK IMAGE / BZIP2");
        assert_eq!(inner_label(Some("raw")), "RAW DISK IMAGE / BZIP2");
        assert_eq!(inner_label(Some("xyz")), "COMPRESSED / BZIP2");
        assert_eq!(inner_label(None), "COMPRESSED / BZIP2");
    }
}
