use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;

use crate::hash::HashAlgo;
use crate::joblog::{JobLogger, LogLevel};
use crate::pipeline::{self, BurnError, VerifyMismatch};
use crate::source;
#[cfg(unix)]
use crate::writers::{BlockDeviceIo, PipelinedRawDeviceIo, RawDeviceIo};
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
    /// Per-job log entry. The parent's `tail_helper` parses these and
    /// forwards them into the `burn_logs` row for the matching job_id,
    /// so the per-item log captures decoder-chain / pipeline diagnostics
    /// alongside the lifecycle events the parent writes directly.
    Log { level: String, message: String },
}

/// Logger that emits `HelperMessage::Log` lines into the shared progress
/// JSONL the parent tails. `debug_enabled` is taken from the helper's
/// `--debug=` CLI arg (the parent reads the user's `debug.logging` pref
/// at spawn time and passes it through). When off, `debug()` is a no-op
/// — no JSONL line written, no string formatting cost incurred by
/// debug_enabled()-guarded call sites.
pub(crate) struct HelperLogger {
    writer: Arc<Mutex<BufWriter<File>>>,
    debug_enabled: bool,
}

impl HelperLogger {
    fn new(writer: Arc<Mutex<BufWriter<File>>>, debug_enabled: bool) -> Self {
        Self {
            writer,
            debug_enabled,
        }
    }
}

impl JobLogger for HelperLogger {
    fn log(&self, level: LogLevel, message: &str) {
        if level == LogLevel::Debug && !self.debug_enabled {
            return;
        }
        let msg = HelperMessage::Log {
            level: level.as_str().to_string(),
            message: message.to_string(),
        };
        if let Ok(mut w) = self.writer.lock() {
            if let Ok(s) = serde_json::to_string(&msg) {
                let _ = writeln!(w, "{}", s);
                let _ = w.flush();
            }
        }
    }

    fn debug_enabled(&self) -> bool {
        self.debug_enabled
    }
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
    // --job= drives the cancel sentinel path. Missing it disables cross-process
    // cancel (older parents won't pass it); the burn still runs.
    let job_id = arg_value(args, "--job=").unwrap_or_default();
    // Writer choice priority: --writer= CLI arg → DISKCUTTER_WRITER env → None (helper default).
    let writer_choice = arg_value(args, "--writer=")
        .or_else(|| std::env::var("DISKCUTTER_WRITER").ok())
        .filter(|s| !s.is_empty());

    // Runtime-tunable perf knobs. Each falls back to a built-in default when
    // absent or invalid so older callers keep working.
    let chunk_size: usize = arg_value(args, "--chunk-bytes=")
        .and_then(|v| v.parse::<u64>().ok())
        .map(|v| v as usize)
        .unwrap_or(pipeline::DEFAULT_CHUNK);
    let workers: usize = arg_value(args, "--workers=")
        .and_then(|v| v.parse().ok())
        .unwrap_or(4);
    let queue_depth: usize = arg_value(args, "--queue-depth=")
        .and_then(|v| v.parse().ok())
        .unwrap_or(16);
    let skip_verify: bool = arg_value(args, "--skip-verify=")
        .map(|v| v == "true")
        .unwrap_or(false);
    let debug_enabled: bool = arg_value(args, "--debug=")
        .map(|v| v == "true")
        .unwrap_or(false);
    // TODO: once disks.rs is no longer hot, propagate config `hash.algo` via
    // spawn_elevated_burn's helper command line. Until then the helper defaults
    // to SHA-256 regardless of UI pref.
    let hash_algo: HashAlgo = arg_value(args, "--hash-algo=")
        .as_deref()
        .map(HashAlgo::parse)
        .unwrap_or(HashAlgo::Sha256);

    // Open in append mode so the file accumulates a historical log of
    // every helper run for this job_id. The parent's tail_helper seeks
    // to the file's current end before reading, so old content doesn't
    // replay on the next burn — but it's still on disk if anyone wants
    // to inspect previous attempts via cat / less.
    let file = match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&progress_path)
    {
        Ok(f) => f,
        Err(_) => return 2,
    };
    let writer = Arc::new(Mutex::new(BufWriter::new(file)));
    let emit = |msg: &HelperMessage| {
        if let Ok(mut w) = writer.lock() {
            if let Ok(s) = serde_json::to_string(msg) {
                let _ = writeln!(w, "{}", s);
                let _ = w.flush();
            }
        }
    };
    let job_log = HelperLogger::new(Arc::clone(&writer), debug_enabled);
    if debug_enabled {
        job_log.debug(&format!(
            "helper: starting burn image={image} target={target} chunk={chunk_size} workers={workers} qdepth={queue_depth}"
        ));
    }

    let (mut reader, source_info) =
        match source::open_streaming_with_log(Path::new(&image), &job_log) {
            Ok(v) => v,
            Err(e) => {
                let code = if e.kind() == std::io::ErrorKind::InvalidInput {
                    "EUNSUPPORTED"
                } else {
                    "EIMAGE"
                };
                emit(&HelperMessage::Error {
                    error_code: code.into(),
                    error_message: format!("open image: {e}"),
                });
                return 1;
            }
        };
    let image_total_bytes = source_info.uncompressed_bytes;

    // Clamp the user-configured chunk_size to the device's reported
    // max IO size on macOS. Many USB / SD readers expose a per-IO cap
    // (typically 128 KiB or 256 KiB) on the rdisk char device; a
    // single pwrite larger than that EINVALs even when alignment is
    // perfect. Without this clamp, the default 1 MiB chunk fails
    // immediately on those devices.
    #[cfg(target_os = "macos")]
    let max_io_bytes = if target.starts_with("/dev/") {
        probe_device_max_io_write(&target)
    } else {
        None
    };
    #[cfg(not(target_os = "macos"))]
    let max_io_bytes: Option<u64> = None;
    let chunk_size = match max_io_bytes {
        Some(max) if (max as usize) < chunk_size => max as usize,
        _ => chunk_size,
    };

    let device_io: Box<dyn DeviceIo> =
        pick_device_io(&target, writer_choice.as_deref(), workers, queue_depth);
    // Snapshot the burn config into the log unconditionally (info-level)
    // so a failure report shows which writer + chunk + worker tuning was
    // active without needing debug.logging=true.
    job_log.info(&format!(
        "helper: writer={} target={} chunk={} workers={} qdepth={} image_bytes={} hash={}",
        device_io.name(),
        target,
        chunk_size,
        workers,
        queue_depth,
        image_total_bytes,
        match hash_algo {
            HashAlgo::Sha256 => "sha256",
            HashAlgo::Xxhash => "xxh64",
        },
    ));
    #[cfg(target_os = "macos")]
    if let Some(bs) = probe_device_block_size(&target) {
        job_log.info(&format!(
            "helper: target block size = {bs} (writes must be a multiple of this)"
        ));
    }
    if let Some(max) = max_io_bytes {
        job_log.info(&format!(
            "helper: target max IO write = {max} bytes (kernel rejects single writes larger than this)"
        ));
    }

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
            let code = if e.raw_os_error() == Some(1)
                || e.kind() == std::io::ErrorKind::PermissionDenied
            {
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

    let cancel = Arc::new(AtomicBool::new(false));
    // Watch the parent-controlled sentinel file. The parent (running as the
    // invoking user) writes to `disks::cancel_sentinel_path(job_id)`; the
    // helper runs as root via osascript, so signals are not an option —
    // file polling is the only reliable cross-process channel here. Reuse
    // the parent's path builder so both sides agree on the location
    // (`/tmp` on unix, `%TEMP%` on windows).
    if !job_id.is_empty() {
        let sentinel = crate::disks::cancel_sentinel_path(&job_id);
        let cancel_watch = Arc::clone(&cancel);
        std::thread::spawn(move || loop {
            if sentinel.exists() {
                cancel_watch.store(true, Ordering::Relaxed);
                return;
            }
            std::thread::sleep(Duration::from_millis(200));
        });
    }
    let burn = match pipeline::burn_with_hash(
        &mut *reader,
        image_total_bytes,
        dev_writer,
        chunk_size,
        hash_algo,
        &cancel,
        |p| {
            emit(&HelperMessage::Progress {
                state: "writing".into(),
                bytes_done: p.bytes_done,
                bytes_total: p.bytes_total,
                bytes_per_sec: p.bytes_per_sec,
            });
        },
    ) {
        Ok(b) => b,
        Err(e) => {
            let code = match e {
                BurnError::Cancelled => "ECANCELLED",
                _ => "EIO",
            };
            emit(&HelperMessage::Error {
                error_code: code.into(),
                error_message: format!("{e}"),
            });
            return 1;
        }
    };

    // If the caller asked us to skip verify entirely, finalise here using the
    // burn-side hash for both source and readback (verified is a no-op).
    if skip_verify {
        let elapsed_ms = burn.elapsed.as_millis() as u64;
        emit(&HelperMessage::Complete {
            bytes_written: burn.bytes_written,
            source_sha256: burn.source_sha256.clone(),
            readback_sha256: burn.source_sha256,
            verify_match: true,
            mismatches: vec![],
            elapsed_ms,
            avg_write_bps: burn.avg_bytes_per_sec,
            avg_verify_bps: 0,
        });
        return 0;
    }

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

    let fast = match pipeline::verify_hash_only_with_hash(
        &mut *dev_reader,
        burn.bytes_written,
        chunk_size,
        hash_algo,
        &cancel,
        |p| {
            emit(&HelperMessage::Progress {
                state: "verifying".into(),
                bytes_done: p.bytes_done,
                bytes_total: p.bytes_total,
                bytes_per_sec: p.bytes_per_sec,
            });
        },
    ) {
        Ok(v) => v,
        Err(e) => {
            let code = match e {
                BurnError::Cancelled => "ECANCELLED",
                _ => "EIO",
            };
            emit(&HelperMessage::Error {
                error_code: code.into(),
                error_message: format!("{e}"),
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
    job_log.info("helper: hash mismatch, reopening image for byte-compare diff");
    let (mut reader2, _) = match source::open_streaming_with_log(Path::new(&image), &job_log) {
        Ok(v) => v,
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

    let verify = match pipeline::verify_with_hash(
        &mut *reader2,
        image_total_bytes,
        &mut *dev_reader2,
        chunk_size,
        hash_algo,
        &cancel,
        |p| {
            emit(&HelperMessage::Progress {
                state: "verifying".into(),
                bytes_done: p.bytes_done,
                bytes_total: p.bytes_total,
                bytes_per_sec: p.bytes_per_sec,
            });
        },
    ) {
        Ok(v) => v,
        Err(e) => {
            let code = match e {
                BurnError::Cancelled => "ECANCELLED",
                _ => "EIO",
            };
            emit(&HelperMessage::Error {
                error_code: code.into(),
                error_message: format!("{e}"),
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

/// Pick the writer impl for the given target. Multiple impls live alongside
/// each other (see `writers/`) so we can experiment by swapping which one is
/// active here — no rewriting of working code. Choice is driven by the
/// caller (typically the helper subprocess); `writer_choice` should already
/// reflect the resolved priority order (CLI `--writer=` > env var > None).
///
///   "raw"        → /dev/rdiskN (char, plain write_all)
///   "block"      → /dev/diskN  (buffered block)
///   "pipelined"  → /dev/rdiskN (Etcher-style worker pool, default for /dev/)
///   "plain"      → PlainFileDeviceIo (only auto-selected when not /dev/)
#[cfg(target_os = "macos")]
fn translate_to_raw_path(target: &str) -> String {
    if let Some(name) = std::path::Path::new(target)
        .file_name()
        .and_then(|s| s.to_str())
    {
        if let Some(rest) = name.strip_prefix("disk") {
            if !rest.starts_with('r') {
                return format!("/dev/r{name}");
            }
        }
    }
    target.to_string()
}

#[cfg(target_os = "macos")]
fn ioctl_on_raw<T: Default>(target: &str, request: libc::c_ulong) -> Option<T> {
    use std::ffi::CString;
    let c = CString::new(translate_to_raw_path(target)).ok()?;
    unsafe {
        let fd = libc::open(c.as_ptr(), libc::O_RDONLY);
        if fd < 0 {
            return None;
        }
        let mut out = T::default();
        let r = libc::ioctl(fd, request, &mut out);
        libc::close(fd);
        if r == 0 {
            Some(out)
        } else {
            None
        }
    }
}

#[cfg(target_os = "macos")]
fn probe_device_block_size(target: &str) -> Option<u32> {
    // DKIOCGETBLOCKSIZE = _IOR('d', 24, uint32_t) → 0x40046418.
    // Used purely for diagnostics — surfaces in the per-row log so an
    // EINVAL on pwrite is interpretable ("write of N bytes vs device
    // block size 4096 → unaligned").
    const DKIOCGETBLOCKSIZE: libc::c_ulong = 0x40046418;
    ioctl_on_raw::<u32>(target, DKIOCGETBLOCKSIZE).filter(|v| *v > 0)
}

#[cfg(target_os = "macos")]
fn probe_device_max_io_write(target: &str) -> Option<u64> {
    // DKIOCGETMAXBYTECOUNTWRITE = _IOR('d', 81, uint64_t) → 0x40086451.
    // Many USB / SD readers cap rdisk per-IO transfer at 128 KiB or
    // 256 KiB; a single pwrite larger than that EINVALs even when the
    // size and offset are perfectly aligned to the block size. We
    // query this up-front and clamp chunk_size to fit so the burn
    // pipeline can't issue an oversized write.
    const DKIOCGETMAXBYTECOUNTWRITE: libc::c_ulong = 0x40086451;
    ioctl_on_raw::<u64>(target, DKIOCGETMAXBYTECOUNTWRITE).filter(|v| *v > 0)
}

fn pick_device_io(
    target: &str,
    writer_choice: Option<&str>,
    workers: usize,
    queue_depth: usize,
) -> Box<dyn DeviceIo> {
    if !target.starts_with("/dev/") {
        let _ = (workers, queue_depth);
        return Box::new(PlainFileDeviceIo);
    }
    #[cfg(unix)]
    {
        match writer_choice {
            Some("raw") => Box::new(RawDeviceIo),
            Some("block") => Box::new(BlockDeviceIo),
            Some("pipelined") | None => Box::new(PipelinedRawDeviceIo::new(workers, queue_depth)),
            Some(other) => {
                eprintln!("unknown writer choice {other:?}, falling back to pipelined");
                Box::new(PipelinedRawDeviceIo::new(workers, queue_depth))
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (writer_choice, workers, queue_depth);
        Box::new(PlainFileDeviceIo)
    }
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
    fn pick_device_io_for_dev_path_is_pipelined_on_unix() {
        // Default for /dev/ targets is the pipelined raw writer.
        let io = pick_device_io("/dev/disk5", None, 4, 15);
        #[cfg(unix)]
        assert_eq!(io.name(), "raw-pipelined");
        #[cfg(not(unix))]
        assert_eq!(io.name(), "plain-file");
    }

    #[cfg(unix)]
    #[test]
    fn pick_device_io_honours_explicit_writer_choice() {
        assert_eq!(
            pick_device_io("/dev/disk5", Some("raw"), 4, 15).name(),
            "raw-device"
        );
        assert_eq!(
            pick_device_io("/dev/disk5", Some("block"), 4, 15).name(),
            "block-device"
        );
        assert_eq!(
            pick_device_io("/dev/disk5", Some("pipelined"), 4, 15).name(),
            "raw-pipelined"
        );
        // Unknown values fall back to the pipelined default.
        assert_eq!(
            pick_device_io("/dev/disk5", Some("bogus"), 4, 15).name(),
            "raw-pipelined"
        );
    }

    #[test]
    fn pick_device_io_for_file_path_is_plain() {
        // File paths always use the plain writer regardless of writer choice.
        let io = pick_device_io("/tmp/foo.img", None, 4, 15);
        assert_eq!(io.name(), "plain-file");
        let io = pick_device_io("/tmp/foo.img", Some("pipelined"), 4, 15);
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

    #[test]
    fn arg_value_returns_first_match_when_key_repeats() {
        // arg_value uses iter().find_map — the first occurrence wins.
        let a = args(&["--image=/tmp/first.iso", "--image=/tmp/second.iso"]);
        assert_eq!(arg_value(&a, "--image="), Some("/tmp/first.iso".into()));
    }

    #[test]
    fn arg_value_preserves_equals_in_value() {
        // strip_prefix only strips the leading prefix; any remaining '=' is part of the value.
        let a = args(&["--foo=bar=baz"]);
        assert_eq!(arg_value(&a, "--foo="), Some("bar=baz".into()));
    }

    #[test]
    fn arg_value_preserves_multiple_equals_in_value() {
        let a = args(&["--query=a=b=c=d"]);
        assert_eq!(arg_value(&a, "--query="), Some("a=b=c=d".into()));
    }

    #[test]
    fn arg_value_does_not_match_partial_prefix() {
        // --imagex= shouldn't match a search for --image=
        let a = args(&["--imagex=/tmp/x"]);
        assert_eq!(arg_value(&a, "--image="), None);
    }

    #[test]
    fn arg_value_returns_none_for_empty_args() {
        let a: Vec<String> = Vec::new();
        assert_eq!(arg_value(&a, "--image="), None);
    }

    #[test]
    fn arg_value_skips_unrelated_args_to_find_match() {
        let a = args(&["--foo=1", "--bar=2", "--target=/dev/disk7"]);
        assert_eq!(arg_value(&a, "--target="), Some("/dev/disk7".into()));
    }

    #[cfg(unix)]
    #[test]
    fn pick_device_io_unknown_writer_choice_falls_back_to_pipelined() {
        // Already covered above for "bogus"; assert other shapes too to lock the contract.
        for unknown in &["", "x", "weird", "RAW", "Block", "pipelined-2"] {
            let io = pick_device_io("/dev/disk5", Some(unknown), 4, 15);
            assert_eq!(io.name(), "raw-pipelined", "input {unknown:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn pick_device_io_file_path_ignores_writer_choice() {
        for choice in &[
            None,
            Some("raw"),
            Some("block"),
            Some("pipelined"),
            Some("bogus"),
        ] {
            let io = pick_device_io("/tmp/foo.img", *choice, 4, 15);
            assert_eq!(io.name(), "plain-file", "choice {choice:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn pick_device_io_dev_path_with_none_picks_pipelined() {
        let io = pick_device_io("/dev/disk0", None, 4, 15);
        assert_eq!(io.name(), "raw-pipelined");
    }

    #[test]
    fn arg_value_recognises_hash_algo_xxhash() {
        // The helper parses `--hash-algo=xxhash` straight via `arg_value`,
        // then routes through `HashAlgo::parse`. Both halves must hold.
        let a = args(&["--image=/tmp/x.iso", "--hash-algo=xxhash"]);
        assert_eq!(arg_value(&a, "--hash-algo="), Some("xxhash".into()));
        assert_eq!(HashAlgo::parse("xxhash"), HashAlgo::Xxhash);
    }

    #[test]
    fn arg_value_hash_algo_defaults_to_sha256_when_missing() {
        // Absent flag → helper falls back to SHA-256.
        let a = args(&["--image=/tmp/x.iso", "--target=/dev/disk5"]);
        assert_eq!(arg_value(&a, "--hash-algo="), None);
        let algo = arg_value(&a, "--hash-algo=")
            .as_deref()
            .map(HashAlgo::parse)
            .unwrap_or(HashAlgo::Sha256);
        assert_eq!(algo, HashAlgo::Sha256);
    }

    #[test]
    fn arg_value_hash_algo_unknown_value_falls_back_to_sha256() {
        let a = args(&["--hash-algo=blake3"]);
        let algo = arg_value(&a, "--hash-algo=")
            .as_deref()
            .map(HashAlgo::parse)
            .unwrap_or(HashAlgo::Sha256);
        assert_eq!(algo, HashAlgo::Sha256);
    }

    #[test]
    fn arg_value_hash_algo_accepts_xxh64_alias() {
        let a = args(&["--hash-algo=xxh64"]);
        let algo = arg_value(&a, "--hash-algo=")
            .as_deref()
            .map(HashAlgo::parse)
            .unwrap_or(HashAlgo::Sha256);
        assert_eq!(algo, HashAlgo::Xxhash);
    }

    // ------------------------------------------------------------------
    // HelperMessage IPC contract.
    //
    // These tests lock down the *wire shape* of HelperMessage. The parent
    // app reads progress-file lines through `disks.rs::emit_helper_line`,
    // which dispatches on the `kind` discriminator and then pulls each
    // variant's fields by name. Any rename / field-shape change on this
    // side silently breaks that parser, so we assert the exact JSON keys
    // and tag values here. The parsing-side struct lives in disks.rs and
    // is itself covered by sibling tests; this side guards the producer.
    // ------------------------------------------------------------------

    fn to_json_value(m: &HelperMessage) -> serde_json::Value {
        serde_json::from_str(&serde_json::to_string(m).unwrap()).unwrap()
    }

    #[test]
    fn helper_message_progress_has_expected_fields() {
        let m = HelperMessage::Progress {
            state: "writing".into(),
            bytes_done: 42,
            bytes_total: 100,
            bytes_per_sec: 7,
        };
        let v = to_json_value(&m);
        assert_eq!(v["kind"], "progress");
        assert_eq!(v["state"], "writing");
        assert_eq!(v["bytes_done"], 42);
        assert_eq!(v["bytes_total"], 100);
        assert_eq!(v["bytes_per_sec"], 7);
        // No extra fields beyond the documented contract.
        let obj = v.as_object().unwrap();
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec![
                "bytes_done",
                "bytes_per_sec",
                "bytes_total",
                "kind",
                "state"
            ]
        );
    }

    #[test]
    fn helper_message_progress_verifying_state_serializes_verbatim() {
        // Two `state` values exist in production: "writing" and "verifying".
        // Both must pass through unmolested by the rename_all attribute (it
        // only applies to the enum tag, not field values).
        let m = HelperMessage::Progress {
            state: "verifying".into(),
            bytes_done: 0,
            bytes_total: 0,
            bytes_per_sec: 0,
        };
        let v = to_json_value(&m);
        assert_eq!(v["state"], "verifying");
    }

    #[test]
    fn helper_message_complete_has_expected_fields_when_match() {
        let m = HelperMessage::Complete {
            bytes_written: 1024,
            source_sha256: "deadbeef".into(),
            readback_sha256: "deadbeef".into(),
            verify_match: true,
            mismatches: vec![],
            elapsed_ms: 5000,
            avg_write_bps: 200_000,
            avg_verify_bps: 400_000,
        };
        let v = to_json_value(&m);
        assert_eq!(v["kind"], "complete");
        assert_eq!(v["bytes_written"], 1024);
        assert_eq!(v["source_sha256"], "deadbeef");
        assert_eq!(v["readback_sha256"], "deadbeef");
        assert_eq!(v["verify_match"], true);
        assert!(v["mismatches"].is_array());
        assert_eq!(v["mismatches"].as_array().unwrap().len(), 0);
        assert_eq!(v["elapsed_ms"], 5000);
        assert_eq!(v["avg_write_bps"], 200_000);
        assert_eq!(v["avg_verify_bps"], 400_000);

        let obj = v.as_object().unwrap();
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec![
                "avg_verify_bps",
                "avg_write_bps",
                "bytes_written",
                "elapsed_ms",
                "kind",
                "mismatches",
                "readback_sha256",
                "source_sha256",
                "verify_match",
            ]
        );
    }

    #[test]
    fn helper_message_complete_with_mismatches_serializes_their_shape() {
        // VerifyMismatch fields are the forensic detail the parent UI shows
        // when a verify fails — lba, byte_offset, expected, actual.
        let m = HelperMessage::Complete {
            bytes_written: 2048,
            source_sha256: "aa".into(),
            readback_sha256: "bb".into(),
            verify_match: false,
            mismatches: vec![VerifyMismatch {
                lba: "0x10".into(),
                byte_offset: "0x2000".into(),
                expected: "deadbeef".into(),
                actual: "cafef00d".into(),
            }],
            elapsed_ms: 100,
            avg_write_bps: 1,
            avg_verify_bps: 1,
        };
        let v = to_json_value(&m);
        assert_eq!(v["verify_match"], false);
        let mm = &v["mismatches"][0];
        assert_eq!(mm["lba"], "0x10");
        assert_eq!(mm["byte_offset"], "0x2000");
        assert_eq!(mm["expected"], "deadbeef");
        assert_eq!(mm["actual"], "cafef00d");
        let mm_obj = mm.as_object().unwrap();
        let mut keys: Vec<&str> = mm_obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(keys, vec!["actual", "byte_offset", "expected", "lba"]);
    }

    #[test]
    fn helper_message_error_has_expected_fields() {
        let m = HelperMessage::Error {
            error_code: "ENEEDS_FDA".into(),
            error_message: "Full Disk Access required".into(),
        };
        let v = to_json_value(&m);
        assert_eq!(v["kind"], "error");
        assert_eq!(v["error_code"], "ENEEDS_FDA");
        assert_eq!(v["error_message"], "Full Disk Access required");
        let obj = v.as_object().unwrap();
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(keys, vec!["error_code", "error_message", "kind"]);
    }

    #[test]
    fn helper_message_kind_tag_uses_lowercase_variant_names() {
        // The `#[serde(rename_all = "lowercase")]` attribute is the IPC
        // contract — disks.rs::emit_helper_line dispatches by exactly these
        // three string values.
        for (m, expected) in [
            (
                HelperMessage::Progress {
                    state: "writing".into(),
                    bytes_done: 0,
                    bytes_total: 0,
                    bytes_per_sec: 0,
                },
                "progress",
            ),
            (
                HelperMessage::Complete {
                    bytes_written: 0,
                    source_sha256: String::new(),
                    readback_sha256: String::new(),
                    verify_match: true,
                    mismatches: vec![],
                    elapsed_ms: 0,
                    avg_write_bps: 0,
                    avg_verify_bps: 0,
                },
                "complete",
            ),
            (
                HelperMessage::Error {
                    error_code: String::new(),
                    error_message: String::new(),
                },
                "error",
            ),
        ] {
            let v = to_json_value(&m);
            assert_eq!(v["kind"], expected, "kind tag mismatch for {expected}");
        }
    }

    #[test]
    fn helper_message_serializes_as_single_line_json() {
        // The receiver reads the progress file line-by-line; each emit
        // writes one JSON object terminated by a newline. The JSON itself
        // must not contain embedded newlines or it'll break the splitter.
        let m = HelperMessage::Progress {
            state: "writing".into(),
            bytes_done: 1,
            bytes_total: 2,
            bytes_per_sec: 3,
        };
        let s = serde_json::to_string(&m).unwrap();
        assert!(!s.contains('\n'), "serialized form must be one line: {s}");
    }
}
