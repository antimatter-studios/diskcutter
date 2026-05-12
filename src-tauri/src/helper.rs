use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::atomic::AtomicBool;

use serde::Serialize;

use crate::pipeline::{self, VerifyMismatch};
use crate::readers::ImageReaderRegistry;
#[cfg(unix)]
use crate::writers::RawDeviceIo;
use crate::writers::{DeviceIo, PlainFileDeviceIo};

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum HelperMessage {
    Progress {
        state: String,
        bytes_done: u64,
        bytes_total: u64,
        bytes_per_sec: u64,
    },
    Complete {
        bytes_written: u64,
        source_sha256: String,
        readback_sha256: String,
        verify_match: bool,
        mismatches: Vec<VerifyMismatch>,
        elapsed_ms: u64,
        avg_write_bps: u64,
        avg_verify_bps: u64,
    },
    Error {
        error_code: String,
        error_message: String,
    },
}

pub fn run_helper(args: &[String]) -> i32 {
    let image = match arg_value(args, "--image=") {
        Some(v) => v,
        None => return 2,
    };
    let target = match arg_value(args, "--target=") {
        Some(v) => v,
        None => return 2,
    };
    let progress_path = match arg_value(args, "--progress=") {
        Some(v) => v,
        None => return 2,
    };

    let file = match File::create(&progress_path) {
        Ok(f) => f,
        Err(_) => return 2,
    };
    let writer = std::sync::Mutex::new(BufWriter::new(file));
    let emit = |msg: &HelperMessage| {
        if let Ok(mut w) = writer.lock() {
            if let Ok(s) = serde_json::to_string(msg) {
                let _ = writeln!(w, "{}", s);
                let _ = w.flush();
            }
        }
    };

    let registry = ImageReaderRegistry::with_defaults();
    let factory = match registry.probe(Path::new(&image)) {
        Some((_, f)) => f,
        None => {
            emit(&HelperMessage::Error {
                error_code: "EUNSUPPORTED".into(),
                error_message: "unsupported image format".into(),
            });
            return 1;
        }
    };

    let mut reader = match factory.open(Path::new(&image)) {
        Ok(r) => r,
        Err(e) => {
            emit(&HelperMessage::Error {
                error_code: "EIMAGE".into(),
                error_message: format!("open image: {e}"),
            });
            return 1;
        }
    };

    let device_io: Box<dyn DeviceIo> = pick_device_io(&target);

    // Claim the disk through DiskArbitration. This (a) unmounts via DADiskUnmount
    // from our own session, atomic with (b) installing a mount-approval callback
    // that dissents any reattach. Both held in one session — no race window
    // between unmount and approval the way `diskutil unmountDisk` (separate
    // process, separate session) leaves open. Guard lives across write + verify.
    #[cfg(target_os = "macos")]
    let _disk_claim = if target.starts_with("/dev/") {
        match crate::disk_arb::DiskClaim::for_dev(&target) {
            Ok(c) => Some(c),
            Err(e) => {
                emit(&HelperMessage::Error {
                    error_code: "ETARGET".into(),
                    error_message: format!("DA claim failed: {e}"),
                });
                return 1;
            }
        }
    } else {
        None
    };

    let dev_writer = match device_io.open_write(Path::new(&target)) {
        Ok(w) => w,
        Err(e) => {
            let code = if e.raw_os_error() == Some(1) || e.kind() == std::io::ErrorKind::PermissionDenied {
                "ENEEDS_FDA"
            } else {
                "ETARGET"
            };
            emit(&HelperMessage::Error {
                error_code: code.into(),
                error_message: format!("open target: {e}"),
            });
            return 1;
        }
    };

    let cancel = AtomicBool::new(false);
    let burn = match pipeline::burn(&mut *reader, dev_writer, &cancel, |p| {
        emit(&HelperMessage::Progress {
            state: "writing".into(),
            bytes_done: p.bytes_done,
            bytes_total: p.bytes_total,
            bytes_per_sec: p.bytes_per_sec,
        });
    }) {
        Ok(b) => b,
        Err(e) => {
            emit(&HelperMessage::Error {
                error_code: "EIO".into(),
                error_message: format!("{e:?}"),
            });
            return 1;
        }
    };

    let mut dev_reader = match device_io.open_read(Path::new(&target)) {
        Ok(r) => r,
        Err(e) => {
            emit(&HelperMessage::Error {
                error_code: "ETARGET".into(),
                error_message: format!("reopen target: {e}"),
            });
            return 1;
        }
    };

    let fast = match pipeline::verify_hash_only(&mut *dev_reader, burn.bytes_written, &cancel, |p| {
        emit(&HelperMessage::Progress {
            state: "verifying".into(),
            bytes_done: p.bytes_done,
            bytes_total: p.bytes_total,
            bytes_per_sec: p.bytes_per_sec,
        });
    }) {
        Ok(v) => v,
        Err(e) => {
            emit(&HelperMessage::Error {
                error_code: "EIO".into(),
                error_message: format!("{e:?}"),
            });
            return 1;
        }
    };

    if fast.readback_sha256 == burn.source_sha256 {
        let elapsed_ms = (burn.elapsed.as_millis() + fast.elapsed.as_millis()) as u64;
        emit(&HelperMessage::Complete {
            bytes_written: burn.bytes_written,
            source_sha256: burn.source_sha256,
            readback_sha256: fast.readback_sha256,
            verify_match: true,
            mismatches: vec![],
            elapsed_ms,
            avg_write_bps: burn.avg_bytes_per_sec,
            avg_verify_bps: fast.avg_bytes_per_sec,
        });
        return 0;
    }

    // Hash mismatch — fall back to the slow byte-compare path to collect
    // per-sector forensic detail (LBA/offset/expected/actual).
    drop(dev_reader);
    let mut reader2 = match factory.open(Path::new(&image)) {
        Ok(r) => r,
        Err(e) => {
            emit(&HelperMessage::Error {
                error_code: "EIMAGE".into(),
                error_message: format!("reopen image: {e}"),
            });
            return 1;
        }
    };
    let mut dev_reader2 = match device_io.open_read(Path::new(&target)) {
        Ok(r) => r,
        Err(e) => {
            emit(&HelperMessage::Error {
                error_code: "ETARGET".into(),
                error_message: format!("reopen target: {e}"),
            });
            return 1;
        }
    };

    let verify = match pipeline::verify(&mut *reader2, &mut *dev_reader2, &cancel, |p| {
        emit(&HelperMessage::Progress {
            state: "verifying".into(),
            bytes_done: p.bytes_done,
            bytes_total: p.bytes_total,
            bytes_per_sec: p.bytes_per_sec,
        });
    }) {
        Ok(v) => v,
        Err(e) => {
            emit(&HelperMessage::Error {
                error_code: "EIO".into(),
                error_message: format!("{e:?}"),
            });
            return 1;
        }
    };

    let elapsed_ms =
        (burn.elapsed.as_millis() + fast.elapsed.as_millis() + verify.elapsed.as_millis()) as u64;
    emit(&HelperMessage::Complete {
        bytes_written: burn.bytes_written,
        source_sha256: burn.source_sha256,
        readback_sha256: verify.readback_sha256,
        verify_match: verify.match_,
        mismatches: verify.mismatches,
        elapsed_ms,
        avg_write_bps: burn.avg_bytes_per_sec,
        avg_verify_bps: verify.avg_bytes_per_sec,
    });

    0
}

fn pick_device_io(target: &str) -> Box<dyn DeviceIo> {
    if target.starts_with("/dev/") {
        #[cfg(unix)]
        {
            return Box::new(RawDeviceIo);
        }
    }
    Box::new(PlainFileDeviceIo)
}

fn arg_value(args: &[String], prefix: &str) -> Option<String> {
    args.iter()
        .find_map(|a| a.strip_prefix(prefix).map(|s| s.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn arg_value_returns_first_matching_prefix() {
        let a = args(&["--image=/tmp/a.iso", "--target=/dev/disk5"]);
        assert_eq!(arg_value(&a, "--image="), Some("/tmp/a.iso".into()));
        assert_eq!(arg_value(&a, "--target="), Some("/dev/disk5".into()));
    }

    #[test]
    fn arg_value_returns_none_when_missing() {
        let a = args(&["--image=/tmp/a.iso"]);
        assert_eq!(arg_value(&a, "--progress="), None);
    }

    #[test]
    fn arg_value_handles_empty_value() {
        let a = args(&["--image="]);
        assert_eq!(arg_value(&a, "--image="), Some("".into()));
    }

    #[test]
    fn pick_device_io_for_dev_path_is_raw_on_unix() {
        let io = pick_device_io("/dev/disk5");
        // helper always picks raw for /dev/ on unix; falls back to plain elsewhere.
        #[cfg(unix)]
        assert_eq!(io.name(), "raw-device");
        #[cfg(not(unix))]
        assert_eq!(io.name(), "plain-file");
    }

    #[test]
    fn pick_device_io_for_file_path_is_plain() {
        let io = pick_device_io("/tmp/foo.img");
        assert_eq!(io.name(), "plain-file");
    }

    #[test]
    fn helper_message_progress_serializes_with_kind_tag() {
        let m = HelperMessage::Progress {
            state: "writing".into(),
            bytes_done: 1,
            bytes_total: 10,
            bytes_per_sec: 5,
        };
        let s = serde_json::to_string(&m).unwrap();
        assert!(s.contains("\"kind\":\"progress\""), "got {s}");
        assert!(s.contains("\"state\":\"writing\""));
    }

    #[test]
    fn helper_message_error_serializes_with_kind_tag() {
        let m = HelperMessage::Error {
            error_code: "EIO".into(),
            error_message: "boom".into(),
        };
        let s = serde_json::to_string(&m).unwrap();
        assert!(s.contains("\"kind\":\"error\""));
        assert!(s.contains("\"error_code\":\"EIO\""));
    }
}
