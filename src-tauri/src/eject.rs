//! Eject a previously-burned target so the user can yank the
//! drive without an "improperly removed" warning. Pairs with the
//! `auto.eject` preference: the frontend invokes this from its
//! `job-complete` listener so the workflow is "burn → verify → eject
//! → user pulls the stick → plug in the next one".
//!
//! Backends:
//! - macOS: `diskutil eject <device>` — the only blessed way to
//!   release a DA-claimed disk.
//! - Linux: prefer `udisksctl unmount + power-off` (works inside a
//!   user session), fall back to `eject` (sysvinit-era; widely
//!   installed; works on many distros without root).
//! - Windows: not implemented yet — returns an Err so the frontend
//!   can show "eject not supported on this OS" instead of pretending
//!   it worked.
//!
//! Each backend is best-effort: a failure to eject doesn't roll
//! back the burn, it just surfaces a non-fatal warning.

use serde::Serialize;

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct EjectOutcome {
    pub success: bool,
    /// The path we tried to eject, echoed back for the frontend.
    pub device: String,
    /// Backend program we invoked (e.g. "diskutil"). Empty when no
    /// backend ran (unsupported platform).
    pub backend: String,
    /// Free-form note: success message or stderr from the failing
    /// backend.
    pub note: String,
}

/// Validate `device` looks like a real device path. We refuse empty
/// strings and anything containing whitespace or shell metacharacters
/// — the device path goes straight to a child process, so paranoid
/// wins. Real Linux/macOS device paths are simple ASCII (`/dev/sdb`,
/// `/dev/disk5`) and never contain spaces.
pub fn validate_device(raw: &str) -> Result<&str, &'static str> {
    let s = raw.trim();
    if s.is_empty() {
        return Err("device path is empty");
    }
    for ch in s.chars() {
        if ch.is_whitespace() {
            return Err("device path contains whitespace");
        }
        if matches!(ch, ';' | '|' | '&' | '`' | '$' | '(' | ')' | '<' | '>') {
            return Err("device path contains shell metacharacters");
        }
    }
    if !s.starts_with('/') {
        return Err("device path must start with '/'");
    }
    Ok(s)
}

#[tauri::command]
pub fn eject_disk(device: String) -> Result<EjectOutcome, String> {
    let dev = validate_device(&device).map_err(|e| e.to_string())?;
    Ok(eject(dev))
}

/// Plain function entry point. Always returns an `EjectOutcome` —
/// failures land in `success = false` rather than `Err` so the
/// frontend can render a uniform toast either way.
pub fn eject(device: &str) -> EjectOutcome {
    #[cfg(target_os = "macos")]
    {
        eject_macos(device)
    }
    #[cfg(target_os = "linux")]
    {
        eject_linux(device)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        EjectOutcome {
            success: false,
            device: device.to_string(),
            backend: String::new(),
            note: "eject not implemented on this platform".to_string(),
        }
    }
}

#[cfg(target_os = "macos")]
fn eject_macos(device: &str) -> EjectOutcome {
    use std::process::Command;
    let out = match Command::new("diskutil").args(["eject", device]).output() {
        Ok(o) => o,
        Err(e) => {
            return EjectOutcome {
                success: false,
                device: device.to_string(),
                backend: "diskutil".into(),
                note: format!("spawn failed: {e}"),
            };
        }
    };
    let note = if out.status.success() {
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stdout = String::from_utf8_lossy(&out.stdout);
        format!(
            "diskutil exited {}: {} {}",
            out.status,
            stderr.trim(),
            stdout.trim()
        )
        .trim()
        .to_string()
    };
    EjectOutcome {
        success: out.status.success(),
        device: device.to_string(),
        backend: "diskutil".into(),
        note,
    }
}

#[cfg(target_os = "linux")]
fn eject_linux(device: &str) -> EjectOutcome {
    use std::process::Command;
    // Prefer udisksctl (no root needed in a user session) — it
    // unmounts every partition then powers off the drive. Falls back
    // to the classic `eject` binary if udisksctl is missing.
    if let Ok(out) = Command::new("udisksctl")
        .args(["power-off", "-b", device])
        .output()
    {
        if out.status.success() {
            return EjectOutcome {
                success: true,
                device: device.to_string(),
                backend: "udisksctl".into(),
                note: String::from_utf8_lossy(&out.stdout).trim().to_string(),
            };
        }
        // udisksctl was found but failed — try `eject` next.
    }
    if let Ok(out) = Command::new("eject").arg(device).output() {
        return EjectOutcome {
            success: out.status.success(),
            device: device.to_string(),
            backend: "eject".into(),
            note: if out.status.success() {
                String::new()
            } else {
                String::from_utf8_lossy(&out.stderr).trim().to_string()
            },
        };
    }
    EjectOutcome {
        success: false,
        device: device.to_string(),
        backend: String::new(),
        note: "neither udisksctl nor eject is installed".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_device_accepts_typical_paths() {
        assert!(validate_device("/dev/disk5").is_ok());
        assert!(validate_device("/dev/sdb").is_ok());
        assert!(validate_device("/dev/nvme0n1").is_ok());
    }

    #[test]
    fn validate_device_trims_whitespace_around() {
        assert_eq!(validate_device("  /dev/disk5  ").unwrap(), "/dev/disk5");
    }

    #[test]
    fn validate_device_rejects_empty() {
        assert!(validate_device("").is_err());
        assert!(validate_device("   ").is_err());
    }

    #[test]
    fn validate_device_rejects_relative_paths() {
        assert!(validate_device("disk5").is_err());
        assert!(validate_device("./dev/disk5").is_err());
    }

    #[test]
    fn validate_device_rejects_shell_metacharacters() {
        for evil in &[
            "/dev/disk5; rm -rf /",
            "/dev/disk5 && shutdown",
            "/dev/disk5|cat",
            "/dev/disk5`whoami`",
            "/dev/disk5$(whoami)",
            "/dev/disk5>file",
        ] {
            assert!(validate_device(evil).is_err(), "should reject {evil:?}");
        }
    }

    #[test]
    fn validate_device_rejects_internal_whitespace() {
        assert!(validate_device("/dev/disk 5").is_err());
        assert!(validate_device("/dev/disk\t5").is_err());
    }

    #[test]
    fn eject_disk_command_rejects_invalid_device() {
        let r = eject_disk("not-a-path".into());
        assert!(r.is_err());
    }

    #[test]
    fn eject_disk_command_returns_outcome_for_valid_path() {
        // Path is well-formed but device almost certainly doesn't
        // exist; we expect either success=false (real backend ran
        // and complained) or success=false (no platform impl). Either
        // way, no panic and the device echoes back.
        let r = eject_disk("/dev/disk-cutter-nonexistent-zzzz".into());
        assert!(r.is_ok());
        let o = r.unwrap();
        assert_eq!(o.device, "/dev/disk-cutter-nonexistent-zzzz");
        assert!(!o.success);
    }
}
