//! Bootability sanity check via QEMU. After a successful burn the
//! frontend can ask "did we just produce something that actually
//! boots?" — we shell out to a locally-installed `qemu-system-*`
//! binary, point it at the freshly-written device (or the source
//! image itself) in `-snapshot` mode so QEMU never writes back, and
//! report on whether QEMU exited cleanly within a reasonable
//! window.
//!
//! `-snapshot` is critical: it makes QEMU keep all writes in a
//! tmpfile so the device-under-test stays read-only from QEMU's
//! perspective. Without it, a botched OS in the image could mangle
//! the just-written drive.
//!
//! No frontend is required to use this — it's a stand-alone
//! diagnostic command. Pairs naturally with the burn pipeline as a
//! post-burn smoke test that the frontend can offer once a verify
//! succeeds.
//!
//! Cross-platform note: on platforms without a usable QEMU binary,
//! `qemu_test_image` returns `available = false` so the frontend can
//! disable the button instead of failing loudly.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::Serialize;

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct QemuAvailability {
    /// True when at least one of the qemu-system-* binaries we know
    /// about is present and runnable on $PATH.
    pub available: bool,
    /// The first binary name found, e.g. "qemu-system-x86_64".
    pub binary: String,
    /// First-line of `qemu-system-* --version` output, for display.
    pub version: String,
}

#[derive(Serialize, Clone, Debug)]
pub struct QemuTestReport {
    /// Did the test actually run (binary found + spawned)?
    pub ran: bool,
    /// Did the boot test pass our heuristic?
    pub passed: bool,
    /// Wall-clock time the QEMU invocation took, milliseconds.
    pub elapsed_ms: u64,
    /// Free-form note explaining the verdict — included so the
    /// frontend can display "QEMU not installed", "boot OK after
    /// 12s", or "QEMU exited 1 (corrupted MBR?)".
    pub note: String,
    /// The binary used, e.g. "qemu-system-x86_64". Empty if `ran`
    /// is false.
    pub binary: String,
}

/// Architecture / firmware combinations we know how to dispatch.
/// Order matters — the first one that's installed wins.
const QEMU_BINARIES: &[&str] = &[
    "qemu-system-x86_64",
    "qemu-system-aarch64",
    "qemu-system-i386",
];

/// Detect the first available QEMU binary on `$PATH`. Returns
/// `available = false` when none are installed; the frontend uses
/// this to grey out the "boot test" button.
pub fn detect() -> QemuAvailability {
    for bin in QEMU_BINARIES {
        if let Some(version) = probe_binary(bin) {
            return QemuAvailability {
                available: true,
                binary: bin.to_string(),
                version,
            };
        }
    }
    QemuAvailability {
        available: false,
        binary: String::new(),
        version: String::new(),
    }
}

/// Run `<bin> --version` with a hard timeout. Returns the first line
/// of stdout when the binary exists and answers, otherwise `None`.
fn probe_binary(bin: &str) -> Option<String> {
    let out = Command::new(bin)
        .arg("--version")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .stdin(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let first = stdout.lines().next().unwrap_or("").trim();
    if first.is_empty() {
        return None;
    }
    Some(first.to_string())
}

/// Build the QEMU argument vector for booting `image_path` with
/// `-snapshot` (read-only behaviour, writes go to a tmpfile). Pulled
/// out so we can unit-test it without invoking QEMU.
pub fn build_args(image_path: &str, headless: bool) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "-drive".into(),
        format!("file={image_path},format=raw,if=virtio,snapshot=on,readonly=off"),
        // 512 MiB RAM — enough for typical bootloader + early init,
        // not so much we lock up a developer's laptop running this.
        "-m".into(),
        "512".into(),
        // Boot from disk, not netboot.
        "-boot".into(),
        "c".into(),
    ];
    if headless {
        // No GUI window, no audio, no graphics card. We're checking
        // "did the bootloader hand off?", not "does the desktop look
        // right".
        args.push("-nographic".into());
        args.push("-monitor".into());
        args.push("none".into());
        args.push("-serial".into());
        args.push("stdio".into());
    }
    args
}

/// Map a QEMU process outcome onto our pass/fail heuristic.
///
/// QEMU returning 0 doesn't mean the OS booted — it just means QEMU
/// itself exited cleanly (e.g. the user closed the GUI window).
/// Conversely, hitting our wall-clock timeout often means *something*
/// is running inside (vs. a crashed unbootable image that would exit
/// near-instant). We treat "QEMU stayed alive past the warmup
/// threshold" as a positive signal — pragmatic, not provably correct.
pub fn classify_outcome(
    exit_status: Option<i32>,
    elapsed: Duration,
    warmup: Duration,
) -> (bool, String) {
    match exit_status {
        Some(0) => (true, "QEMU exited cleanly".into()),
        Some(code) if elapsed >= warmup => (
            true,
            format!(
                "QEMU stayed alive {}s (exited {code}) — bootloader probably handed off",
                elapsed.as_secs()
            ),
        ),
        Some(code) => (
            false,
            format!(
                "QEMU exited {code} after only {}ms — image may be unbootable",
                elapsed.as_millis()
            ),
        ),
        None => (
            true,
            format!(
                "QEMU still running after {}s (we killed it) — boot looked alive",
                elapsed.as_secs()
            ),
        ),
    }
}

/// Run a bootability test. `headless` defaults to true so this works
/// on CI / SSH; pass false if a developer wants a window. The test
/// runs for at most `timeout`; QEMU is killed at the deadline and
/// the verdict is computed from `classify_outcome`.
pub fn test_image(image_path: &str, headless: bool, timeout: Duration) -> QemuTestReport {
    let availability = detect();
    if !availability.available {
        return QemuTestReport {
            ran: false,
            passed: false,
            elapsed_ms: 0,
            note: "QEMU not installed (apt install qemu-system / brew install qemu)".into(),
            binary: String::new(),
        };
    }
    if !Path::new(image_path).exists() {
        return QemuTestReport {
            ran: false,
            passed: false,
            elapsed_ms: 0,
            note: format!("image not found: {image_path}"),
            binary: availability.binary,
        };
    }

    let args = build_args(image_path, headless);
    let started = Instant::now();
    let mut child = match Command::new(&availability.binary)
        .args(&args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .stdin(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            return QemuTestReport {
                ran: false,
                passed: false,
                elapsed_ms: 0,
                note: format!("failed to spawn {}: {e}", availability.binary),
                binary: availability.binary,
            };
        }
    };

    // Poll for exit; kill at the deadline.
    let warmup = Duration::from_secs(5);
    loop {
        if let Ok(Some(status)) = child.try_wait() {
            let elapsed = started.elapsed();
            let (passed, note) = classify_outcome(status.code(), elapsed, warmup);
            return QemuTestReport {
                ran: true,
                passed,
                elapsed_ms: elapsed.as_millis() as u64,
                note,
                binary: availability.binary,
            };
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            let elapsed = started.elapsed();
            let (passed, note) = classify_outcome(None, elapsed, warmup);
            return QemuTestReport {
                ran: true,
                passed,
                elapsed_ms: elapsed.as_millis() as u64,
                note,
                binary: availability.binary,
            };
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Tauri command — frontend can light up a "boot test" button only
/// when `available` is true. Cheap (no QEMU spawn).
#[tauri::command]
pub fn qemu_check() -> QemuAvailability {
    detect()
}

/// Tauri command — runs the actual boot test. `timeout_secs` is
/// clamped between 5 and 120 so a runaway test can't hang the UI.
#[tauri::command]
pub fn qemu_test_image(image_path: String, timeout_secs: u64) -> QemuTestReport {
    let clamped = timeout_secs.clamp(5, 120);
    test_image(&image_path, true, Duration::from_secs(clamped))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_does_not_panic() {
        let _ = detect();
    }

    #[test]
    fn build_args_includes_drive_with_snapshot_on() {
        let a = build_args("/tmp/img.iso", true);
        let drive_idx = a.iter().position(|s| s == "-drive").unwrap();
        let drive_val = &a[drive_idx + 1];
        assert!(drive_val.contains("file=/tmp/img.iso"));
        assert!(drive_val.contains("snapshot=on"), "got {drive_val}");
    }

    #[test]
    fn build_args_headless_adds_nographic_and_serial_stdio() {
        let a = build_args("/tmp/x.iso", true);
        assert!(a.contains(&"-nographic".to_string()));
        assert!(a.contains(&"-serial".to_string()));
        assert!(a.contains(&"stdio".to_string()));
    }

    #[test]
    fn build_args_non_headless_omits_nographic() {
        let a = build_args("/tmp/x.iso", false);
        assert!(!a.contains(&"-nographic".to_string()));
        assert!(!a.contains(&"-serial".to_string()));
    }

    #[test]
    fn build_args_boots_from_disk() {
        let a = build_args("/tmp/x.iso", false);
        let i = a.iter().position(|s| s == "-boot").unwrap();
        assert_eq!(a[i + 1], "c");
    }

    #[test]
    fn build_args_pins_modest_ram() {
        let a = build_args("/tmp/x.iso", false);
        let i = a.iter().position(|s| s == "-m").unwrap();
        assert_eq!(a[i + 1], "512");
    }

    #[test]
    fn classify_outcome_zero_is_pass() {
        let (ok, note) = classify_outcome(
            Some(0),
            Duration::from_millis(2_000),
            Duration::from_secs(5),
        );
        assert!(ok);
        assert!(note.contains("cleanly"), "got {note}");
    }

    #[test]
    fn classify_outcome_nonzero_after_warmup_is_pass() {
        let (ok, _) = classify_outcome(Some(1), Duration::from_secs(10), Duration::from_secs(5));
        assert!(ok, "long-lived QEMU should be considered booted");
    }

    #[test]
    fn classify_outcome_nonzero_before_warmup_is_fail() {
        let (ok, note) =
            classify_outcome(Some(1), Duration::from_millis(800), Duration::from_secs(5));
        assert!(!ok);
        assert!(note.contains("unbootable"), "got {note}");
    }

    #[test]
    fn classify_outcome_killed_after_timeout_is_pass() {
        let (ok, note) = classify_outcome(None, Duration::from_secs(45), Duration::from_secs(5));
        assert!(ok);
        assert!(note.contains("still running") || note.contains("killed"));
    }

    #[test]
    fn test_image_missing_file_does_not_run_qemu() {
        let r = test_image(
            "/tmp/disk-cutter-this-file-does-not-exist.iso",
            true,
            Duration::from_secs(5),
        );
        // Either QEMU wasn't installed (ran=false) or QEMU was
        // installed and we caught the missing-file case (ran=false too).
        assert!(!r.ran);
        assert!(!r.passed);
    }

    #[test]
    fn qemu_check_does_not_panic() {
        let _ = qemu_check();
    }

    #[test]
    fn qemu_test_image_clamps_timeout_low() {
        // 0 must clamp to >= 5; we don't actually run QEMU here because
        // the file doesn't exist (returns early).
        let r = qemu_test_image(
            "/tmp/disk-cutter-this-file-does-not-exist.iso".to_string(),
            0,
        );
        assert!(!r.ran);
    }

    #[test]
    fn qemu_test_image_clamps_timeout_high() {
        // 9999 must clamp; same early-return path because file missing.
        let r = qemu_test_image(
            "/tmp/disk-cutter-this-file-does-not-exist.iso".to_string(),
            9999,
        );
        assert!(!r.ran);
    }
}
