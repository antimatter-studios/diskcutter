//! xz format adapter for the decoder chain.
//!
//! Magic: `fd 37 7a 58 5a 00` (6 bytes). On match the source is wrapped
//! in an [`xz2::read::XzDecoder`] whose `R` is our
//! [`ReadAdapter`] bridge, then re-exposed back through
//! [`ReaderInterface`] so the chain stays uniformly streaming.
//!
//! The old `src-tauri/src/readers/xz.rs` reader continues to coexist
//! and is what production currently uses; Phase 3 will migrate
//! consumers off it.

use super::adapter::ReadAdapter;
use crate::decoder_chain::format::{peek_and_rewind, FormatTryOpen};
use crate::decoder_chain::interface::ReaderInterface;
use std::io;
use xz2::read::XzDecoder;

/// xz magic — `xz` stream-header bytes per the file-format spec.
const XZ_MAGIC: [u8; 6] = [0xFD, 0x37, 0x7A, 0x58, 0x5A, 0x00];

/// Unit struct so the registry can hold a `&'static dyn FormatTryOpen`.
pub struct XzFormat;

/// Singleton entry referenced from [`crate::decoder_chain::READERS`].
pub static XZ_FORMAT: XzFormat = XzFormat;

impl FormatTryOpen for XzFormat {
    fn label(&self) -> &'static str {
        "xz"
    }

    fn try_open(
        &self,
        src: Box<dyn ReaderInterface>,
    ) -> Result<Box<dyn ReaderInterface>, Box<dyn ReaderInterface>> {
        let (peek, rewound) = match peek_and_rewind(src, XZ_MAGIC.len()) {
            Ok(v) => v,
            // I/O error during peek: there's nothing we can do to hand
            // the source back intact; surface it as a no-match against
            // an empty placeholder so the chain falls through to raw.
            Err(_) => return Err(Box::new(EmptyReader)),
        };
        if peek.len() < XZ_MAGIC.len() || peek[..XZ_MAGIC.len()] != XZ_MAGIC {
            return Err(rewound);
        }
        let decoder = XzDecoder::new(ReadAdapter::new(rewound));
        Ok(Box::new(XzStream { inner: decoder }))
    }
}

/// Streaming xz layer: produces decompressed bytes by delegating to
/// `xz2::read::XzDecoder`, which is in turn fed by our boxed source via
/// [`ReadAdapter`].
pub struct XzStream {
    inner: XzDecoder<ReadAdapter>,
}

impl ReaderInterface for XzStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        io::Read::read(&mut self.inner, buf)
    }
}

/// Fallback returned when [`peek_and_rewind`] itself errored mid-read
/// — the original source is no longer recoverable, so we hand back an
/// immediate-EOF reader. The identify loop will then label this as
/// `"raw"` and downstream `read()` will see `Ok(0)`.
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
    use std::io::Write;
    use xz2::write::XzEncoder;

    /// In-memory `ReaderInterface` over an owned byte buffer. Duplicate
    /// of the one in `decoder_chain::tests` so format submodules don't
    /// have to reach across module boundaries.
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

    /// Encode `payload` as a complete xz stream (header + block + index
    /// + footer).
    fn xz_bytes(payload: &[u8]) -> Vec<u8> {
        let mut e = XzEncoder::new(Vec::new(), 1);
        e.write_all(payload).unwrap();
        e.finish().unwrap()
    }

    /// Drain a `ReaderInterface` to EOF.
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

    #[test]
    fn try_open_rejects_non_xz_bytes() {
        let original = b"this is not an xz stream, just plain bytes".to_vec();
        let src: Box<dyn ReaderInterface> = Box::new(MemReader::new(original.clone()));
        match XZ_FORMAT.try_open(src) {
            Ok(_) => panic!("non-xz bytes must not be claimed by XzFormat"),
            Err(mut rewound) => {
                // Source must come back byte-identical after the peek.
                assert_eq!(drain(&mut *rewound), original);
            }
        }
    }

    #[test]
    fn try_open_accepts_and_decompresses_xz_stream() {
        let payload: Vec<u8> = (0..20_000u32).map(|i| (i % 256) as u8).collect();
        let compressed = xz_bytes(&payload);
        let src: Box<dyn ReaderInterface> = Box::new(MemReader::new(compressed));
        let mut wrapped = match XZ_FORMAT.try_open(src) {
            Ok(w) => w,
            Err(_) => panic!("xz magic must be recognised"),
        };
        assert_eq!(drain(&mut *wrapped), payload);
    }

    #[test]
    fn identify_data_stream_picks_up_xz_via_registry() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("payload.img.xz");
        let payload: Vec<u8> = (0..8_000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(&p, xz_bytes(&payload)).unwrap();

        let leaf = RawFilehandle::open(&p).unwrap();
        let src: Box<dyn ReaderInterface> = Box::new(leaf);
        let registry: &[&'static dyn FormatTryOpen] = &[&XZ_FORMAT];
        let (mut chain, labels) = identify_data_stream(src, registry).unwrap();
        assert_eq!(labels, vec!["xz", "raw"]);
        assert_eq!(drain(&mut *chain), payload);
    }
}
