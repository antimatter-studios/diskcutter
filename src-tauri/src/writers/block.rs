// /dev/diskN access — buffered block-device path. Slower than /dev/rdiskN on
// the hardware we've tested but kept as a swappable alternative for
// experimentation. Switch via `DISKCUTTER_WRITER=block` env var (see helper.rs).

#[cfg(unix)]
use std::fs::{File, OpenOptions};
#[cfg(unix)]
use std::io::{Read, Result, Write};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
#[cfg(unix)]
use std::path::{Path, PathBuf};

#[cfg(unix)]
use super::{DeviceIo, DeviceReader, DeviceWriter};

#[cfg(unix)]
pub struct BlockDeviceIo;

#[cfg(unix)]
impl DeviceIo for BlockDeviceIo {
    fn name(&self) -> &'static str {
        "block-device"
    }

    fn open_write(&self, device: &Path) -> Result<Box<dyn DeviceWriter>> {
        let target = to_block_path(device);
        let mut opts = OpenOptions::new();
        opts.read(true).write(true);
        #[cfg(target_os = "macos")]
        {
            opts.custom_flags(libc::O_EXLOCK);
        }
        let file = opts.open(&target)?;
        Ok(Box::new(BlockWriter { file }))
    }

    fn open_read(&self, device: &Path) -> Result<Box<dyn DeviceReader>> {
        let target = to_block_path(device);
        let file = File::open(&target)?;
        Ok(Box::new(BlockReader { file }))
    }
}

#[cfg(unix)]
fn to_block_path(device: &Path) -> PathBuf {
    // macOS: /dev/rdiskN -> /dev/diskN (block device).
    #[cfg(target_os = "macos")]
    {
        if let Some(name) = device.file_name().and_then(|s| s.to_str()) {
            if let Some(rest) = name.strip_prefix("rdisk") {
                return PathBuf::from(format!("/dev/disk{rest}"));
            }
        }
    }
    device.to_path_buf()
}

#[cfg(unix)]
pub struct BlockWriter {
    file: File,
}

#[cfg(unix)]
impl Write for BlockWriter {
    fn write(&mut self, buf: &[u8]) -> Result<usize> {
        self.file.write(buf)
    }
    fn flush(&mut self) -> Result<()> {
        self.file.flush()
    }
}

#[cfg(unix)]
impl DeviceWriter for BlockWriter {
    fn finish(mut self: Box<Self>) -> Result<()> {
        self.file.flush()?;
        self.file.sync_all()
    }
}

#[cfg(unix)]
pub struct BlockReader {
    file: File,
}

#[cfg(unix)]
impl Read for BlockReader {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        self.file.read(buf)
    }
}

#[cfg(unix)]
impl DeviceReader for BlockReader {}
