//! Parse the xz stream footer to recover total uncompressed size without
//! decompressing the body.
//!
//! The decoder chain itself is streaming and can't answer "how many bytes
//! when fully decompressed" cheaply for arbitrary formats. xz is the one
//! exception: its index records carry per-block uncompressed sizes, and
//! the index lives at a fixed offset from end-of-file, so we can pop it
//! open with two `seek`s. For other formats (gzip mtime-only footer,
//! bzip2 blocked, zstd optional content-size) callers fall back to using
//! the compressed source size for queue display.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// Parse the xz stream footer (12 bytes at end-of-file) and walk the index
/// records backward to sum the uncompressed sizes of every block. Returns
/// `None` if the file is too short, malformed, or has no parseable index —
/// caller falls back to using the compressed size.
///
/// The index format is the documented one
/// (<https://tukaani.org/xz/xz-file-format.txt>) — multibyte integers
/// (VLIs) of 1..9 bytes, list begins with `index_indicator = 0x00`, then
/// number_of_records (VLI), then `number_of_records` × {unpadded_size,
/// uncompressed_size}, then padding to 4-byte alignment, then CRC32.
pub fn read_total_uncompressed(path: &Path) -> Option<u64> {
    let mut f = File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    if len < 24 {
        return None;
    }
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
    fn read_total_uncompressed_matches_payload_length() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("x.xz");
        std::fs::write(&p, xz_bytes(&vec![0xABu8; 4096])).unwrap();
        assert_eq!(read_total_uncompressed(&p), Some(4096));
    }

    #[test]
    fn read_total_uncompressed_returns_none_for_short_file() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("short.xz");
        std::fs::write(&p, b"too short").unwrap();
        assert!(read_total_uncompressed(&p).is_none());
    }

    #[test]
    fn decode_vli_handles_single_byte() {
        assert_eq!(decode_vli(&[0x00]), Some((0, 1)));
        assert_eq!(decode_vli(&[0x7F]), Some((127, 1)));
    }

    #[test]
    fn decode_vli_handles_two_bytes() {
        assert_eq!(decode_vli(&[0x80, 0x01]), Some((128, 2)));
    }

    #[test]
    fn decode_vli_returns_none_on_unterminated_sequence() {
        let nine = [0xFFu8; 9];
        assert_eq!(decode_vli(&nine), None);
    }

    #[test]
    fn parse_index_sums_uncompressed_records() {
        let idx = [0x00, 0x02, 0x01, 0x0A, 0x01, 0x14];
        assert_eq!(parse_index_total_uncompressed(&idx), Some(30));
    }

    #[test]
    fn parse_index_returns_none_for_missing_indicator() {
        assert!(parse_index_total_uncompressed(&[0x01, 0x01, 0x10, 0x20]).is_none());
    }
}
