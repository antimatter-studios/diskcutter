// /dev/diskN access — buffered block-device path. Slower than /dev/rdiskN on
// the hardware we've tested but kept as a swappable alternative for
// experimentation. Switch via `DISKCUTTER_WRITER=block` env var (see helper.rs).

#[cfg(unix)]
use std::fs::{File, OpenOptions};
#[cfg(unix)]
use std::io::{Read, Result, Write};
#[cfg(target_os = "macos")]
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
        let file = opts.open(&target).map_err(|e| {
            std::io::Error::new(e.kind(), format!("open(2) {}: {}", target.display(), e))
        })?;
        Ok(Box::new(BlockWriter { file, offset: 0 }))
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
    offset: u64,
}

#[cfg(unix)]
impl Write for BlockWriter {
    fn write(&mut self, buf: &[u8]) -> Result<usize> {
        match self.file.write(buf) {
            Ok(n) => {
                self.offset += n as u64;
                Ok(n)
            }
            Err(e) => Err(wrap_write_err(e, self.offset, buf.len())),
        }
    }
    fn flush(&mut self) -> Result<()> {
        self.file.flush()
    }
}

#[cfg(unix)]
impl DeviceWriter for BlockWriter {
    fn finish(mut self: Box<Self>) -> Result<()> {
        self.file.flush()?;
        self.file.sync_all().map_err(|e| {
            std::io::Error::new(
                e.kind(),
                format!("sync_all after {} bytes written: {}", self.offset, e),
            )
        })
    }
}

#[cfg(unix)]
fn wrap_write_err(e: std::io::Error, offset: u64, len: usize) -> std::io::Error {
    let kind = e.kind();
    let raw = e.raw_os_error();
    let detail = match raw {
        Some(code) => format!("write at offset={offset} len={len} failed: {e} (errno {code})"),
        None => format!("write at offset={offset} len={len} failed: {e}"),
    };
    std::io::Error::new(kind, detail)
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

#[cfg(all(unix, test))]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn block_device_io_name() {
        assert_eq!(BlockDeviceIo.name(), "block-device");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn to_block_path_strips_r_prefix_for_rdisk() {
        assert_eq!(
            to_block_path(&PathBuf::from("/dev/rdisk5")),
            PathBuf::from("/dev/disk5")
        );
        assert_eq!(
            to_block_path(&PathBuf::from("/dev/rdisk0")),
            PathBuf::from("/dev/disk0")
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn to_block_path_preserves_already_block_device() {
        // /dev/diskN doesn't start with "rdisk" so it should pass through unchanged.
        assert_eq!(
            to_block_path(&PathBuf::from("/dev/disk5")),
            PathBuf::from("/dev/disk5")
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn to_block_path_handles_rdisk_with_partition_suffix() {
        assert_eq!(
            to_block_path(&PathBuf::from("/dev/rdisk5s1")),
            PathBuf::from("/dev/disk5s1")
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn to_block_path_handles_large_rdisk_number() {
        assert_eq!(
            to_block_path(&PathBuf::from("/dev/rdisk999")),
            PathBuf::from("/dev/disk999")
        );
    }

    #[test]
    fn to_block_path_passes_non_disk_paths_through() {
        let p = PathBuf::from("/tmp/some-file.img");
        assert_eq!(to_block_path(&p), p);
    }

    #[test]
    fn to_block_path_handles_empty_path() {
        let p = PathBuf::from("");
        assert_eq!(to_block_path(&p), p);
    }

    #[test]
    fn to_block_path_handles_dev_root() {
        let p = PathBuf::from("/dev/");
        assert_eq!(to_block_path(&p), p);
    }

    #[test]
    fn to_block_path_leaves_unrelated_dev_paths_untouched() {
        let p = PathBuf::from("/dev/null");
        assert_eq!(to_block_path(&p), p);
    }

    // Round-trip via tmpfile: BlockDeviceIo on a regular file path doesn't
    // translate the path (no "rdisk" prefix) and opens it directly. This
    // mirrors plain.rs's round-trip test but exercises the BlockWriter /
    // BlockReader I/O glue (write, flush, finish, sync_all, read_to_end).
    #[test]
    fn round_trip_write_then_read_on_regular_file() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("blk.img");
        // Pre-create so the OpenOptions read+write doesn't fail (no `create`).
        std::fs::write(&p, b"").unwrap();
        let io = BlockDeviceIo;

        let mut writer = io.open_write(&p).unwrap();
        writer.write_all(b"block-round-trip").unwrap();
        writer.finish().unwrap();

        let mut reader = io.open_read(&p).unwrap();
        let mut out = Vec::new();
        Read::read_to_end(&mut reader, &mut out).unwrap();
        assert_eq!(out, b"block-round-trip");
    }

    #[test]
    fn open_read_errors_on_missing_file() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("ghost.img");
        let io = BlockDeviceIo;
        assert!(io.open_read(&p).is_err());
    }
}
