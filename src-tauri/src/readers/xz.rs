use std::fs::File;
use std::io::{Read, Result};
use std::path::Path;

use xz2::read::XzDecoder;

use super::{ImageInfo, ImageReader, ImageReaderFactory};

pub struct XzReaderFactory;

fn inner_label(inner_ext: Option<&str>) -> &'static str {
    match inner_ext {
        Some("iso") => "ISO 9660 / XZ",
        Some("img") | Some("bin") | Some("raw") => "RAW DISK IMAGE / XZ",
        _ => "COMPRESSED / XZ",
    }
}

fn inner_extension(stem: &Path) -> Option<String> {
    stem.extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
}

/// Parse the xz stream footer (12 bytes at end-of-file) and walk the index
/// records backward to sum the uncompressed sizes of every block. Returns
/// `None` if the file is too short, malformed, or uses an extension we
/// don't understand — caller falls back to using the compressed size.
///
/// The index format is the documented one
/// (<https://tukaani.org/xz/xz-file-format.txt>) — Multibyte integers
/// (VLIs) of 1..9 bytes, list begins with `index_indicator = 0x00`, then
/// number_of_records (VLI), then `number_of_records` × {unpadded_size,
/// uncompressed_size}, then padding to 4-byte alignment, then CRC32.
fn read_total_uncompressed(path: &Path) -> Option<u64> {
    let mut f = File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    if len < 24 {
        return None;
    }
    use std::io::{Seek, SeekFrom};
    // Footer is the last 12 bytes: CRC32(4) | backward_size(4) | stream_flags(2) | "YZ"(2)
    f.seek(SeekFrom::End(-12)).ok()?;
    let mut footer = [0u8; 12];
    f.read_exact(&mut footer).ok()?;
    if &footer[10..12] != b"YZ" {
        return None;
    }
    let backward_size_enc = u32::from_le_bytes([footer[4], footer[5], footer[6], footer[7]]);
    let real_backward = (backward_size_enc as u64).checked_add(1)?.checked_mul(4)?;
    let index_end = len.checked_sub(12)?;
    let index_start = index_end.checked_sub(real_backward)?;
    if index_start >= index_end {
        return None;
    }
    f.seek(SeekFrom::Start(index_start)).ok()?;
    let mut index = vec![0u8; (index_end - index_start) as usize];
    f.read_exact(&mut index).ok()?;
    parse_index_total_uncompressed(&index)
}

fn parse_index_total_uncompressed(idx: &[u8]) -> Option<u64> {
    if idx.is_empty() || idx[0] != 0x00 {
        return None;
    }
    let mut p = 1usize;
    let (records, consumed) = decode_vli(&idx[p..])?;
    p += consumed;
    let mut total: u64 = 0;
    for _ in 0..records {
        let (_unpadded, c1) = decode_vli(&idx[p..])?;
        p += c1;
        let (uncompressed, c2) = decode_vli(&idx[p..])?;
        p += c2;
        total = total.checked_add(uncompressed)?;
    }
    Some(total)
}

/// Decode an xz variable-length integer. Up to 9 bytes, each carrying 7
/// data bits + one continuation bit (MSB). Returns (value, bytes_consumed).
fn decode_vli(bytes: &[u8]) -> Option<(u64, usize)> {
    let mut v: u64 = 0;
    for (i, &b) in bytes.iter().take(9).enumerate() {
        v |= ((b & 0x7F) as u64) << (7 * i);
        if b & 0x80 == 0 {
            return Some((v, i + 1));
        }
    }
    None
}

impl ImageReaderFactory for XzReaderFactory {
    fn name(&self) -> &'static str {
        "xz"
    }

    fn probe(&self, path: &Path) -> Option<ImageInfo> {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|s| s.to_ascii_lowercase());
        if ext.as_deref() != Some("xz") {
            return None;
        }
        let meta = std::fs::metadata(path).ok()?;
        let source = meta.len();
        let inner_ext = path.file_stem().map(Path::new).and_then(inner_extension);
        let uncompressed = read_total_uncompressed(path).unwrap_or(source);
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
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "not a .xz"))?;
        let file = File::open(path)?;
        let dec = XzDecoder::new(file);
        Ok(Box::new(XzReader { info, inner: dec }))
    }
}

pub struct XzReader {
    info: ImageInfo,
    inner: XzDecoder<File>,
}

impl Read for XzReader {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        self.inner.read(buf)
    }
}

impl ImageReader for XzReader {
    fn info(&self) -> &ImageInfo {
        &self.info
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::tempdir;
    use xz2::write::XzEncoder;

    fn xz_bytes(payload: &[u8]) -> Vec<u8> {
        let mut e = XzEncoder::new(Vec::new(), 1);
        e.write_all(payload).unwrap();
        e.finish().unwrap()
    }

    #[test]
    fn probe_recognises_iso_xz_with_distro_label() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("boot.iso.xz");
        std::fs::write(&p, xz_bytes(b"hello world")).unwrap();
        let info = XzReaderFactory.probe(&p).unwrap();
        assert_eq!(info.format_label, "ISO 9660 / XZ");
        assert_eq!(info.uncompressed_bytes, 11);
    }

    #[test]
    fn probe_recognises_img_xz_with_raw_label() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("disk.img.xz");
        let payload = vec![0xABu8; 4096];
        std::fs::write(&p, xz_bytes(&payload)).unwrap();
        let info = XzReaderFactory.probe(&p).unwrap();
        assert_eq!(info.format_label, "RAW DISK IMAGE / XZ");
        assert_eq!(info.uncompressed_bytes, 4096);
    }

    #[test]
    fn probe_falls_back_to_compressed_size_when_index_unparseable() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("junk.iso.xz");
        // Not a real xz file — too short for footer parse. probe() still
        // returns Some(...) because the extension matches; uncompressed
        // size falls back to source size.
        std::fs::write(&p, b"not really xz").unwrap();
        let info = XzReaderFactory.probe(&p).unwrap();
        assert_eq!(info.format_label, "ISO 9660 / XZ");
        assert_eq!(info.uncompressed_bytes, info.source_bytes);
    }

    #[test]
    fn probe_rejects_non_xz_extension() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("file.iso");
        std::fs::write(&p, xz_bytes(b"xyz")).unwrap();
        assert!(XzReaderFactory.probe(&p).is_none());
    }

    #[test]
    fn open_streams_decompressed_payload() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("payload.img.xz");
        let payload: Vec<u8> = (0..50_000u32).map(|i| (i % 256) as u8).collect();
        std::fs::write(&p, xz_bytes(&payload)).unwrap();
        let mut r = XzReaderFactory.open(&p).unwrap();
        let mut out = Vec::new();
        r.read_to_end(&mut out).unwrap();
        assert_eq!(out, payload);
    }

    #[test]
    fn decode_vli_handles_single_byte() {
        assert_eq!(decode_vli(&[0x00]), Some((0, 1)));
        assert_eq!(decode_vli(&[0x7F]), Some((127, 1)));
    }

    #[test]
    fn decode_vli_handles_two_bytes() {
        // 0x80, 0x01 → 0 | (1 << 7) = 128
        assert_eq!(decode_vli(&[0x80, 0x01]), Some((128, 2)));
    }

    #[test]
    fn decode_vli_returns_none_on_unterminated_sequence() {
        // 9 bytes all with continuation bit set
        let nine = [0xFFu8; 9];
        assert_eq!(decode_vli(&nine), None);
    }

    #[test]
    fn parse_index_returns_none_for_missing_indicator() {
        // Index indicator byte must be 0x00.
        assert!(parse_index_total_uncompressed(&[0x01, 0x01, 0x10, 0x20]).is_none());
    }

    #[test]
    fn parse_index_sums_uncompressed_records() {
        // indicator(0x00) | n_records(VLI 2) | rec1{unpadded=1, uncompressed=10}
        // | rec2{unpadded=1, uncompressed=20}
        let idx = [0x00, 0x02, 0x01, 0x0A, 0x01, 0x14];
        assert_eq!(parse_index_total_uncompressed(&idx), Some(30));
    }

    #[test]
    fn inner_label_maps_known_inner_extensions() {
        assert_eq!(inner_label(Some("iso")), "ISO 9660 / XZ");
        assert_eq!(inner_label(Some("img")), "RAW DISK IMAGE / XZ");
        assert_eq!(inner_label(Some("bin")), "RAW DISK IMAGE / XZ");
        assert_eq!(inner_label(Some("raw")), "RAW DISK IMAGE / XZ");
        assert_eq!(inner_label(Some("xyz")), "COMPRESSED / XZ");
        assert_eq!(inner_label(None), "COMPRESSED / XZ");
    }
}
