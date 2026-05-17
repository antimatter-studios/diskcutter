//! `ReaderInterface` — the minimum surface every layer in the decoder
//! chain implements. A leaf (`RawFilehandle`) and every wrapping decoder
//! (xz, gz, bz2, zstd, …) both look like this from the outside.
//!
//! Deliberately a *streaming* read trait, not a positioned one. The
//! chain peels compression layers in order, byte-by-byte; only the
//! `DiskReader` facade's `layout()` path materialises a bounded prefix
//! into a `BlockRead` for partition probing.
//!
//! `Send` so a chained reader can be moved into a background burn
//! thread; `Sync` is intentionally NOT required — readers are
//! single-owner streaming state machines.
//!
//! Phase 2 agents implement this on their concrete decoder type. The
//! `Box<dyn ReaderInterface>` form is what the registry and
//! `identify_data_stream` traffic in, so a new format author never has
//! to know about the chain structure.

use std::io;

/// A single layer in the decoder chain. Streaming, single-direction,
/// no seek. The next layer above receives raw bytes from this one's
/// `read()`.
pub trait ReaderInterface: Send {
    /// Read up to `buf.len()` bytes into `buf`. Returns `Ok(0)` on EOF
    /// (mirrors `std::io::Read`). Short reads are legal; callers must
    /// loop if they need a full buffer.
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize>;
}

/// Forwarding impl so `Box<dyn ReaderInterface>` is itself a
/// `ReaderInterface`. Needed because the chain stacks boxed layers and
/// each wrapper holds its predecessor as a boxed trait object.
impl ReaderInterface for Box<dyn ReaderInterface> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        (**self).read(buf)
    }
}
