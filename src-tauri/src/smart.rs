//! SMART preflight — query the target device's self-reported health
//! before kicking off a destructive write. The intent is "prevent
//! burning to a drive that's already dying", not "be a full SMART
//! diagnostic suite". macOS exposes a single coarse `SMARTStatus` via
//! `diskutil info -plist`; that's what we read here. Linux/Windows
//! return [`SmartVerdict::Unsupported`] for now (placeholders, no
//! shell-out) so the rest of the pipeline doesn't need to feature-flag
//! the call site.
//!
//! USB sticks and SD cards almost never expose SMART, so a typical
//! verdict for the common "burn an ISO to a USB" case will be
//! [`SmartVerdict::Unknown`]. That's deliberately distinct from
//! [`SmartVerdict::Healthy`] so the UI can stay quiet on USB sticks
//! but still light up red on a verifiably-failing SSD.

use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SmartVerdict {
    /// Drive reports `SMARTStatus = Verified`. Safe to burn.
    Healthy,
    /// Drive reports a concerning status (`Failing`, `About to fail`,
    /// any non-`Verified`/`Not Supported`). Burning is still allowed
    /// — caller decides — but the UI should flag it loudly. The raw
    /// status string is included for forensics / log surfaces.
    Failing { status: String },
    /// `SMARTStatus = Not Supported` (typical for USB sticks, SD
    /// cards, virtual disks) or any value we don't recognise. Treated
    /// as informational; do not block a burn.
    Unknown { status: String },
    /// Platform doesn't have a SMART query implementation, or the
    /// query failed (e.g. `diskutil` exited non-zero). Treated as
    /// informational.
    Unsupported,
}

#[allow(dead_code)] // label/is_risky used by feat/smart-preflight's burn-log integration; not wired here
impl SmartVerdict {
    /// Short human label for log lines / UI badges.
    pub fn label(&self) -> &'static str {
        match self {
            SmartVerdict::Healthy => "healthy",
            SmartVerdict::Failing { .. } => "failing",
            SmartVerdict::Unknown { .. } => "unknown",
            SmartVerdict::Unsupported => "unsupported",
        }
    }

    /// True when the verdict says the drive should be considered at
    /// risk. Currently only [`SmartVerdict::Failing`].
    pub fn is_risky(&self) -> bool {
        matches!(self, SmartVerdict::Failing { .. })
    }
}

/// Query SMART status for the given block device. `device` is a
/// platform-native path (`/dev/disk5`, `\\.\PhysicalDrive5`, etc).
///
/// Always returns a verdict — never an error. Failures to query are
/// folded into [`SmartVerdict::Unsupported`] so callers can treat
/// the call as best-effort enrichment rather than a fallible step.
pub fn check(device: &str) -> SmartVerdict {
    #[cfg(target_os = "macos")]
    {
        check_macos(device)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = device;
        SmartVerdict::Unsupported
    }
}

#[cfg(target_os = "macos")]
fn check_macos(device: &str) -> SmartVerdict {
    use std::process::Command;

    let out = match Command::new("diskutil")
        .args(["info", "-plist", device])
        .output()
    {
        Ok(o) if o.status.success() => o,
        _ => return SmartVerdict::Unsupported,
    };
    parse_smart_status(&out.stdout).unwrap_or(SmartVerdict::Unsupported)
}

/// Pure parser — extracts the `SMARTStatus` field from a `diskutil
/// info -plist` output and maps it to a verdict. Split out so we can
/// unit-test all the verdict mappings without invoking diskutil.
///
/// Returns `None` only when the plist is unparseable. A plist that
/// parses but lacks `SMARTStatus` is mapped to
/// [`SmartVerdict::Unsupported`] (some virtual / RAID-member devices
/// behave that way).
pub fn parse_smart_status(bytes: &[u8]) -> Option<SmartVerdict> {
    let val: plist::Value = plist::from_bytes(bytes).ok()?;
    let dict = val.as_dictionary()?;
    let raw = match dict.get("SMARTStatus").and_then(|v| v.as_string()) {
        Some(s) => s.to_string(),
        None => return Some(SmartVerdict::Unsupported),
    };
    Some(classify(&raw))
}

/// Map a raw `SMARTStatus` string (as produced by diskutil) to a
/// verdict. Case-insensitive on the comparison so vendor variations
/// don't sneak past.
fn classify(raw: &str) -> SmartVerdict {
    let trimmed = raw.trim();
    let lower = trimmed.to_ascii_lowercase();
    match lower.as_str() {
        "verified" => SmartVerdict::Healthy,
        "not supported" | "" => SmartVerdict::Unknown {
            status: trimmed.to_string(),
        },
        // diskutil documents "Failing", but be liberal: any string
        // containing "fail" or "about to fail" should be treated as
        // failing; everything else we haven't seen we keep as Unknown
        // (informational) rather than Failing (alarming).
        s if s.contains("fail") => SmartVerdict::Failing {
            status: trimmed.to_string(),
        },
        _ => SmartVerdict::Unknown {
            status: trimmed.to_string(),
        },
    }
}

/// Tauri command — frontend can call this when a target is selected
/// to render a health badge in the disk picker. The command is
/// intentionally infallible from the frontend's perspective: a
/// missing tool, a non-existent device, etc all collapse to
/// [`SmartVerdict::Unsupported`] so the UI logic stays simple.
#[tauri::command]
pub fn smart_check(device: String) -> SmartVerdict {
    check(&device)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info_plist_with_smart(status: Option<&str>) -> Vec<u8> {
        let mut s = String::from(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
             <plist version=\"1.0\">\n<dict>\n",
        );
        if let Some(v) = status {
            s.push_str("  <key>SMARTStatus</key>\n");
            s.push_str(&format!("  <string>{v}</string>\n"));
        }
        s.push_str("</dict>\n</plist>\n");
        s.into_bytes()
    }

    #[test]
    fn classify_verified_is_healthy() {
        assert_eq!(classify("Verified"), SmartVerdict::Healthy);
        assert_eq!(classify("verified"), SmartVerdict::Healthy);
        assert_eq!(classify("  Verified  "), SmartVerdict::Healthy);
    }

    #[test]
    fn classify_not_supported_is_unknown() {
        match classify("Not Supported") {
            SmartVerdict::Unknown { status } => assert_eq!(status, "Not Supported"),
            v => panic!("expected Unknown, got {v:?}"),
        }
    }

    #[test]
    fn classify_failing_is_failing() {
        match classify("Failing") {
            SmartVerdict::Failing { status } => assert_eq!(status, "Failing"),
            v => panic!("expected Failing, got {v:?}"),
        }
        match classify("About to Fail") {
            SmartVerdict::Failing { status } => assert_eq!(status, "About to Fail"),
            v => panic!("expected Failing, got {v:?}"),
        }
    }

    #[test]
    fn classify_unknown_status_falls_through_to_unknown() {
        match classify("Verifying") {
            SmartVerdict::Unknown { status } => assert_eq!(status, "Verifying"),
            v => panic!("expected Unknown, got {v:?}"),
        }
    }

    #[test]
    fn classify_empty_string_is_unknown_not_healthy() {
        match classify("") {
            SmartVerdict::Unknown { status } => assert!(status.is_empty()),
            v => panic!("expected Unknown, got {v:?}"),
        }
    }

    #[test]
    fn parse_extracts_verified_status() {
        let xml = info_plist_with_smart(Some("Verified"));
        assert_eq!(parse_smart_status(&xml), Some(SmartVerdict::Healthy));
    }

    #[test]
    fn parse_extracts_failing_status() {
        let xml = info_plist_with_smart(Some("Failing"));
        match parse_smart_status(&xml).unwrap() {
            SmartVerdict::Failing { status } => assert_eq!(status, "Failing"),
            v => panic!("expected Failing, got {v:?}"),
        }
    }

    #[test]
    fn parse_with_missing_smart_key_is_unsupported() {
        let xml = info_plist_with_smart(None);
        assert_eq!(parse_smart_status(&xml), Some(SmartVerdict::Unsupported));
    }

    #[test]
    fn parse_returns_none_for_invalid_plist() {
        assert!(parse_smart_status(b"not a plist").is_none());
    }

    #[test]
    fn label_is_stable_per_variant() {
        assert_eq!(SmartVerdict::Healthy.label(), "healthy");
        assert_eq!(
            SmartVerdict::Failing { status: "x".into() }.label(),
            "failing"
        );
        assert_eq!(
            SmartVerdict::Unknown { status: "x".into() }.label(),
            "unknown"
        );
        assert_eq!(SmartVerdict::Unsupported.label(), "unsupported");
    }

    #[test]
    fn is_risky_only_for_failing() {
        assert!(!SmartVerdict::Healthy.is_risky());
        assert!(SmartVerdict::Failing {
            status: "Failing".into()
        }
        .is_risky());
        assert!(!SmartVerdict::Unknown {
            status: "Not Supported".into()
        }
        .is_risky());
        assert!(!SmartVerdict::Unsupported.is_risky());
    }

    #[test]
    fn check_for_nonexistent_device_is_unsupported() {
        // /dev/diskZZ doesn't exist; diskutil will exit non-zero and
        // we should fall through to Unsupported, not panic.
        let v = check("/dev/diskZZ-does-not-exist");
        assert_eq!(v, SmartVerdict::Unsupported);
    }
}
