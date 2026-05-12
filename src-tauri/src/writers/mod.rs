use std::io::{Read, Result, Write};
use std::path::Path;

mod block;
#[cfg(unix)]
mod pipelined;
mod plain;
mod raw;
#[cfg(unix)]
pub use block::BlockDeviceIo;
#[cfg(unix)]
#[allow(unused_imports)]
pub use pipelined::PipelinedRawDeviceIo;
pub use plain::PlainFileDeviceIo;
#[cfg(unix)]
pub use raw::RawDeviceIo;

pub trait DeviceWriter: Write + Send {
    fn finish(self: Box<Self>) -> Result<()>;
}

pub trait DeviceReader: Read + Send {}

pub trait DeviceIo: Send + Sync {
    #[allow(dead_code)]
    fn name(&self) -> &'static str;
    fn open_write(&self, device: &Path) -> Result<Box<dyn DeviceWriter>>;
    fn open_read(&self, device: &Path) -> Result<Box<dyn DeviceReader>>;
}
