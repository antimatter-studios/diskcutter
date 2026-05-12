use std::io::{Read, Result, Write};
use std::path::Path;

mod plain;
mod raw;
pub use plain::PlainFileDeviceIo;
#[cfg(unix)]
pub use raw::RawDeviceIo;

pub trait DeviceWriter: Write + Send {
    fn finish(self: Box<Self>) -> Result<()>;
}

pub trait DeviceReader: Read + Send {}

pub trait DeviceIo: Send + Sync {
    fn name(&self) -> &'static str;
    fn open_write(&self, device: &Path) -> Result<Box<dyn DeviceWriter>>;
    fn open_read(&self, device: &Path) -> Result<Box<dyn DeviceReader>>;
}
