use std::fs::File;
use std::io::{BufReader, Read, Result};
use std::path::Path;

use super::{ImageInfo, ImageReader, ImageReaderFactory};

pub struct ZstdReaderFactory;

fn inner_label(inner_ext: Option<&str>) -> &'static str {
    match inner_ext {
        Some("iso") => "ISO 9660 / ZSTD",
        Some("img") | Some("bin") | Some("raw") => "RAW DISK IMAGE / ZSTD",
        _ => "COMPRESSED / ZSTD",
    }
}

fn inner_extension(stem: &Path) -> Option<String> {
    stem.extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
}

/// Read the zstd frame header to extract the decompressed-content-size hint
/// when the producer included one. zstd encodes a "Frame_Content_Size" in
/// the first 14 bytes of the stream; if absent we fall back to the
/// compressed size. Reference: RFC 8478 §3.1.1.
fn read_frame_content_size(path: &Path) -> Option<u64> {
    let mut f = File::open(path).ok()?;
    let mut head = [0u8; 14];
    let n = f.read(&mut head).ok()?;
    if n < 6 {
        return None;
    }
    // Magic number: 0xFD2FB528 (LE).
    if head[..4] != [0x28, 0xB5, 0x2F, 0xFD] {
        return None;
    }
    let fhd = head[4];
    let fcs_flag = fhd >> 6;
    let single_segment = (fhd >> 5) & 1 == 1;
    let dict_id_flag = fhd & 0b11;
    // FCS is implicit (1 byte) when fcs_flag == 0 and single_segment == 1.
    let fcs_size = match (fcs_flag, single_segment) {
        (0, true) => 1usize,
        (0, false) => return None, // FCS field omitted entirely
        (1, _) => 2,
        (2, _) => 4,
        (3, _) => 8,
        _ => return None,
    };
    // Skip Window_Descriptor (1 byte if !single_segment) and Dictionary_ID.
    let dict_size = match dict_id_flag {
        0 => 0,
        1 => 1,
        2 => 2,
        3 => 4,
        _ => return None,
    };
    let mut pos = 5usize;
    if !single_segment {
        pos += 1;
    }
    pos += dict_size;
    if pos + fcs_size > head.len() {
        return None;
    }
    let mut buf = [0u8; 8];
    buf[..fcs_size].copy_from_slice(&head[pos..pos + fcs_size]);
    let raw = u64::from_le_bytes(buf);
    // Per spec, a 2-byte FCS is biased by +256.
    let value = if fcs_size == 2 { raw + 256 } else { raw };
    Some(value)
}

impl ImageReaderFactory for ZstdReaderFactory {
    fn name(&self) -> &'static str {
        "zstd"
    }

    fn probe(&self, path: &Path) -> Option<ImageInfo> {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|s| s.to_ascii_lowercase());
        let by_ext = matches!(ext.as_deref(), Some("zst") | Some("zstd"));
        let by_magic = super::magic::is_zstd(&super::magic::read_head(path, 4));
        if !by_ext && !by_magic {
            return None;
        }
        let meta = std::fs::metadata(path).ok()?;
        let source = meta.len();
        let inner_ext = path.file_stem().map(Path::new).and_then(inner_extension);
        let uncompressed = read_frame_content_size(path).unwrap_or(source);
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
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "not a .zst"))?;
        let file = File::open(path)?;
        let dec = zstd::stream::Decoder::new(BufReader::new(file))?;
        Ok(Box::new(ZstdReader {
            info,
            inner: Box::new(dec),
        }))
    }
}

pub struct ZstdReader {
    info: ImageInfo,
    inner: Box<dyn Read + Send>,
}

impl Read for ZstdReader {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        self.inner.read(buf)
    }
}

impl ImageReader for ZstdReader {
    fn info(&self) -> &ImageInfo {
        &self.info
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::tempdir;

    /// Bulk compress with size known up front so the producer writes a
    /// Frame_Content_Size in the header — that's what mirrors the typical
    /// "compress an iso file once" producer pattern, and it's the only
    /// path that lets `read_frame_content_size` recover the original size
    /// without decompressing. Streamed encoders typically omit FCS.
    fn zst_bytes(payload: &[u8]) -> Vec<u8> {
        zstd::bulk::compress(payload, 0).unwrap()
    }

    fn zst_bytes_streamed(payload: &[u8]) -> Vec<u8> {
        let mut e = zstd::stream::Encoder::new(Vec::new(), 0).unwrap();
        e.write_all(payload).unwrap();
        e.finish().unwrap()
    }

    #[test]
    fn probe_recognises_iso_zst_with_distro_label() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("boot.iso.zst");
        std::fs::write(&p, zst_bytes(b"hello world")).unwrap();
        let info = ZstdReaderFactory.probe(&p).unwrap();
        assert_eq!(info.format_label, "ISO 9660 / ZSTD");
        assert_eq!(info.uncompressed_bytes, 11);
    }

    #[test]
    fn probe_recognises_zstd_extension_alias() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("disk.img.zstd");
        std::fs::write(&p, zst_bytes(b"abcd")).unwrap();
        let info = ZstdReaderFactory.probe(&p).unwrap();
        assert_eq!(info.format_label, "RAW DISK IMAGE / ZSTD");
        assert_eq!(info.uncompressed_bytes, 4);
    }

    #[test]
    fn probe_recognises_img_zst_with_raw_label() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("disk.img.zst");
        let payload = vec![0xCDu8; 4096];
        std::fs::write(&p, zst_bytes(&payload)).unwrap();
        let info = ZstdReaderFactory.probe(&p).unwrap();
        assert_eq!(info.format_label, "RAW DISK IMAGE / ZSTD");
        assert_eq!(info.uncompressed_bytes, 4096);
    }

    #[test]
    fn probe_falls_back_when_frame_size_missing() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("junk.iso.zst");
        std::fs::write(&p, b"not really zstd").unwrap();
        let info = ZstdReaderFactory.probe(&p).unwrap();
        assert_eq!(info.uncompressed_bytes, info.source_bytes);
    }

    #[test]
    fn probe_falls_back_for_streamed_encoder_without_fcs() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("streamed.img.zst");
        // Streamed encoders write FCS_flag=0, single_segment=0 → field
        // omitted entirely. Probe still succeeds and falls back to the
        // compressed size for the progress bar.
        let payload = vec![0u8; 4096];
        std::fs::write(&p, zst_bytes_streamed(&payload)).unwrap();
        let info = ZstdReaderFactory.probe(&p).unwrap();
        assert_eq!(info.uncompressed_bytes, info.source_bytes);
    }

    #[test]
    fn probe_accepts_renamed_file_via_magic() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("renamed.iso");
        std::fs::write(&p, zst_bytes(b"xyz")).unwrap();
        let info = ZstdReaderFactory.probe(&p).expect("magic should match");
        assert!(info.format_label.contains("ZSTD"));
    }

    #[test]
    fn probe_rejects_when_neither_extension_nor_magic_match() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("file.iso");
        std::fs::write(&p, b"plain bytes").unwrap();
        assert!(ZstdReaderFactory.probe(&p).is_none());
    }

    #[test]
    fn probe_returns_none_for_missing_file() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("ghost.iso.zst");
        assert!(ZstdReaderFactory.probe(&p).is_none());
    }

    #[test]
    fn open_streams_decompressed_payload() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("payload.img.zst");
        let payload: Vec<u8> = (0..50_000u32).map(|i| (i % 256) as u8).collect();
        std::fs::write(&p, zst_bytes(&payload)).unwrap();
        let mut r = ZstdReaderFactory.open(&p).unwrap();
        let mut out = Vec::new();
        r.read_to_end(&mut out).unwrap();
        assert_eq!(out, payload);
    }

    #[test]
    fn inner_label_maps_known_inner_extensions() {
        assert_eq!(inner_label(Some("iso")), "ISO 9660 / ZSTD");
        assert_eq!(inner_label(Some("img")), "RAW DISK IMAGE / ZSTD");
        assert_eq!(inner_label(Some("bin")), "RAW DISK IMAGE / ZSTD");
        assert_eq!(inner_label(Some("raw")), "RAW DISK IMAGE / ZSTD");
        assert_eq!(inner_label(Some("xyz")), "COMPRESSED / ZSTD");
        assert_eq!(inner_label(None), "COMPRESSED / ZSTD");
    }
}
