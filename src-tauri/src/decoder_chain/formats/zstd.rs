//! Zstandard support for the decoder chain.
//!
//! Matches on the 4-byte zstd magic (`28 b5 2f fd`, RFC 8478 §3.1.1)
//! and wraps the source in a streaming `zstd::Decoder`. The decoder
//! expects `BufRead`, so the boxed source is funnelled through the
//! shared `Read` adapter and then a `BufReader`.

use std::io;

use ::zstd::stream::Decoder as ZstdDecoder;

use super::adapter::ReadAdapter;
use crate::decoder_chain::format::{peek_and_rewind, FormatTryOpen};
use crate::decoder_chain::interface::ReaderInterface;

/// zstd frame magic — first 4 bytes of every zstd stream.
const ZSTD_MAGIC: [u8; 4] = [0x28, 0xB5, 0x2F, 0xFD];

/// Unit struct registering zstd with the decoder chain.
pub struct ZstdFormat;

/// Singleton entry for the `READERS` static.
pub static ZSTD_FORMAT: ZstdFormat = ZstdFormat;

impl FormatTryOpen for ZstdFormat {
    fn label(&self) -> &'static str {
        "zstd"
    }

    fn try_open(
        &self,
        src: Box<dyn ReaderInterface>,
    ) -> Result<Box<dyn ReaderInterface>, Box<dyn ReaderInterface>> {
        let (peek, rewound) = match peek_and_rewind(src, ZSTD_MAGIC.len()) {
            Ok(v) => v,
            // I/O error during peek — surface as no-match. The caller
            // gets a fresh-from-zero stream synthesised below; we can't
            // hand the original back because we never got it. In
            // practice this branch is unreachable for our leaf reader
            // (it returns EOF, not Err, on short files).
            Err(_) => return Err(Box::new(EmptyReader)),
        };
        if peek.len() < ZSTD_MAGIC.len() || peek[..ZSTD_MAGIC.len()] != ZSTD_MAGIC {
            return Err(rewound);
        }
        // Match: build the streaming decoder over the rewound source.
        // `zstd::Decoder::new` takes a `Read` and internally wraps it
        // in `BufReader` — passing our `ReadAdapter` (`Read` impl)
        // directly yields the `Decoder<'_, BufReader<ReadAdapter>>`
        // shape the struct field expects.
        let as_read = ReadAdapter::new(rewound);
        match ZstdDecoder::new(as_read) {
            Ok(dec) => Ok(Box::new(ZstdStream { inner: dec })),
            // Header validated to magic but the decoder still rejected
            // it — corrupt stream. We've already consumed the rewound
            // source into the decoder constructor, so there's nothing
            // to hand back. Convention: treat as no-match against a
            // synthetic empty reader, letting the chain terminate at
            // raw with zero bytes. Real files do not hit this.
            Err(_) => Err(Box::new(EmptyReader)),
        }
    }
}

/// Streaming-zstd layer. Owns the `zstd::Decoder` and forwards `read`
/// through to it.
pub struct ZstdStream {
    inner: ZstdDecoder<'static, io::BufReader<ReadAdapter>>,
}

impl ReaderInterface for ZstdStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        io::Read::read(&mut self.inner, buf)
    }
}

/// Fallback `ReaderInterface` used only when `try_open` cannot
/// preserve the input (peek error, or corrupt header after magic
/// match). Always returns EOF.
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
    use std::io::{Cursor, Write};

    /// In-memory `ReaderInterface` over a byte slice. Duplicate of the
    /// helper in `decoder_chain::tests` because that one is private to
    /// the outer module.
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
        let mut buf = [0u8; 4096];
        loop {
            match r.read(&mut buf).unwrap() {
                0 => break,
                n => out.extend_from_slice(&buf[..n]),
            }
        }
        out
    }

    fn zst_encode(payload: &[u8]) -> Vec<u8> {
        ::zstd::bulk::compress(payload, 0).unwrap()
    }

    #[test]
    fn try_open_rejects_non_zstd_bytes() {
        let body = b"not really a zstd stream at all".to_vec();
        let src: Box<dyn ReaderInterface> = Box::new(MemReader::new(body.clone()));
        let result = ZSTD_FORMAT.try_open(src);
        match result {
            Ok(_) => panic!("expected zstd to reject non-zstd bytes"),
            Err(mut rewound) => {
                let drained = drain(&mut *rewound);
                assert_eq!(
                    drained, body,
                    "rewound source must yield the original bytes"
                );
            }
        }
    }

    #[test]
    fn try_open_accepts_and_decompresses_zstd_stream() {
        let payload: Vec<u8> = (0..32_768u32).map(|i| (i % 251) as u8).collect();
        let compressed = zst_encode(&payload);
        let src: Box<dyn ReaderInterface> = Box::new(MemReader::new(compressed));
        let wrapped = ZSTD_FORMAT
            .try_open(src)
            .map_err(|_| "expected zstd to accept the magic bytes")
            .unwrap();
        let mut wrapped = wrapped;
        let decompressed = drain(&mut *wrapped);
        assert_eq!(decompressed, payload);
    }

    #[test]
    fn identify_data_stream_picks_up_zstd_via_registry() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("payload.bin.zst");
        let payload: Vec<u8> = (0..16_000u32).map(|i| (i % 256) as u8).collect();
        let compressed = zst_encode(&payload);
        std::fs::File::create(&p)
            .unwrap()
            .write_all(&compressed)
            .unwrap();

        let leaf: Box<dyn ReaderInterface> = Box::new(RawFilehandle::open(&p).unwrap());
        let registry: &[&'static dyn FormatTryOpen] = &[&ZSTD_FORMAT];
        let (mut chain, labels) =
            identify_data_stream(leaf, registry, &crate::joblog::NullLogger).unwrap();
        assert_eq!(labels, vec!["zstd", "raw"]);
        assert_eq!(drain(&mut *chain), payload);
    }
}
