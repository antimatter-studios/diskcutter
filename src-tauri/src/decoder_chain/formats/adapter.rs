//! Bridge between [`super::super::interface::ReaderInterface`] and the
//! standard library's [`std::io::Read`].
//!
//! Decompression crates (`flate2`, `xz2`, `bzip2`, `zstd`, …) all
//! generically wrap a `R: Read`. Our chain, however, traffics in
//! `Box<dyn ReaderInterface>` so layers can be registered uniformly.
//! `ReadAdapter` is the thin newtype that lets a boxed
//! `ReaderInterface` masquerade as `std::io::Read` so it can be fed
//! into one of those decoders.
//!
//! The adapter intentionally lives in `formats/` rather than at the
//! top of `decoder_chain/` — every Phase 2 format needs it, but nothing
//! outside the format implementations does.

use super::super::interface::ReaderInterface;
use std::io::{self, Read};

/// Adapts a `Box<dyn ReaderInterface>` to `std::io::Read` for use with
/// generic decoder wrappers (`GzDecoder<R: Read>`, etc.).
pub(crate) struct ReadAdapter {
    pub(crate) inner: Box<dyn ReaderInterface>,
}

impl ReadAdapter {
    pub(crate) fn new(inner: Box<dyn ReaderInterface>) -> Self {
        Self { inner }
    }
}

impl Read for ReadAdapter {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.read(buf)
    }
}
