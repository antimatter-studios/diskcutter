//! Content-based file-format detection.
//!
//! Each reader factory used to gate on file extension only — fast but
//! wrong when a user renames `ubuntu.iso.gz` to `ubuntu.iso`. The helpers
//! here read the file's first 16 bytes (or last 512 for VHD's footer) and
//! check the published magic signatures, letting factories accept by
//! magic OR extension. The first factory the registry visits whose
//! either-check passes wins.
//!
//! Reads are tiny (≤ 512 bytes) and only performed during probe, so the
//! extra I/O is invisible against the burn that follows.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// Read up to `n` bytes from the start of `path`. Returns an empty vec
/// on any I/O error — probe code treats that as "no magic match".
pub(crate) fn read_head(path: &Path, n: usize) -> Vec<u8> {
    let Ok(mut f) = File::open(path) else {
        return Vec::new();
    };
    let mut buf = vec![0u8; n];
    let read = f.read(&mut buf).unwrap_or(0);
    buf.truncate(read);
    buf
}

/// Read the last `n` bytes of `path`. VHD's only reliable identifier is
/// the footer magic `b"conectix"` at the trailing 512-byte block, so
/// extension-less detection needs tail-of-file access.
pub(crate) fn read_tail(path: &Path, n: usize) -> Vec<u8> {
    let Ok(mut f) = File::open(path) else {
        return Vec::new();
    };
    let Ok(meta) = f.metadata() else {
        return Vec::new();
    };
    let len = meta.len();
    if len < n as u64 {
        let mut buf = vec![0u8; len as usize];
        let _ = f.read(&mut buf);
        return buf;
    }
    if f.seek(SeekFrom::End(-(n as i64))).is_err() {
        return Vec::new();
    }
    let mut buf = vec![0u8; n];
    let read = f.read(&mut buf).unwrap_or(0);
    buf.truncate(read);
    buf
}

#[allow(dead_code)]
pub(crate) fn is_qcow2(head: &[u8]) -> bool {
    head.starts_with(&[0x51, 0x46, 0x49, 0xFB]) // "QFI\xfb"
}

#[allow(dead_code)]
pub(crate) fn is_vhdx(head: &[u8]) -> bool {
    head.starts_with(b"vhdxfile")
}

#[allow(dead_code)]
pub(crate) fn is_vmdk(head: &[u8]) -> bool {
    // KDMV = monolithicSparse / streamOptimized. Text "VMDK" desc-file
    // variant isn't supported by am-img-vmdk so we don't detect it.
    head.starts_with(b"KDMV")
}

#[allow(dead_code)]
pub(crate) fn is_vhd_footer(tail: &[u8]) -> bool {
    tail.windows(8).any(|w| w == b"conectix")
}

pub(crate) fn is_gzip(head: &[u8]) -> bool {
    head.starts_with(&[0x1F, 0x8B])
}

pub(crate) fn is_xz(head: &[u8]) -> bool {
    head.starts_with(&[0xFD, b'7', b'z', b'X', b'Z', 0x00])
}

pub(crate) fn is_bzip2(head: &[u8]) -> bool {
    head.starts_with(b"BZh")
}

pub(crate) fn is_zstd(head: &[u8]) -> bool {
    head.starts_with(&[0x28, 0xB5, 0x2F, 0xFD])
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn read_head_returns_truncated_vec_for_short_file() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("short");
        std::fs::write(&p, b"abc").unwrap();
        assert_eq!(read_head(&p, 16), b"abc");
    }

    #[test]
    fn read_head_returns_empty_for_missing_file() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("missing");
        assert!(read_head(&p, 16).is_empty());
    }

    #[test]
    fn read_tail_grabs_last_n_bytes() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("trailer");
        let body: Vec<u8> = (0..1024u32).map(|i| (i % 256) as u8).collect();
        std::fs::write(&p, &body).unwrap();
        let tail = read_tail(&p, 64);
        assert_eq!(tail.len(), 64);
        assert_eq!(tail, &body[body.len() - 64..]);
    }

    #[test]
    fn read_tail_returns_whole_file_when_file_shorter_than_n() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("tiny");
        std::fs::write(&p, b"hi").unwrap();
        assert_eq!(read_tail(&p, 64), b"hi");
    }

    #[test]
    fn is_qcow2_matches_qfi_signature() {
        assert!(is_qcow2(&[0x51, 0x46, 0x49, 0xFB, 0, 0, 0, 3]));
        assert!(!is_qcow2(&[0x51, 0x46, 0x49, 0x00]));
        assert!(!is_qcow2(&[]));
    }

    #[test]
    fn is_vhdx_matches_signature() {
        assert!(is_vhdx(b"vhdxfile\x00\x00"));
        assert!(!is_vhdx(b"vhdxfilx"));
    }

    #[test]
    fn is_vmdk_matches_kdmv_signature() {
        assert!(is_vmdk(b"KDMV\x01\x00\x00\x00"));
        assert!(!is_vmdk(b"VMDK"));
    }

    #[test]
    fn is_vhd_footer_finds_conectix_anywhere_in_tail() {
        let mut t = vec![0u8; 100];
        t.extend_from_slice(b"conectix");
        t.extend_from_slice(&[0u8; 50]);
        assert!(is_vhd_footer(&t));
    }

    #[test]
    fn is_vhd_footer_returns_false_when_signature_absent() {
        assert!(!is_vhd_footer(&[0u8; 100]));
    }

    #[test]
    fn is_gzip_matches_1f8b() {
        assert!(is_gzip(&[0x1F, 0x8B, 0x08]));
        assert!(!is_gzip(&[0x1F, 0x8A]));
    }

    #[test]
    fn is_xz_matches_fd_7zxz_00() {
        assert!(is_xz(&[0xFD, b'7', b'z', b'X', b'Z', 0x00, 0xFF]));
        assert!(!is_xz(&[0xFD, b'7', b'z', b'X', b'Z', 0x01]));
    }

    #[test]
    fn is_bzip2_matches_bzh() {
        assert!(is_bzip2(b"BZh9"));
        assert!(!is_bzip2(b"BZx"));
    }

    #[test]
    fn is_zstd_matches_28b52ffd() {
        assert!(is_zstd(&[0x28, 0xB5, 0x2F, 0xFD]));
        assert!(!is_zstd(&[0x28, 0xB5, 0x2F, 0x00]));
    }
}
