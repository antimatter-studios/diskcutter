//! Sparse-file helpers — punch holes in regular-file outputs whenever
//! the source delivers a chunk of all-zero bytes.
//!
//! This is the simplest form of sparse output: no upstream allocation
//! map needed, no format-specific knowledge. The cost is one pass over
//! each chunk to decide if it's all zero. The benefit is enormous for
//! qcow2 / vhd-dynamic / vhdx-dynamic / vmdk sparse images: the virtual
//! disk's zero clusters never hit the destination filesystem, so a
//! 100 GiB backup of a 12 GiB-allocated VM image takes ~12 GiB on disk
//! instead of 100 GiB.
//!
//! Block-device outputs (burn target /dev/diskN) can't have holes —
//! `seek` past data leaves whatever's already there. So this writer
//! is meant for backup output paths, not burn targets. The caller
//! decides.
//!
//! `is_all_zero` is a pure function the test suite hammers against
//! every interesting boundary (empty, one byte, exact word-multiple,
//! odd-length, single non-zero in the middle).

use std::fs::File;
use std::io::{Result, Seek, SeekFrom, Write};

/// Returns true when every byte in `buf` is zero. Branchy on tiny
/// buffers, falls back to chunked u64 compares for larger sizes so
/// the typical 64-KiB-to-1-MiB chunk path is roughly memory-bandwidth
/// limited.
#[inline]
pub fn is_all_zero(buf: &[u8]) -> bool {
    // Handle the unaligned head + tail separately from the wide middle.
    let (head, mid, tail) = unsafe { buf.align_to::<u64>() };
    if head.iter().any(|b| *b != 0) {
        return false;
    }
    if mid.iter().any(|w| *w != 0) {
        return false;
    }
    if tail.iter().any(|b| *b != 0) {
        return false;
    }
    true
}

/// Write adapter that turns runs of zero bytes into filesystem holes.
/// On every `write` call:
///
///   - If the chunk is entirely zero, we advance the file position
///     past it (`seek(Current(n))`) without writing — the underlying
///     filesystem fills the gap with zero-on-read holes.
///   - If the chunk has at least one non-zero byte, we write the
///     whole chunk verbatim. We do **not** try to split a partially-
///     zero chunk because the chunk boundaries are already a good
///     sparsity granularity in practice (qcow2 cluster_size is 64 KiB
///     by default, vhd-dynamic blocks are 2 MiB, vhdx is 32 MiB —
///     all bigger than our backup chunk).
///
/// On `finish()` we truncate the file to the final cursor position so
/// the trailing zero chunks are reflected as the file's *length*
/// rather than data. Without that, a hole written past the previous
/// end-of-file leaves the file shorter than the source.
pub struct SparseFileWriter {
    file: Option<File>,
    pos: u64,
    chunk_size_hint: usize,
    /// True once we've seen a non-trivial seek-only call — drives the
    /// final truncate so the file ends exactly at `pos`.
    needs_truncate: bool,
}

impl SparseFileWriter {
    pub fn new(file: File, chunk_size_hint: usize) -> Self {
        Self {
            file: Some(file),
            pos: 0,
            chunk_size_hint,
            needs_truncate: false,
        }
    }

    pub fn chunk_size_hint(&self) -> usize {
        self.chunk_size_hint
    }

    /// Current cursor position (== bytes the file is logically long).
    pub fn position(&self) -> u64 {
        self.pos
    }

    /// Truncate to the cursor and return the inner `File`.
    pub fn finish(mut self) -> Result<File> {
        let mut f = self.file.take().expect("file already taken");
        if self.needs_truncate {
            f.set_len(self.pos)?;
        }
        f.flush()?;
        Ok(f)
    }
}

/// Catches the case where the writer is dropped without an explicit
/// `finish()` call — happens whenever the sparse writer sits inside a
/// `Box<dyn Write + Send>` and the owning code can't downcast to call
/// `finish()`. The truncate-and-flush is best-effort: I/O errors at
/// drop time are silently swallowed (matching `File`'s own drop), so
/// callers who care should call `finish()` directly.
impl Drop for SparseFileWriter {
    fn drop(&mut self) {
        if let Some(mut f) = self.file.take() {
            if self.needs_truncate {
                let _ = f.set_len(self.pos);
            }
            let _ = f.flush();
        }
    }
}

impl Write for SparseFileWriter {
    fn write(&mut self, buf: &[u8]) -> Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let f = self.file.as_mut().expect("write after finish");
        if is_all_zero(buf) {
            f.seek(SeekFrom::Current(buf.len() as i64))?;
            self.pos += buf.len() as u64;
            self.needs_truncate = true;
            Ok(buf.len())
        } else {
            f.write_all(buf)?;
            self.pos += buf.len() as u64;
            self.needs_truncate = false;
            Ok(buf.len())
        }
    }

    fn flush(&mut self) -> Result<()> {
        self.file.as_mut().expect("flush after finish").flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use tempfile::tempdir;

    #[test]
    fn is_all_zero_empty_buffer_is_true() {
        assert!(is_all_zero(&[]));
    }

    #[test]
    fn is_all_zero_single_zero_is_true() {
        assert!(is_all_zero(&[0]));
    }

    #[test]
    fn is_all_zero_single_nonzero_is_false() {
        assert!(!is_all_zero(&[1]));
        assert!(!is_all_zero(&[0xFF]));
    }

    #[test]
    fn is_all_zero_all_zero_block_is_true() {
        assert!(is_all_zero(&[0u8; 4096]));
    }

    #[test]
    fn is_all_zero_one_byte_nonzero_at_end_is_false() {
        let mut buf = vec![0u8; 4096];
        buf[4095] = 1;
        assert!(!is_all_zero(&buf));
    }

    #[test]
    fn is_all_zero_one_byte_nonzero_at_middle_is_false() {
        let mut buf = vec![0u8; 4096];
        buf[2048] = 0x42;
        assert!(!is_all_zero(&buf));
    }

    #[test]
    fn is_all_zero_handles_odd_length_buffer() {
        // 7 bytes → all-zero check must cover head (alignment) without
        // skipping over a non-zero byte.
        let mut buf = vec![0u8; 7];
        buf[6] = 0xFF;
        assert!(!is_all_zero(&buf));
    }

    #[test]
    fn sparse_writer_passes_through_nonzero_chunks_verbatim() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("dense.bin");
        let f = File::create(&p).unwrap();
        let mut w = SparseFileWriter::new(f, 1024);
        w.write_all(b"hello").unwrap();
        w.write_all(b" world").unwrap();
        let _ = w.finish().unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"hello world");
    }

    #[test]
    fn sparse_writer_seeks_over_zero_chunks_creating_holes() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("sparse.bin");
        let f = File::create(&p).unwrap();
        let mut w = SparseFileWriter::new(f, 1024);
        w.write_all(b"head").unwrap();
        w.write_all(&[0u8; 16 * 1024]).unwrap(); // hole
        w.write_all(b"tail").unwrap();
        let _ = w.finish().unwrap();
        // Logical file content: head + 16384 zeros + tail
        let mut got = Vec::new();
        File::open(&p).unwrap().read_to_end(&mut got).unwrap();
        let mut want = b"head".to_vec();
        want.extend(vec![0u8; 16 * 1024]);
        want.extend_from_slice(b"tail");
        assert_eq!(got, want);
    }

    #[test]
    fn sparse_writer_truncates_on_trailing_zero_chunk() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("trailing_holes.bin");
        let f = File::create(&p).unwrap();
        let mut w = SparseFileWriter::new(f, 1024);
        w.write_all(b"abc").unwrap();
        // Big zero tail — produces a hole at the end of the file.
        w.write_all(&[0u8; 8 * 1024]).unwrap();
        assert_eq!(w.position(), 3 + 8 * 1024);
        let _ = w.finish().unwrap();
        let meta = std::fs::metadata(&p).unwrap();
        // Final file length matches the logical position; the trailing
        // hole counts toward the file's length even though it consumed
        // no disk blocks.
        assert_eq!(meta.len(), 3 + 8 * 1024);
    }

    #[test]
    fn sparse_writer_position_reports_logical_length() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("a.bin");
        let f = File::create(&p).unwrap();
        let mut w = SparseFileWriter::new(f, 1024);
        w.write_all(&[0u8; 100]).unwrap();
        assert_eq!(w.position(), 100);
        w.write_all(b"x").unwrap();
        assert_eq!(w.position(), 101);
    }

    #[test]
    fn sparse_writer_chunk_size_hint_round_trips() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("h.bin");
        let f = File::create(&p).unwrap();
        let w = SparseFileWriter::new(f, 64 * 1024);
        assert_eq!(w.chunk_size_hint(), 64 * 1024);
    }

    #[test]
    fn sparse_writer_truncates_on_drop_without_finish() {
        // When SparseFileWriter sits inside a Box<dyn Write> the owner
        // can't call finish() — Drop must still truncate, otherwise
        // the trailing hole gets lost and the file appears shorter
        // than the logical content.
        let dir = tempdir().unwrap();
        let p = dir.path().join("dropped.bin");
        {
            let f = File::create(&p).unwrap();
            let mut w = SparseFileWriter::new(f, 1024);
            w.write_all(b"head").unwrap();
            w.write_all(&[0u8; 4096]).unwrap();
            // No w.finish() — let it drop.
        }
        let meta = std::fs::metadata(&p).unwrap();
        assert_eq!(meta.len(), 4 + 4096);
    }

    #[test]
    fn sparse_writer_empty_write_is_noop() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("e.bin");
        let f = File::create(&p).unwrap();
        let mut w = SparseFileWriter::new(f, 1024);
        let n = w.write(&[]).unwrap();
        assert_eq!(n, 0);
        assert_eq!(w.position(), 0);
    }
}
