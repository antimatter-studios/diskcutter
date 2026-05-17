//! Gzip format for the decoder chain.
//!
//! Matches the 2-byte gzip magic (`1f 8b`) and, on match, wraps the
//! source in a [`flate2::read::GzDecoder`] adapted via
//! [`super::adapter::ReadAdapter`] so its output flows as a
//! [`ReaderInterface`].

use std::io::{self, Read};

use flate2::read::GzDecoder;

use super::super::format::{peek_and_rewind, FormatTryOpen};
use super::super::interface::ReaderInterface;
use super::adapter::ReadAdapter;

/// First two bytes of any valid gzip stream (RFC 1952 §2.3.1).
const GZIP_MAGIC: [u8; 2] = [0x1f, 0x8b];

/// Zero-sized type whose `'static` reference goes into `READERS`.
pub struct GzipFormat;

/// Static handle registered in `decoder_chain::READERS`.
pub static GZIP_FORMAT: GzipFormat = GzipFormat;

impl FormatTryOpen for GzipFormat {
    fn label(&self) -> &'static str {
        "gzip"
    }

    fn try_open(
        &self,
        src: Box<dyn ReaderInterface>,
    ) -> Result<Box<dyn ReaderInterface>, Box<dyn ReaderInterface>> {
        // peek_and_rewind is the agreed contract: never destructive on
        // a no-match path.
        let (peek, rewound) = match peek_and_rewind(src, GZIP_MAGIC.len()) {
            Ok(v) => v,
            // EOF or IO error before we could read the magic — fall
            // back to no-match with an empty rewound source. The
            // identify loop will then terminate at "raw".
            Err(_) => return Err(Box::new(EmptyReader)),
        };
        if peek.len() < GZIP_MAGIC.len() || peek[..GZIP_MAGIC.len()] != GZIP_MAGIC {
            return Err(rewound);
        }
        let decoder = GzDecoder::new(ReadAdapter::new(rewound));
        Ok(Box::new(GzipStream { inner: decoder }))
    }
}

/// Streaming gzip decoder. Implements [`ReaderInterface`] by delegating
/// to the wrapped [`GzDecoder`]; the decoder pulls compressed bytes
/// through [`ReadAdapter`] from whatever sits below in the chain.
pub struct GzipStream {
    inner: GzDecoder<ReadAdapter>,
}

impl ReaderInterface for GzipStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.read(buf)
    }
}

/// Used only on the rare error path in `try_open` where `peek_and_rewind`
/// itself failed and we have no source left to hand back. Yields EOF
/// immediately so the identify loop falls through to "raw" cleanly.
struct EmptyReader;

impl ReaderInterface for EmptyReader {
    fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
        Ok(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decoder_chain::identify::identify_data_stream;
    use crate::decoder_chain::raw::RawFilehandle;
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use std::io::Write;

    /// Build an in-memory gzip-compressed buffer of `payload`.
    fn gz_bytes(payload: &[u8]) -> Vec<u8> {
        let mut e = GzEncoder::new(Vec::new(), Compression::default());
        e.write_all(payload).unwrap();
        e.finish().unwrap()
    }

    /// In-memory `ReaderInterface` over a byte slice — the test-suite
    /// stand-in for `RawFilehandle`. Mirrors the helper in
    /// `decoder_chain::tests`, redeclared locally because that one is
    /// private to its `tests` submodule.
    struct MemReader {
        cursor: io::Cursor<Vec<u8>>,
    }

    impl MemReader {
        fn new(bytes: Vec<u8>) -> Self {
            Self {
                cursor: io::Cursor::new(bytes),
            }
        }
    }

    impl ReaderInterface for MemReader {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            io::Read::read(&mut self.cursor, buf)
        }
    }

    fn drain(r: &mut dyn ReaderInterface) -> Vec<u8> {
        let mut out = Vec::new();
        let mut buf = [0u8; 256];
        loop {
            match r.read(&mut buf).unwrap() {
                0 => break,
                n => out.extend_from_slice(&buf[..n]),
            }
        }
        out
    }

    #[test]
    fn try_open_rejects_non_gzip_bytes() {
        let original: Vec<u8> = b"this is plainly not gzip-shaped".to_vec();
        let src: Box<dyn ReaderInterface> = Box::new(MemReader::new(original.clone()));
        let result = GZIP_FORMAT.try_open(src);
        let mut rewound = match result {
            Ok(_) => panic!("gzip should not have matched plain bytes"),
            Err(r) => r,
        };
        // Rewound source must still yield the original bytes
        // byte-for-byte — peek_and_rewind contract.
        assert_eq!(drain(&mut *rewound), original);
    }

    #[test]
    fn try_open_accepts_and_decompresses_gzip_stream() {
        let payload: Vec<u8> = (0..10_000u32).map(|i| (i % 251) as u8).collect();
        let compressed = gz_bytes(&payload);
        let src: Box<dyn ReaderInterface> = Box::new(MemReader::new(compressed));
        let mut wrapped = GZIP_FORMAT
            .try_open(src)
            .unwrap_or_else(|_| panic!("gzip magic should have matched"));
        assert_eq!(drain(&mut *wrapped), payload);
    }

    #[test]
    fn identify_data_stream_picks_up_gzip_via_registry() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("disk.img.gz");
        let payload: Vec<u8> = (0..4_096u32).map(|i| (i & 0xff) as u8).collect();
        std::fs::write(&p, gz_bytes(&payload)).unwrap();

        let leaf: Box<dyn ReaderInterface> = Box::new(RawFilehandle::open(&p).unwrap());
        let registry: &[&'static dyn FormatTryOpen] = &[&GZIP_FORMAT];
        let (mut chain, labels) = identify_data_stream(leaf, registry).unwrap();
        assert_eq!(labels, vec!["gzip", "raw"]);
        assert_eq!(drain(&mut *chain), payload);
    }
}
