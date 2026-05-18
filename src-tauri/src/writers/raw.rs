#[cfg(unix)]
use std::fs::{File, OpenOptions};
#[cfg(unix)]
use std::io::{Read, Result, Write};
#[cfg(target_os = "macos")]
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
#[cfg(unix)]
use std::path::{Path, PathBuf};

#[cfg(unix)]
use super::{DeviceIo, DeviceReader, DeviceWriter};

#[cfg(unix)]
pub struct RawDeviceIo;

#[cfg(unix)]
impl DeviceIo for RawDeviceIo {
    fn name(&self) -> &'static str {
        "raw-device"
    }

    fn open_write(&self, device: &Path) -> Result<Box<dyn DeviceWriter>> {
        // Caller (helper.rs) holds a DiskClaim on macOS — DA has already
        // unmounted and is dissenting any remount. Just open.
        let target = translate_to_raw(device);
        let mut opts = OpenOptions::new();
        opts.read(true).write(true);
        #[cfg(target_os = "macos")]
        {
            opts.custom_flags(libc::O_EXLOCK);
        }
        #[cfg(target_os = "linux")]
        {
            opts.custom_flags(libc::O_SYNC | libc::O_DIRECT);
        }
        match opts.open(&target) {
            Ok(f) => {
                #[cfg(target_os = "macos")]
                unsafe {
                    // Skip the unified buffer cache — writes go straight to
                    // device. Matches what Etcher does explicitly.
                    libc::fcntl(f.as_raw_fd(), libc::F_NOCACHE, 1);
                }
                Ok(Box::new(RawWriter { file: f, offset: 0 }))
            }
            Err(e) => {
                let detail = describe_busy(device);
                Err(std::io::Error::new(
                    e.kind(),
                    format!("opening {}: {}. {}", target.display(), e, detail),
                ))
            }
        }
    }

    fn open_read(&self, device: &Path) -> Result<Box<dyn DeviceReader>> {
        let target = translate_to_raw(device);
        let file = File::open(&target)?;
        #[cfg(target_os = "macos")]
        unsafe {
            libc::fcntl(file.as_raw_fd(), libc::F_NOCACHE, 1);
        }
        Ok(Box::new(RawReader { file }))
    }
}

#[cfg(unix)]
fn translate_to_raw(device: &Path) -> PathBuf {
    // macOS: /dev/diskN -> /dev/rdiskN (unbuffered char device). Empirically
    // faster than the buffered block path for bulk burns on this hardware.
    #[cfg(target_os = "macos")]
    {
        if let Some(name) = device.file_name().and_then(|s| s.to_str()) {
            if let Some(rest) = name.strip_prefix("disk") {
                if !rest.starts_with('r') {
                    return PathBuf::from(format!("/dev/r{name}"));
                }
            }
        }
    }
    device.to_path_buf()
}

#[cfg(target_os = "macos")]
#[allow(dead_code)]
fn unmount_macos(device: &Path) -> std::result::Result<(), String> {
    let name = device.file_name().and_then(|s| s.to_str()).unwrap_or("");
    if !name.starts_with("disk") {
        return Ok(());
    }
    let out = std::process::Command::new("diskutil")
        .args(["unmountDisk", "force", &device.to_string_lossy()])
        .output()
        .map_err(|e| format!("diskutil spawn: {e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
        return Err(if !stderr.is_empty() { stderr } else { stdout });
    }
    Ok(())
}

#[cfg(all(unix, not(target_os = "macos")))]
#[allow(dead_code)]
fn unmount_macos(_device: &Path) -> std::result::Result<(), String> {
    Ok(())
}

#[cfg(target_os = "macos")]
fn describe_busy(device: &Path) -> String {
    let raw = translate_to_raw(device);
    let dev_str = device.to_string_lossy().to_string();
    let raw_str = raw.to_string_lossy().to_string();

    // lsof lives in /usr/sbin which isn't in osascript-admin PATH.
    let lsof = std::process::Command::new("/usr/sbin/lsof")
        .args([&dev_str, &raw_str])
        .output();
    let holders = match lsof {
        Ok(o) => {
            let s = String::from_utf8_lossy(&o.stdout).to_string();
            // First line is the header; useful detail is the rest.
            let lines: Vec<&str> = s.lines().skip(1).collect();
            if lines.is_empty() {
                String::new()
            } else {
                lines.join(" | ")
            }
        }
        Err(_) => String::new(),
    };
    if holders.is_empty() {
        "Disk is held but no process visible to lsof — likely the kernel itself (diskarbitrationd or fseventsd auto-attaching). Try ejecting via Finder and reinserting, then retry immediately.".to_string()
    } else {
        format!("Disk is held by: {holders}. Quit those processes and retry.")
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
fn describe_busy(_device: &Path) -> String {
    "Disk is held by another process.".to_string()
}

#[cfg(unix)]
pub struct RawWriter {
    file: File,
    offset: u64,
}

#[cfg(unix)]
impl Write for RawWriter {
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
impl DeviceWriter for RawWriter {
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
    // EINVAL on macOS rdisk usually means the write exceeded the
    // device's reported max IO size (DKIOCGETMAXBYTECOUNTWRITE) or
    // wasn't aligned to the block size. Surfacing offset + len makes
    // either reading instantly diagnoseable from the row log.
    let kind = e.kind();
    let raw = e.raw_os_error();
    let detail = match raw {
        Some(code) => format!("write at offset={offset} len={len} failed: {e} (errno {code})"),
        None => format!("write at offset={offset} len={len} failed: {e}"),
    };
    std::io::Error::new(kind, detail)
}

#[cfg(unix)]
pub struct RawReader {
    file: File,
}

#[cfg(unix)]
impl Read for RawReader {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        self.file.read(buf)
    }
}

#[cfg(unix)]
impl DeviceReader for RawReader {}

#[cfg(all(unix, test))]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[cfg(target_os = "macos")]
    #[test]
    fn translate_to_raw_inserts_r_prefix() {
        assert_eq!(
            translate_to_raw(&PathBuf::from("/dev/disk5")),
            PathBuf::from("/dev/rdisk5")
        );
        assert_eq!(
            translate_to_raw(&PathBuf::from("/dev/disk0")),
            PathBuf::from("/dev/rdisk0")
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn translate_to_raw_preserves_already_raw_device() {
        assert_eq!(
            translate_to_raw(&PathBuf::from("/dev/rdisk5")),
            PathBuf::from("/dev/rdisk5")
        );
    }

    #[test]
    fn translate_to_raw_passes_non_disk_paths_through() {
        let p = PathBuf::from("/tmp/some-file.img");
        assert_eq!(translate_to_raw(&p), p);
    }

    #[test]
    fn raw_device_io_name() {
        assert_eq!(RawDeviceIo.name(), "raw-device");
    }

    #[test]
    fn translate_to_raw_handles_empty_path() {
        let p = PathBuf::from("");
        // Empty path has no file_name, so it should pass through unchanged.
        assert_eq!(translate_to_raw(&p), p);
    }

    #[test]
    fn translate_to_raw_handles_dev_root_with_no_disk() {
        let p = PathBuf::from("/dev/");
        // "/dev/" has no file_name component to translate.
        assert_eq!(translate_to_raw(&p), p);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn translate_to_raw_handles_disk_without_number() {
        // "disk" with no number still matches the strip_prefix branch
        // (rest = "") and should be prefixed with r.
        assert_eq!(
            translate_to_raw(&PathBuf::from("/dev/disk")),
            PathBuf::from("/dev/rdisk")
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn translate_to_raw_handles_large_disk_number() {
        assert_eq!(
            translate_to_raw(&PathBuf::from("/dev/disk999")),
            PathBuf::from("/dev/rdisk999")
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn translate_to_raw_handles_disk_with_partition_suffix() {
        // diskNsM-style names also get the r prefix.
        assert_eq!(
            translate_to_raw(&PathBuf::from("/dev/disk5s1")),
            PathBuf::from("/dev/rdisk5s1")
        );
    }

    #[test]
    fn translate_to_raw_leaves_unrelated_dev_paths_untouched() {
        let p = PathBuf::from("/dev/null");
        assert_eq!(translate_to_raw(&p), p);
        let p2 = PathBuf::from("/dev/zero");
        assert_eq!(translate_to_raw(&p2), p2);
    }
}
