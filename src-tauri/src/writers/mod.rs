use std::io::{Error, ErrorKind, Read, Result, Write};
use std::path::Path;

mod block;
#[cfg(unix)]
mod parallel;
#[cfg(unix)]
mod pipelined;
mod plain;
mod raw;
#[cfg(unix)]
pub use block::BlockDeviceIo;
#[cfg(unix)]
#[allow(unused_imports)]
pub use parallel::ParallelRawDeviceIo;
#[cfg(unix)]
#[allow(unused_imports)]
pub use pipelined::PipelinedRawDeviceIo;
pub use plain::PlainFileDeviceIo;
#[cfg(unix)]
pub use raw::RawDeviceIo;

pub trait DeviceWriter: Write + Send {
    fn finish(self: Box<Self>) -> Result<()>;

    /// Overwrite `buf.len()` bytes at absolute `offset` without disturbing the
    /// sequential write cursor. Used by the repair path to rewrite only the
    /// chunks that failed read-back. The default is `Unsupported`: only the
    /// simple single-fd writers (raw device, plain file) implement it — the
    /// pipelined/parallel writers are sequential-only by construction.
    fn write_at(&mut self, _buf: &[u8], _offset: u64) -> Result<()> {
        Err(Error::new(
            ErrorKind::Unsupported,
            "positioned write not supported by this writer",
        ))
    }
}

pub trait DeviceReader: Read + Send {}

pub trait DeviceIo: Send + Sync {
    #[allow(dead_code)]
    fn name(&self) -> &'static str;
    fn open_write(&self, device: &Path) -> Result<Box<dyn DeviceWriter>>;
    fn open_read(&self, device: &Path) -> Result<Box<dyn DeviceReader>>;

    /// Open the device for in-place positioned writes (the repair path).
    /// Unlike [`open_write`] this must NOT truncate or zero existing
    /// contents — repair overwrites individual chunks of an already-burned
    /// device. The default is `Unsupported`; raw-device and plain-file IO
    /// override it. Callers select a positioned-capable IO by target type
    /// rather than reusing the (sequential, pipelined) burn IO.
    fn open_write_at(&self, _device: &Path) -> Result<Box<dyn DeviceWriter>> {
        Err(Error::new(
            ErrorKind::Unsupported,
            "positioned open not supported by this device IO",
        ))
    }
}
