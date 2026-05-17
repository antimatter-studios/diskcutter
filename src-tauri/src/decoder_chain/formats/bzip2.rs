//! bzip2 format adapter for the decoder chain.
//!
//! Magic: `42 5a 68` ("BZh") — the three-byte header that begins every
//! bzip2 stream. We peek that many bytes, compare, and on match wrap
//! the (rewound) source in a `bzip2::read::BzDecoder` via the shared
//! `ReadAdapter` adapter.

use bzip2::read::BzDecoder;
use std::io;

use super::adapter::ReadAdapter;
use crate::decoder_chain::format::{peek_and_rewind, FormatTryOpen};
use crate::decoder_chain::interface::ReaderInterface;

/// bzip2 stream magic: "BZh".
const BZIP2_MAGIC: &[u8; 3] = b"BZh";

/// Zero-sized format marker. Registered as `&BZIP2_FORMAT` in the
/// `READERS` slice.
pub struct Bzip2Format;

/// Static singleton — the registry holds a reference to this.
pub static BZIP2_FORMAT: Bzip2Format = Bzip2Format;

impl FormatTryOpen for Bzip2Format {
    fn label(&self) -> &'static str {
        "bzip2"
    }

    fn try_open(
        &self,
        src: Box<dyn ReaderInterface>,
    ) -> Result<Box<dyn ReaderInterface>, Box<dyn ReaderInterface>> {
        // Peek the first 3 bytes; rewind preserves them in the returned
        // reader so either branch can resume the stream from byte 0.
        let (peeked, rewound) = match peek_and_rewind(src, BZIP2_MAGIC.len()) {
            Ok(v) => v,
            // peek_and_rewind only fails when the underlying read
            // itself errored. There's no source left to hand back, so
            // we synthesise an empty reader — the identify loop will
            // walk past it and terminate at "raw".
            Err(_) => return Err(Box::new(EmptyReader)),
        };
        if peeked.as_slice() == BZIP2_MAGIC.as_slice() {
            let bridged = ReadAdapter::new(rewound);
            let dec = BzDecoder::new(bridged);
            Ok(Box::new(Bzip2Stream { inner: dec }))
        } else {
            Err(rewound)
        }
    }
}

/// Streaming bzip2 decoder over a chained source.
///
/// Holds the `BzDecoder` directly; `read` forwards into it. Bz2 streams
/// can be multi-member but `BzDecoder` handles that internally, so
/// there's nothing to manage at this layer.
pub struct Bzip2Stream {
    inner: BzDecoder<ReadAdapter>,
}

impl ReaderInterface for Bzip2Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        io::Read::read(&mut self.inner, buf)
    }
}

/// Fallback reader used only when `peek_and_rewind` itself errors — in
/// that case the source is unrecoverable, so an immediate-EOF reader is
/// the safest thing to feed back to the identify loop.
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
    use ::bzip2::write::BzEncoder;
    use ::bzip2::Compression;
    use std::io::{Cursor, Write};

    /// In-memory `ReaderInterface` over a byte slice. Mirrors the shape
    /// used by Phase 1's `decoder_chain::tests::MemReader` — duplicated
    /// here because that helper is `pub(super)` and unreachable from a
    /// child module.
    struct MemReader {
        cursor: Cursor<Vec<u8>>,
    }

    impl MemReader {
        fn new(bytes: Vec<u8>) -> Self {
            Self {
                cursor: Cursor::new(bytes),
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

    fn bz_compress(payload: &[u8]) -> Vec<u8> {
        let mut e = BzEncoder::new(Vec::new(), Compression::default());
        e.write_all(payload).unwrap();
        e.finish().unwrap()
    }

    #[test]
    fn try_open_rejects_non_bzip2_bytes() {
        let original = b"not bzip2 content, just plain bytes".to_vec();
        let src: Box<dyn ReaderInterface> = Box::new(MemReader::new(original.clone()));
        match BZIP2_FORMAT.try_open(src) {
            Ok(_) => panic!("Bzip2Format claimed a non-bzip2 source"),
            Err(mut rewound) => {
                // Rewound reader must still yield the original bytes
                // verbatim — the peek-and-rewind contract.
                let drained = drain(&mut *rewound);
                assert_eq!(drained, original);
            }
        }
    }

    #[test]
    fn try_open_accepts_and_decompresses_bzip2_stream() {
        let payload: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
        let compressed = bz_compress(&payload);
        let src: Box<dyn ReaderInterface> = Box::new(MemReader::new(compressed));
        let mut wrapped = match BZIP2_FORMAT.try_open(src) {
            Ok(w) => w,
            Err(_) => panic!("Bzip2Format rejected a valid bzip2 stream"),
        };
        let drained = drain(&mut *wrapped);
        assert_eq!(drained, payload);
    }

    #[test]
    fn identify_data_stream_picks_up_bzip2_via_registry() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("payload.img.bz2");
        let payload: Vec<u8> = (0..8192u32)
            .map(|i| (i.wrapping_mul(7) % 256) as u8)
            .collect();
        let compressed = bz_compress(&payload);
        std::fs::File::create(&p)
            .unwrap()
            .write_all(&compressed)
            .unwrap();

        let leaf: Box<dyn ReaderInterface> = Box::new(RawFilehandle::open(&p).unwrap());
        let registry: &[&'static dyn FormatTryOpen] = &[&BZIP2_FORMAT];
        let (mut chain, labels) = identify_data_stream(leaf, registry).unwrap();
        assert_eq!(labels, vec!["bzip2", "raw"]);
        let drained = drain(&mut *chain);
        assert_eq!(drained, payload);
    }
}
