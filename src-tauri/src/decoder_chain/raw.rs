//! `RawFilehandle` — the leaf reader at the bottom of every chain.
//!
//! Wraps a `std::fs::File` and exposes it through `ReaderInterface`.
//! When `identify_data_stream` runs and no format matches, what comes
//! out is a chain that ends in one of these — its `read()` produces
//! raw on-disk bytes, suitable for an uncompressed `.iso` / `.img`.

use super::interface::ReaderInterface;
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

/// Concrete leaf: a buffered handle on a regular file.
pub struct RawFilehandle {
    inner: io::BufReader<File>,
}

impl RawFilehandle {
    /// Open `path` for reading. Errors propagate the underlying
    /// `std::fs::File::open` error verbatim — caller decides how to
    /// present it.
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        Ok(Self {
            inner: io::BufReader::new(file),
        })
    }
}

impl ReaderInterface for RawFilehandle {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.read(buf)
    }
}
