use std::io::{Read, Result, Write};
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

/// Force a burn target to commit its volatile write cache to stable media.
///
/// On macOS a plain `fsync`/`sync_all` only guarantees the OS has handed the
/// data to the device — it does NOT tell an SD reader / USB bridge to flush its
/// internal DRAM cache to NAND, and (because our fds are opened `F_NOCACHE`) it
/// barely does anything at the OS layer either. The only call that forces the
/// physical commit is `F_FULLFSYNC` (filesystem files) or the
/// `DKIOCSYNCHRONIZECACHE` ioctl (raw disk nodes). Burn targets are raw
/// `/dev/rdiskN` char devices with no filesystem, so `F_FULLFSYNC` is a no-op
/// there and we must issue the ioctl. Without this, the readback verify can
/// pass by reading bytes still sitting in the reader's cache while the later
/// blocks were never programmed — the card boots p1 but can't open the root fs.
#[cfg(unix)]
pub(crate) fn sync_device_cache(fd: std::os::unix::io::RawFd, bytes_written: u64) -> Result<()> {
    // Push the OS side first. Cheap, and correct on the buffered block path.
    let ret = unsafe { libc::fsync(fd) };
    if ret != 0 {
        let e = std::io::Error::last_os_error();
        return Err(std::io::Error::new(
            e.kind(),
            format!("fsync after {bytes_written} bytes written: {e}"),
        ));
    }
    #[cfg(target_os = "macos")]
    {
        // DKIOCSYNCHRONIZECACHE = _IO('d', 22) → 0x20006416. Commands the disk
        // controller to flush its write cache to media. Regular files (used in
        // tests) return ENOTTY — that's expected, not an error.
        const DKIOCSYNCHRONIZECACHE: libc::c_ulong = 0x20006416;
        let ret = unsafe { libc::ioctl(fd, DKIOCSYNCHRONIZECACHE) };
        if ret != 0 {
            let e = std::io::Error::last_os_error();
            if e.raw_os_error() != Some(libc::ENOTTY) {
                return Err(std::io::Error::new(
                    e.kind(),
                    format!("device cache sync after {bytes_written} bytes written: {e}"),
                ));
            }
        }
    }
    Ok(())
}

pub trait DeviceReader: Read + Send {}

pub trait DeviceIo: Send + Sync {
    #[allow(dead_code)]
    fn name(&self) -> &'static str;
    fn open_write(&self, device: &Path) -> Result<Box<dyn DeviceWriter>>;
    fn open_read(&self, device: &Path) -> Result<Box<dyn DeviceReader>>;
}
