//! `FormatTryOpen` — the registry entry point each compression format
//! implements. The identify loop walks the static `READERS` slice and
//! asks each one "is this you?".
//!
//! Match contract:
//! - On match: consume `src`, wrap it, return `Ok(wrapped)`.
//! - On no match: rewind any bytes the matcher peeked and return
//!   `Err(src)` so the next format can try.
//!
//! The rewind is the subtle part. A naive implementation that just
//! reads N bytes and then hands the source back has *lost* those
//! bytes. The standard pattern — provided here as `peek_and_rewind` —
//! is to read into a small buffer, then re-prepend that buffer in
//! front of the source via a `Cursor + Chain` adapter. The returned
//! reader is byte-for-byte equivalent to the input; the format check
//! is non-destructive from the caller's point of view.
//!
//! Phase 2 agents call `peek_and_rewind` first, inspect the magic
//! bytes, and either match (wrap the rewound source in their decoder)
//! or no-match (hand the rewound source straight back).

use super::interface::ReaderInterface;
use std::io::{self, Read};

/// Static list-entry trait for an identifiable stream format.
///
/// `Sync` so the registry can hold `&'static dyn FormatTryOpen` in a
/// `static` slice; no interior mutability is required since each
/// implementer is a zero-sized unit struct or holds only `'static`
/// data.
pub trait FormatTryOpen: Sync {
    /// Short diagnostic name used in `format_chain()` output (e.g.
    /// `"xz"`, `"gz"`). Stable across releases — frontend may match on
    /// it.
    fn label(&self) -> &'static str;

    /// Attempt to claim `src`. On success return the wrapped reader;
    /// on no-match return `src` with any peeked bytes rewound (use
    /// `peek_and_rewind` to make that easy).
    fn try_open(
        &self,
        src: Box<dyn ReaderInterface>,
    ) -> Result<Box<dyn ReaderInterface>, Box<dyn ReaderInterface>>;
}

/// Peek up to `n` bytes from `src`, then return a reader that yields
/// those bytes followed by the remainder of `src`. The returned reader
/// is observationally identical to the original — useful for magic
/// sniffing without consuming the stream.
///
/// `buf_out` is filled with the peeked bytes (truncated to whatever
/// could actually be read; EOF before `n` is not an error). The
/// returned reader still includes those peeked bytes — i.e. they
/// appear in *both* `buf_out` and the future reads. That's the right
/// shape: the matcher inspects `buf_out`, then either consumes the
/// returned reader (match) or hands it back (no match).
pub fn peek_and_rewind(
    mut src: Box<dyn ReaderInterface>,
    n: usize,
) -> io::Result<(Vec<u8>, Box<dyn ReaderInterface>)> {
    let mut buf = vec![0u8; n];
    let mut filled = 0;
    while filled < n {
        match src.read(&mut buf[filled..])? {
            0 => break,
            k => filled += k,
        }
    }
    buf.truncate(filled);
    let rewound: Box<dyn ReaderInterface> = Box::new(RewindReader {
        head: io::Cursor::new(buf.clone()),
        tail: src,
    });
    Ok((buf, rewound))
}

/// Internal adapter: yields `head` first, then defers to `tail`.
struct RewindReader {
    head: io::Cursor<Vec<u8>>,
    tail: Box<dyn ReaderInterface>,
}

impl ReaderInterface for RewindReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.head.read(buf)?;
        if n > 0 {
            return Ok(n);
        }
        self.tail.read(buf)
    }
}
