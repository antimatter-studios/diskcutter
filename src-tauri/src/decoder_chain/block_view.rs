//! Bounded-prefix `BlockRead` adapter.
//!
//! Partition probing wants positioned reads (`read_at(offset, buf)`),
//! but the decoder chain is streaming. Bridge: pull a bounded prefix
//! from the chain (default 4 MiB), stash the bytes in a `Vec<u8>`, and
//! expose that vec through `BlockRead`.
//!
//! "Bounded prefix" is deliberate. A multi-GB compressed image would
//! cost minutes to fully decompress just to count partitions; the
//! partition table itself lives in the first sectors (MBR at byte 0;
//! GPT primary header at LBA 1 = byte 512). 4 MiB covers the table
//! plus enough headroom that filesystem sniffs of early-starting
//! partitions succeed. Partitions whose start offset is past the
//! prefix get `read_at` failures from `BlockRead`, which the
//! partition crate maps to `None` for that partition's filesystem —
//! acceptable per spec.

use fs_core::{BlockRead, Error, Result};

use super::interface::ReaderInterface;

/// Default prefix size: 4 MiB. Sized to comfortably hold:
/// - MBR (sector 0) + GPT primary header (sector 1) + entries (32
///   sectors after).
/// - The first sectors of partitions that start within the first
///   8192 sectors — covers ISO boot tracks and EFI System Partition
///   sniffs on typical images.
pub const DEFAULT_PREFIX_BYTES: usize = 4 * 1024 * 1024;

/// Read up to `limit` bytes from `src` into a fresh `Vec<u8>`. Stops
/// early on EOF. Short reads from the underlying chain are tolerated
/// — we loop until the buffer is full or EOF is observed.
pub fn slurp_prefix(src: &mut dyn ReaderInterface, limit: usize) -> std::io::Result<Vec<u8>> {
    let mut out = vec![0u8; limit];
    let mut filled = 0;
    while filled < limit {
        match src.read(&mut out[filled..])? {
            0 => break,
            n => filled += n,
        }
    }
    out.truncate(filled);
    Ok(out)
}

/// `BlockRead` over an owned byte buffer. The buffer is treated as
/// the *entire* device for purposes of `size_bytes` — callers using
/// this for partition probing get an "image" sized to whatever prefix
/// they slurped.
pub struct PrefixBlockView {
    bytes: Vec<u8>,
}

impl PrefixBlockView {
    pub fn new(bytes: Vec<u8>) -> Self {
        Self { bytes }
    }
}

impl BlockRead for PrefixBlockView {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        let off = offset as usize;
        let end = off.checked_add(buf.len()).ok_or_else(|| {
            Error::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "offset+len overflow",
            ))
        })?;
        if end > self.bytes.len() {
            return Err(Error::Io(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "read past bounded prefix",
            )));
        }
        buf.copy_from_slice(&self.bytes[off..end]);
        Ok(())
    }

    fn size_bytes(&self) -> u64 {
        self.bytes.len() as u64
    }
}
