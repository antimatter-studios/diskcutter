//! `DiskReader` — public facade for the decoder chain.
//!
//! Hides the entire `ReaderInterface` / `FormatTryOpen` / identify
//! machinery from consumers. The public surface is: `open`, `read`,
//! `format_chain` (for diagnostics), `layout` (bounded-prefix
//! partition probe).
//!
//! All four are intentionally minimal — anything richer (positioned
//! reads, full partition probes on compressed sources, etc.) belongs
//! in Phase 3 when consumers actually need it.

use std::io;
use std::path::Path;

use crate::inspect::{inspect_block_read, PartitionSummary};

use super::block_view::{slurp_prefix, PrefixBlockView, DEFAULT_PREFIX_BYTES};
use super::identify::identify_data_stream;
use super::interface::ReaderInterface;
use super::raw::RawFilehandle;
use super::READERS;

/// Streaming reader over a disk image — opaque w.r.t. compression.
/// Callers `open()` a path, then `read()` raw bytes; the layered
/// decompression is invisible.
pub struct DiskReader {
    chain: Box<dyn ReaderInterface>,
    labels: Vec<&'static str>,
}

impl DiskReader {
    /// Open `path` and resolve its decoder chain.
    ///
    /// Walks the registered formats once per layer until the source
    /// is raw bytes. `format_chain()` afterwards reports the labels
    /// seen, innermost first, always terminating in `"raw"`.
    pub fn open(path: &Path) -> io::Result<Self> {
        let leaf: Box<dyn ReaderInterface> = Box::new(RawFilehandle::open(path)?);
        let (chain, labels) = identify_data_stream(leaf, READERS)?;
        Ok(Self { chain, labels })
    }

    /// Construct from an existing `ReaderInterface` source. Used by
    /// the test suite to feed in-memory bytes through the same code
    /// path the file-backed `open()` exercises.
    #[cfg(test)]
    pub(crate) fn from_source(
        src: Box<dyn ReaderInterface>,
        registry: &[&'static dyn super::format::FormatTryOpen],
    ) -> io::Result<Self> {
        let (chain, labels) = identify_data_stream(src, registry)?;
        Ok(Self { chain, labels })
    }

    /// Read up to `buf.len()` bytes of raw (fully-decompressed) image
    /// data. Short reads legal, `Ok(0)` on EOF.
    pub fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.chain.read(buf)
    }

    /// Single-string label for the decoder chain, suitable for display in
    /// the inspect/queue UI.
    ///
    /// Shape:
    /// - raw leaf only          → `"RAW"`
    /// - one compression layer  → `"XZ"` / `"GZIP"` / `"BZIP2"` / `"ZSTD"`
    /// - multiple layers        → `"XZ over GZIP"` (outermost first)
    ///
    /// The trailing `"raw"` is omitted unless it is the *only* layer —
    /// every chain ends in raw, so surfacing it is redundant.
    pub fn format_label(&self) -> String {
        let layers: Vec<&'static str> = self.labels.to_vec();
        match layers.as_slice() {
            [single] => single.to_ascii_uppercase(),
            [_, ..] => {
                let compressed: Vec<String> = layers
                    .iter()
                    .filter(|l| **l != "raw")
                    .map(|l| l.to_ascii_uppercase())
                    .collect();
                if compressed.is_empty() {
                    "RAW".to_string()
                } else {
                    compressed.join(" over ")
                }
            }
            [] => "RAW".to_string(),
        }
    }

    /// The labels of the decoder layers in order, innermost first.
    /// Always ends in `"raw"`. For diagnostics — UI may surface as
    /// "xz → raw".
    pub fn format_chain(&self) -> Vec<&'static str> {
        self.labels.clone()
    }

    /// Slurp up to `limit` bytes from the chain into a freshly-built
    /// `BlockRead` view. The chain's read position advances past the
    /// slurped bytes; subsequent `read` calls return *post-prefix*
    /// content. Use this when you want both a partition probe AND to
    /// keep streaming the remainder (rare — most callers either probe
    /// OR burn).
    pub fn slurp_prefix(&mut self, limit: usize) -> io::Result<Vec<u8>> {
        slurp_prefix(&mut *self.chain, limit)
    }

    /// Slurp up to `DEFAULT_PREFIX_BYTES` of decompressed bytes and
    /// run the existing partition probe over them. Returns `None`
    /// when no partition table is found within the prefix (which is
    /// also what a single-filesystem image would yield).
    ///
    /// Consumes the prefix from the chain — calling `read` after
    /// `layout()` returns *post-prefix* bytes. Calling `layout()`
    /// twice on the same `DiskReader` would re-slurp from the
    /// already-advanced position; callers wanting both should call
    /// `layout()` first.
    pub fn layout(&mut self) -> io::Result<Option<PartitionSummary>> {
        let bytes = slurp_prefix(&mut *self.chain, DEFAULT_PREFIX_BYTES)?;
        let view = PrefixBlockView::new(bytes);
        Ok(inspect_block_read(&view))
    }
}

/// Lets a `DiskReader` slot directly into any `&mut dyn Read` API — in
/// particular the burn/verify pipeline.
impl io::Read for DiskReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.chain.read(buf)
    }
}
