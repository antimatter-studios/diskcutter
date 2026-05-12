use std::fs::File;
use std::io::{Read, Result, Seek, SeekFrom};
use std::path::Path;

use flate2::read::GzDecoder;

use super::{ImageInfo, ImageReader, ImageReaderFactory};

pub struct GzipReaderFactory;

fn inner_label(inner_ext: Option<&str>) -> &'static str {
    match inner_ext {
        Some("iso") => "ISO 9660 / GZIP",
        Some("img") | Some("bin") | Some("raw") => "RAW DISK IMAGE / GZIP",
        _ => "COMPRESSED / GZIP",
    }
}

fn inner_extension(stem: &Path) -> Option<String> {
    stem.extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
}

/// Read the gzip ISIZE trailer — the uncompressed size of the LAST member,
/// modulo 2^32. Correct for single-stream gzips < 4 GiB (the usual case for
/// distro images); ambiguous above 4 GiB and for multi-member archives. The
/// pipeline tolerates a wrong total via `bytes_total.max(done)`, so even a
/// stale ISIZE just makes the progress bar drift, not fail.
fn read_isize(path: &Path) -> Option<u64> {
    let mut f = File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    if len < 4 {
        return None;
    }
    f.seek(SeekFrom::End(-4)).ok()?;
    let mut b = [0u8; 4];
    f.read_exact(&mut b).ok()?;
    Some(u32::from_le_bytes(b) as u64)
}

impl ImageReaderFactory for GzipReaderFactory {
    fn name(&self) -> &'static str {
        "gzip"
    }

    fn probe(&self, path: &Path) -> Option<ImageInfo> {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|s| s.to_ascii_lowercase());
        if ext.as_deref() != Some("gz") {
            return None;
        }
        let meta = std::fs::metadata(path).ok()?;
        let source = meta.len();
        let inner_ext = path.file_stem().map(Path::new).and_then(inner_extension);
        let uncompressed = read_isize(path).unwrap_or(source);
        Some(ImageInfo {
            path: path.to_path_buf(),
            format_label: inner_label(inner_ext.as_deref()).to_string(),
            source_bytes: source,
            uncompressed_bytes: uncompressed,
        })
    }

    fn open(&self, path: &Path) -> Result<Box<dyn ImageReader>> {
        let info = self
            .probe(path)
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "not a .gz"))?;
        let file = File::open(path)?;
        let dec = GzDecoder::new(file);
        Ok(Box::new(GzipReader { info, inner: dec }))
    }
}

pub struct GzipReader {
    info: ImageInfo,
    inner: GzDecoder<File>,
}

impl Read for GzipReader {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        self.inner.read(buf)
    }
}

impl ImageReader for GzipReader {
    fn info(&self) -> &ImageInfo {
        &self.info
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use std::io::Write;
    use tempfile::tempdir;

    fn gz_bytes(payload: &[u8]) -> Vec<u8> {
        let mut e = GzEncoder::new(Vec::new(), Compression::default());
        e.write_all(payload).unwrap();
        e.finish().unwrap()
    }

    #[test]
    fn probe_recognises_iso_gz_with_distro_label() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("boot.iso.gz");
        std::fs::write(&p, gz_bytes(b"hello world")).unwrap();
        let info = GzipReaderFactory.probe(&p).unwrap();
        assert_eq!(info.format_label, "ISO 9660 / GZIP");
        assert_eq!(info.uncompressed_bytes, 11);
    }

    #[test]
    fn probe_recognises_img_gz_with_raw_label() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("disk.img.gz");
        std::fs::write(&p, gz_bytes(b"abcd")).unwrap();
        let info = GzipReaderFactory.probe(&p).unwrap();
        assert_eq!(info.format_label, "RAW DISK IMAGE / GZIP");
        assert_eq!(info.uncompressed_bytes, 4);
    }

    #[test]
    fn probe_falls_back_when_inner_extension_unknown() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("thing.dat.gz");
        std::fs::write(&p, gz_bytes(b"xyz")).unwrap();
        let info = GzipReaderFactory.probe(&p).unwrap();
        assert_eq!(info.format_label, "COMPRESSED / GZIP");
    }

    #[test]
    fn probe_rejects_non_gz_extension() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("file.iso");
        std::fs::write(&p, gz_bytes(b"xyz")).unwrap();
        assert!(GzipReaderFactory.probe(&p).is_none());
    }

    #[test]
    fn probe_returns_none_for_missing_file() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("ghost.iso.gz");
        assert!(GzipReaderFactory.probe(&p).is_none());
    }

    #[test]
    fn open_streams_decompressed_payload() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("payload.img.gz");
        let payload: Vec<u8> = (0..10_000u32).map(|i| (i % 256) as u8).collect();
        std::fs::write(&p, gz_bytes(&payload)).unwrap();
        let mut r = GzipReaderFactory.open(&p).unwrap();
        let mut out = Vec::new();
        r.read_to_end(&mut out).unwrap();
        assert_eq!(out, payload);
    }

    #[test]
    fn read_isize_returns_uncompressed_size_for_small_payload() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("a.gz");
        std::fs::write(&p, gz_bytes(b"abcdef")).unwrap();
        assert_eq!(read_isize(&p), Some(6));
    }

    #[test]
    fn inner_label_maps_known_inner_extensions() {
        assert_eq!(inner_label(Some("iso")), "ISO 9660 / GZIP");
        assert_eq!(inner_label(Some("img")), "RAW DISK IMAGE / GZIP");
        assert_eq!(inner_label(Some("bin")), "RAW DISK IMAGE / GZIP");
        assert_eq!(inner_label(Some("raw")), "RAW DISK IMAGE / GZIP");
        assert_eq!(inner_label(Some("xyz")), "COMPRESSED / GZIP");
        assert_eq!(inner_label(None), "COMPRESSED / GZIP");
    }
}
