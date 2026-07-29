use std::io::{Read, Result, Seek, Write};
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
}

/// Device readers are `Seek` so a capture can resume from an offset rather than
/// re-reading the whole source. The concrete readers wrap a `File`, so seeking
/// is a direct delegate; the trait object stays seekable for `capture::capture`.
pub trait DeviceReader: Read + Seek + Send {}

pub trait DeviceIo: Send + Sync {
    #[allow(dead_code)]
    fn name(&self) -> &'static str;
    fn open_write(&self, device: &Path) -> Result<Box<dyn DeviceWriter>>;
    fn open_read(&self, device: &Path) -> Result<Box<dyn DeviceReader>>;
}
