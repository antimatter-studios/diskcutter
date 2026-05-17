//! Decoder chain — recursive reader-discovery for compressed disk
//! images.
//!
//! Given a path to a disk image (possibly nested under one or more
//! compression layers, e.g. `image.iso.xz`), this module produces a
//! streaming reader that yields raw on-disk bytes. Compression
//! formats are registered statically; the chain peels them one at a
//! time until no format matches.
//!
//! ## Architecture
//!
//! - [`interface::ReaderInterface`] — the single streaming trait every
//!   layer (leaf or wrapper) implements.
//! - [`format::FormatTryOpen`] — the entry trait each compression
//!   format implements; offered the source, it either matches and
//!   wraps, or rejects (with the source rewound).
//! - [`raw::RawFilehandle`] — the leaf reader; wraps a `std::fs::File`.
//! - [`identify::identify_data_stream`] — the recursion engine that
//!   walks the registry until no format matches.
//!
//! ## Adding a format (for Phase 2 agents)
//!
//! 1. Create a unit struct implementing [`format::FormatTryOpen`].
//! 2. In `try_open`, call [`format::peek_and_rewind`] to look at the
//!    first ~16 bytes for the magic number.
//! 3. On match: wrap the rewound source in your streaming decoder
//!    (which itself implements [`interface::ReaderInterface`]) and
//!    return `Ok(boxed)`.
//! 4. On no match: return `Err(rewound)` unchanged.
//! 5. Add a `&FOO_FACTORY` entry to the [`READERS`] slice below in
//!    the order you want it tried.
//!
//! Standard magic-number lengths for the formats Phase 2 will add:
//! gzip 2 bytes (`1f 8b`), xz 6 bytes (`fd 37 7a 58 5a 00`), bzip2 3
//! bytes (`42 5a 68`), zstd 4 bytes (`28 b5 2f fd`). Peeking 16 is
//! comfortably above all of them.

pub mod block_view;
pub mod disk_reader;
pub mod format;
pub mod formats;
pub mod identify;
pub mod interface;
pub mod raw;

pub use disk_reader::DiskReader;
pub use format::{peek_and_rewind, FormatTryOpen};
pub use interface::ReaderInterface;
pub use raw::RawFilehandle;

/// Registered formats, tried in order. Order doesn't affect correctness
/// (each format has its own magic) but defines the diagnostic chain
/// labels for stacked sources.
pub static READERS: &[&'static dyn FormatTryOpen] = &[
    &formats::xz::XZ_FORMAT,
    &formats::gzip::GZIP_FORMAT,
    &formats::bzip2::BZIP2_FORMAT,
    &formats::zstd::ZSTD_FORMAT,
];

#[cfg(test)]
mod tests {
    use super::*;
    use format::{peek_and_rewind, FormatTryOpen};
    use identify::identify_data_stream;
    use interface::ReaderInterface;
    use std::io::{self, Write};

    /// In-memory `ReaderInterface` over a byte slice — the test-suite
    /// stand-in for `RawFilehandle`.
    pub(super) struct MemReader {
        cursor: io::Cursor<Vec<u8>>,
    }

    impl MemReader {
        pub(super) fn new(bytes: Vec<u8>) -> Self {
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

    /// Test-only fake format. Claims any source whose first byte
    /// matches `magic`; on match strips one byte and yields the
    /// remainder. Used to prove the identify loop wraps once and
    /// stacks correctly.
    pub(super) struct FakeFormat {
        pub(super) label: &'static str,
        pub(super) magic: u8,
    }

    impl FormatTryOpen for FakeFormat {
        fn label(&self) -> &'static str {
            self.label
        }

        fn try_open(
            &self,
            src: Box<dyn ReaderInterface>,
        ) -> Result<Box<dyn ReaderInterface>, Box<dyn ReaderInterface>> {
            let (peek, rewound) = match peek_and_rewind(src, 1) {
                Ok(v) => v,
                Err(_) => return Err(Box::new(MemReader::new(Vec::new()))),
            };
            if peek.first().copied() == Some(self.magic) {
                Ok(Box::new(StripOne {
                    inner: rewound,
                    stripped: false,
                }))
            } else {
                Err(rewound)
            }
        }
    }

    /// Decoder side of `FakeFormat`: drops exactly one byte then
    /// passes through.
    struct StripOne {
        inner: Box<dyn ReaderInterface>,
        stripped: bool,
    }

    impl ReaderInterface for StripOne {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if !self.stripped {
                let mut throwaway = [0u8; 1];
                let n = self.inner.read(&mut throwaway)?;
                if n == 0 {
                    return Ok(0);
                }
                self.stripped = true;
            }
            self.inner.read(buf)
        }
    }

    /// Drain a `ReaderInterface` fully into a `Vec<u8>`.
    pub(super) fn drain(r: &mut dyn ReaderInterface) -> Vec<u8> {
        let mut out = Vec::new();
        let mut buf = [0u8; 64];
        loop {
            match r.read(&mut buf).unwrap() {
                0 => break,
                n => out.extend_from_slice(&buf[..n]),
            }
        }
        out
    }

    /// In-test logger that records every entry so assertions can see what
    /// the chain reported. Mirrors the `RecordingLogger` in joblog's own
    /// test module — that one isn't reachable from here, so we keep a
    /// small copy for chain-level diagnostics.
    struct DiagRecorder {
        entries: std::sync::Mutex<Vec<(crate::joblog::LogLevel, String)>>,
    }
    impl DiagRecorder {
        fn new() -> Self {
            Self {
                entries: std::sync::Mutex::new(Vec::new()),
            }
        }
        fn lines(&self) -> Vec<String> {
            self.entries
                .lock()
                .unwrap()
                .iter()
                .map(|(lv, m)| format!("[{}] {m}", lv.as_str()))
                .collect()
        }
    }
    impl crate::joblog::JobLogger for DiagRecorder {
        fn log(&self, level: crate::joblog::LogLevel, message: &str) {
            self.entries
                .lock()
                .unwrap()
                .push((level, message.to_string()));
        }
        fn debug_enabled(&self) -> bool {
            true
        }
    }

    /// End-to-end shape check for `.img.xz`: exactly the case the user is
    /// asking about. Builds a real xz-compressed payload, opens it through
    /// `DiskReader::open_with_log`, and asserts the chain reports the
    /// expected two-link result (`xz → raw`) — no phantom "img" layer,
    /// because `.img` is a filename convention, not a format.
    #[test]
    fn img_xz_chain_yields_xz_then_raw_with_expected_log_lines() {
        use std::io::Write;
        use xz2::write::XzEncoder;
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("payload.img.xz");
        let payload: Vec<u8> = (0..4_096u32).map(|i| (i % 251) as u8).collect();
        let mut e = XzEncoder::new(Vec::new(), 1);
        e.write_all(&payload).unwrap();
        std::fs::write(&p, e.finish().unwrap()).unwrap();

        let rec = DiagRecorder::new();
        let mut dr = DiskReader::open_with_log(&p, &rec).expect("open_with_log");
        assert_eq!(dr.format_chain(), vec!["xz", "raw"]);

        // Drain to prove decompression works end-to-end.
        let mut out = Vec::new();
        let mut buf = [0u8; 128];
        loop {
            match dr.read(&mut buf).unwrap() {
                0 => break,
                n => out.extend_from_slice(&buf[..n]),
            }
        }
        assert_eq!(out, payload);

        // Verify the log trail surfaces every link the user expects to
        // see. Substring matching keeps the test resilient to wording.
        let lines = rec.lines();
        let dump = lines.join("\n");
        let must_have = [
            "opening",
            "head[0..",
            "matched layer 0 = xz",
            "no format claimed depth=1, terminating at raw",
            "xz → raw",
        ];
        for needle in must_have {
            assert!(
                lines.iter().any(|l| l.contains(needle)),
                "expected a log line containing {needle:?}, got:\n{dump}"
            );
        }
    }

    #[test]
    fn raw_filehandle_reads_bytes_from_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("payload.bin");
        let body: Vec<u8> = (0u8..200).collect();
        std::fs::File::create(&p).unwrap().write_all(&body).unwrap();
        let mut leaf = RawFilehandle::open(&p).unwrap();
        let drained = drain(&mut leaf);
        assert_eq!(drained, body);
    }

    #[test]
    fn peek_and_rewind_yields_same_bytes() {
        let original: Vec<u8> = (0u8..32).collect();
        let src: Box<dyn ReaderInterface> = Box::new(MemReader::new(original.clone()));
        let (peeked, mut rewound) = peek_and_rewind(src, 4).unwrap();
        assert_eq!(peeked, &original[..4]);
        let drained = drain(&mut *rewound);
        assert_eq!(drained, original);
    }

    #[test]
    fn identify_with_empty_registry_returns_source_unchanged() {
        let body: Vec<u8> = (10u8..50).collect();
        let src: Box<dyn ReaderInterface> = Box::new(MemReader::new(body.clone()));
        let registry: &[&'static dyn FormatTryOpen] = &[];
        let (mut chain, labels) =
            identify_data_stream(src, registry, &crate::joblog::NullLogger).unwrap();
        assert_eq!(labels, vec!["raw"]);
        assert_eq!(drain(&mut *chain), body);
    }

    pub(super) static FAKE_A: FakeFormat = FakeFormat {
        label: "fakeA",
        magic: 0xAA,
    };
    pub(super) static FAKE_B: FakeFormat = FakeFormat {
        label: "fakeB",
        magic: 0xBB,
    };

    #[test]
    fn identify_with_one_format_wraps_once_then_stops() {
        let mut body = vec![0xAAu8];
        body.extend_from_slice(b"hello world");
        let src: Box<dyn ReaderInterface> = Box::new(MemReader::new(body.clone()));
        let registry: &[&'static dyn FormatTryOpen] = &[&FAKE_A];
        let (mut chain, labels) =
            identify_data_stream(src, registry, &crate::joblog::NullLogger).unwrap();
        assert_eq!(labels, vec!["fakeA", "raw"]);
        assert_eq!(drain(&mut *chain), b"hello world");
    }

    #[test]
    fn identify_with_two_formats_stacks_correctly() {
        let mut body = vec![0xBBu8, 0xAAu8];
        body.extend_from_slice(b"payload");
        let src: Box<dyn ReaderInterface> = Box::new(MemReader::new(body.clone()));
        let registry: &[&'static dyn FormatTryOpen] = &[&FAKE_A, &FAKE_B];
        let (mut chain, labels) =
            identify_data_stream(src, registry, &crate::joblog::NullLogger).unwrap();
        assert_eq!(labels, vec!["fakeB", "fakeA", "raw"]);
        assert_eq!(drain(&mut *chain), b"payload");
    }

    #[test]
    fn identify_no_match_leaves_source_untouched() {
        let body = vec![0xCCu8, 1, 2, 3, 4];
        let src: Box<dyn ReaderInterface> = Box::new(MemReader::new(body.clone()));
        let registry: &[&'static dyn FormatTryOpen] = &[&FAKE_A, &FAKE_B];
        let (mut chain, labels) =
            identify_data_stream(src, registry, &crate::joblog::NullLogger).unwrap();
        assert_eq!(labels, vec!["raw"]);
        assert_eq!(drain(&mut *chain), body);
    }

    #[test]
    fn disk_reader_open_round_trips_known_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("img.bin");
        let body: Vec<u8> = (0u8..128).collect();
        std::fs::File::create(&p).unwrap().write_all(&body).unwrap();
        let mut dr = DiskReader::open(&p).unwrap();
        assert_eq!(dr.format_chain(), vec!["raw"]);
        let mut out = Vec::new();
        let mut buf = [0u8; 32];
        loop {
            match dr.read(&mut buf).unwrap() {
                0 => break,
                n => out.extend_from_slice(&buf[..n]),
            }
        }
        assert_eq!(out, body);
    }

    #[test]
    fn disk_reader_from_source_with_fake_format_drains_post_decode() {
        let mut body = vec![0xAAu8];
        body.extend_from_slice(b"raw image bytes");
        let src: Box<dyn ReaderInterface> = Box::new(MemReader::new(body));
        let registry: &[&'static dyn FormatTryOpen] = &[&FAKE_A];
        let mut dr = DiskReader::from_source(src, registry).unwrap();
        assert_eq!(dr.format_chain(), vec!["fakeA", "raw"]);
        let mut out = Vec::new();
        let mut buf = [0u8; 8];
        loop {
            match dr.read(&mut buf).unwrap() {
                0 => break,
                n => out.extend_from_slice(&buf[..n]),
            }
        }
        assert_eq!(out, b"raw image bytes");
    }

    #[test]
    fn prefix_block_view_reads_slurped_bytes() {
        use super::block_view::{slurp_prefix, PrefixBlockView};
        use fs_core::BlockRead;
        let body: Vec<u8> = (0u8..200).collect();
        let mut src: Box<dyn ReaderInterface> = Box::new(MemReader::new(body.clone()));
        let prefix = slurp_prefix(&mut *src, 64).unwrap();
        assert_eq!(prefix.len(), 64);
        assert_eq!(prefix, &body[..64]);
        let view = PrefixBlockView::new(prefix);
        assert_eq!(view.size_bytes(), 64);
        let mut buf = [0u8; 16];
        view.read_at(8, &mut buf).unwrap();
        assert_eq!(buf, body[8..24]);
        let mut overflow = [0u8; 8];
        assert!(view.read_at(60, &mut overflow).is_err());
    }
}
