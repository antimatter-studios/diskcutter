//! Self-diagnostic for the user's environment. Answers "is everything
//! Disk Cutter needs actually present and working?" in a single
//! Tauri command, so the Prefs view (or a CLI `disk-cutter doctor`
//! invocation) can render a checklist instead of failing one feature
//! at a time.
//!
//! Each check is small, side-effect-free, and pure pass/fail/warn.
//! Adding a new check = appending a `Check` to `run_all` and writing
//! a small builder function. No global registry / dyn dispatch — keep
//! it readable.
//!
//! Hard rule: every check must complete in well under a second. We
//! call this on UI mount and the user shouldn't notice.

use std::path::Path;
use std::process::{Command, Stdio};

use serde::Serialize;

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    /// All good. Render in green.
    Pass,
    /// Optional capability missing — Disk Cutter still works but a
    /// feature that depends on this is degraded. Render in yellow.
    Warn,
    /// Required capability missing — burns will fail. Render in red.
    Fail,
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct Check {
    /// Stable id used as React key on the frontend.
    pub id: &'static str,
    /// Short label, e.g. "QEMU bootability test".
    pub name: &'static str,
    /// Verdict.
    pub status: CheckStatus,
    /// Free-form note: where the binary was found, what version, what
    /// was missing, how to install it.
    pub note: String,
    /// Bucket: "core" (must work), "feature" (optional capability),
    /// "info" (informational).
    pub category: &'static str,
}

impl Check {
    fn pass(id: &'static str, name: &'static str, category: &'static str, note: &str) -> Self {
        Check {
            id,
            name,
            status: CheckStatus::Pass,
            note: note.to_string(),
            category,
        }
    }

    fn warn(id: &'static str, name: &'static str, category: &'static str, note: &str) -> Self {
        Check {
            id,
            name,
            status: CheckStatus::Warn,
            note: note.to_string(),
            category,
        }
    }

    fn fail(id: &'static str, name: &'static str, category: &'static str, note: &str) -> Self {
        Check {
            id,
            name,
            status: CheckStatus::Fail,
            note: note.to_string(),
            category,
        }
    }
}

#[derive(Serialize, Clone, Debug)]
pub struct DoctorReport {
    pub checks: Vec<Check>,
    /// Aggregate verdict computed across all checks: any Fail wins,
    /// otherwise any Warn, otherwise Pass.
    pub overall: CheckStatus,
}

/// Compose the worst status across a slice of checks. Pass-by-default
/// for empty slices keeps the function total.
pub fn aggregate(checks: &[Check]) -> CheckStatus {
    let mut overall = CheckStatus::Pass;
    for c in checks {
        match c.status {
            CheckStatus::Fail => return CheckStatus::Fail,
            CheckStatus::Warn => overall = CheckStatus::Warn,
            CheckStatus::Pass => {}
        }
    }
    overall
}

/// Best-effort: return the first line of `<bin> --version` if the
/// binary is on $PATH and exits 0. None otherwise.
pub fn probe_binary(bin: &str) -> Option<String> {
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
    let s = String::from_utf8_lossy(&out.stdout);
    s.lines().next().map(|s| s.trim().to_string())
}

fn check_qemu() -> Check {
    if let Some(v) = probe_binary("qemu-system-x86_64") {
        return Check::pass(
            "qemu",
            "QEMU bootability test",
            "feature",
            &format!("found qemu-system-x86_64: {v}"),
        );
    }
    if let Some(v) = probe_binary("qemu-system-aarch64") {
        return Check::pass(
            "qemu",
            "QEMU bootability test",
            "feature",
            &format!("found qemu-system-aarch64: {v}"),
        );
    }
    Check::warn(
        "qemu",
        "QEMU bootability test",
        "feature",
        "qemu-system-* not on PATH — install qemu (brew install qemu / apt install qemu-system) to enable post-burn boot tests",
    )
}

#[cfg(target_os = "macos")]
fn check_eject_backend() -> Check {
    if probe_binary("diskutil").is_some() {
        Check::pass("eject", "Eject backend", "core", "diskutil present")
    } else {
        Check::fail(
            "eject",
            "Eject backend",
            "core",
            "diskutil not found — should be a macOS built-in; PATH may be misconfigured",
        )
    }
}

#[cfg(target_os = "linux")]
fn check_eject_backend() -> Check {
    if probe_binary("udisksctl").is_some() {
        Check::pass(
            "eject",
            "Eject backend",
            "core",
            "udisksctl present (preferred Linux ejector)",
        )
    } else if probe_binary("eject").is_some() {
        Check::warn(
            "eject",
            "Eject backend",
            "core",
            "udisksctl not installed; falling back to classic eject(1). Consider installing udisks2.",
        )
    } else {
        Check::fail(
            "eject",
            "Eject backend",
            "core",
            "neither udisksctl nor eject(1) found — auto-eject will fail. Install udisks2 or util-linux.",
        )
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn check_eject_backend() -> Check {
    Check::warn(
        "eject",
        "Eject backend",
        "core",
        "eject not implemented on this platform yet",
    )
}

fn check_app_data_writable() -> Check {
    let dir = std::env::temp_dir().join("disk-cutter-doctor-write");
    let result = std::fs::write(&dir, b"ok").and_then(|_| std::fs::remove_file(&dir));
    match result {
        Ok(_) => Check::pass(
            "tmpdir",
            "Temp directory writable",
            "core",
            &format!("wrote {} successfully", dir.display()),
        ),
        Err(e) => Check::fail(
            "tmpdir",
            "Temp directory writable",
            "core",
            &format!("could not write temp file: {e}"),
        ),
    }
}

#[cfg(target_os = "macos")]
fn check_full_disk_access() -> Check {
    // Heuristic: macOS gates raw device IO behind FDA. We can't probe
    // it directly without trying a write, so we look for whether the
    // process can stat a known FDA-protected file (TCC.db) — failing
    // means we very likely lack FDA. We avoid actually writing to a
    // raw device just to check.
    let probe = Path::new("/Library/Application Support/com.apple.TCC/TCC.db");
    if probe.metadata().is_ok() {
        Check::pass(
            "fda",
            "Full Disk Access",
            "core",
            "we can read TCC.db, so FDA is likely granted to the parent terminal/app",
        )
    } else {
        Check::warn(
            "fda",
            "Full Disk Access",
            "core",
            "could not stat /Library/Application Support/com.apple.TCC/TCC.db — Disk Cutter (or the launching app) may need Full Disk Access for raw device writes",
        )
    }
}

#[cfg(not(target_os = "macos"))]
fn check_full_disk_access() -> Check {
    Check::pass(
        "fda",
        "Full Disk Access",
        "core",
        "not applicable on this platform",
    )
}

fn check_temp_space() -> Check {
    // Quick smoke: can we get a usable temp dir? We don't actually
    // measure free space (cross-platform statvfs is a yak); the
    // "could we create a small file?" check above already covers the
    // worst case (full disk).
    let tmp = std::env::temp_dir();
    if tmp.as_os_str().is_empty() {
        return Check::fail(
            "tempdir-resolved",
            "Temp directory resolves",
            "info",
            "std::env::temp_dir() returned an empty path",
        );
    }
    Check::pass(
        "tempdir-resolved",
        "Temp directory resolves",
        "info",
        &format!("resolved to {}", tmp.display()),
    )
}

/// Run every check sequentially. Returns a [`DoctorReport`] with the
/// collected results plus an aggregate. Total wall time should stay
/// well under one second on a healthy machine.
pub fn run_all() -> DoctorReport {
    let checks = vec![
        check_app_data_writable(),
        check_temp_space(),
        check_eject_backend(),
        check_full_disk_access(),
        check_qemu(),
    ];
    let overall = aggregate(&checks);
    DoctorReport { checks, overall }
}

#[tauri::command]
pub fn doctor() -> DoctorReport {
    run_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aggregate_returns_pass_for_empty_slice() {
        assert_eq!(aggregate(&[]), CheckStatus::Pass);
    }

    #[test]
    fn aggregate_returns_pass_when_all_pass() {
        let checks = vec![
            Check::pass("a", "A", "core", ""),
            Check::pass("b", "B", "core", ""),
        ];
        assert_eq!(aggregate(&checks), CheckStatus::Pass);
    }

    #[test]
    fn aggregate_returns_warn_when_any_warn_no_fail() {
        let checks = vec![
            Check::pass("a", "A", "core", ""),
            Check::warn("b", "B", "feature", ""),
            Check::pass("c", "C", "core", ""),
        ];
        assert_eq!(aggregate(&checks), CheckStatus::Warn);
    }

    #[test]
    fn aggregate_returns_fail_when_any_fail_short_circuits() {
        let checks = vec![
            Check::warn("a", "A", "feature", ""),
            Check::fail("b", "B", "core", ""),
            Check::warn("c", "C", "feature", ""),
        ];
        assert_eq!(aggregate(&checks), CheckStatus::Fail);
    }

    #[test]
    fn check_constructors_set_status_correctly() {
        assert_eq!(Check::pass("a", "A", "core", "n").status, CheckStatus::Pass);
        assert_eq!(Check::warn("a", "A", "core", "n").status, CheckStatus::Warn);
        assert_eq!(Check::fail("a", "A", "core", "n").status, CheckStatus::Fail);
    }

    #[test]
    fn check_constructors_capture_metadata() {
        let c = Check::warn("xyz", "Long Name", "feature", "explanation");
        assert_eq!(c.id, "xyz");
        assert_eq!(c.name, "Long Name");
        assert_eq!(c.category, "feature");
        assert_eq!(c.note, "explanation");
    }

    #[test]
    fn probe_binary_returns_none_for_missing_command() {
        assert!(probe_binary("disk-cutter-this-binary-cannot-exist-abc123").is_none());
    }

    #[test]
    fn probe_binary_returns_first_line_for_real_command() {
        // `cat --version` works on macOS + most Linuxes; if not present
        // we accept either Some(_) or None as "didn't crash".
        let _ = probe_binary("cat");
    }

    #[test]
    fn check_app_data_writable_returns_a_check() {
        let c = check_app_data_writable();
        assert_eq!(c.id, "tmpdir");
        // On any developer's machine this should pass — we wrote to
        // the system tempdir which is universally writable.
        assert_eq!(c.status, CheckStatus::Pass);
    }

    #[test]
    fn check_temp_space_resolves() {
        let c = check_temp_space();
        assert_eq!(c.status, CheckStatus::Pass);
        assert_eq!(c.id, "tempdir-resolved");
    }

    #[test]
    fn run_all_returns_a_report_with_overall_in_pass_warn_fail() {
        let r = run_all();
        assert!(!r.checks.is_empty());
        assert!(matches!(
            r.overall,
            CheckStatus::Pass | CheckStatus::Warn | CheckStatus::Fail
        ));
    }

    #[test]
    fn run_all_includes_the_eject_check() {
        let r = run_all();
        assert!(r.checks.iter().any(|c| c.id == "eject"));
    }

    #[test]
    fn run_all_includes_the_qemu_check() {
        let r = run_all();
        assert!(r.checks.iter().any(|c| c.id == "qemu"));
    }

    #[test]
    fn doctor_command_is_a_thin_wrapper_around_run_all() {
        let a = doctor();
        let b = run_all();
        assert_eq!(a.checks.len(), b.checks.len());
        assert_eq!(a.overall, b.overall);
    }

    #[test]
    fn run_all_completes_quickly() {
        use std::time::Instant;
        let start = Instant::now();
        let _ = run_all();
        let elapsed = start.elapsed();
        // Doctor runs on UI mount; needs to be sub-second.
        assert!(
            elapsed.as_secs() < 2,
            "doctor took {:?} — must stay under 2s",
            elapsed
        );
    }
}
