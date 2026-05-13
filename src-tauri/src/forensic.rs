//! Forensic-grade burn record export.
//!
//! Produces a structured, tamper-evident record of every burn — what
//! image was written, to which device, by which host, when, with
//! which verification result, and a sha256 of the canonical JSON form
//! so any post-hoc edit is detectable. Use case: regulatory audit
//! trails ("we wrote OS-image-vX.Y.Z to N units on YYYY-MM-DD"),
//! incident response ("which images touched this drive in the last
//! 90 days"), and IT shops that need to prove what they did.
//!
//! Inputs are pure data: the `BurnRecord` + log rows already returned
//! by the SQLite layer, plus a `HostInfo` struct that the caller
//! gathers at export time. The render functions take those values
//! by reference and return strings — no I/O, no database access, no
//! current-time peeking — so every formatter is unit-testable with
//! synthetic inputs.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::db::{BurnLogRow, BurnRecord};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct HostInfo {
    pub os: String,
    pub arch: String,
    pub hostname: String,
    pub disk_cutter_version: String,
}

impl HostInfo {
    /// Capture host info at the moment of export. The fields are
    /// intentionally stable across the run (no current-time, no PID)
    /// so a re-rendered report from the same source data produces
    /// the same JSON and the same digest.
    pub fn current() -> Self {
        Self {
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            hostname: gethostname().unwrap_or_else(|| "unknown".into()),
            disk_cutter_version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }
}

fn gethostname() -> Option<String> {
    // libc::gethostname into a buffer; ignore errors.
    let mut buf = vec![0i8; 256];
    let rc = unsafe { libc::gethostname(buf.as_mut_ptr(), buf.len()) };
    if rc != 0 {
        return None;
    }
    let cstr = unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) };
    Some(cstr.to_string_lossy().into_owned())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ForensicReport {
    pub schema_version: u32,
    pub host: HostInfo,
    pub burn: BurnSection,
    pub logs: Vec<LogEntry>,
    pub digest_sha256: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BurnSection {
    pub job_id: String,
    pub image_name: String,
    pub image_path: String,
    pub image_size_bytes: u64,
    pub target_device: String,
    pub source_sha256: Option<String>,
    pub readback_sha256: Option<String>,
    pub verify_match: Option<bool>,
    pub bytes_written: Option<u64>,
    pub elapsed_ms: Option<u64>,
    pub avg_write_bps: Option<u64>,
    pub avg_verify_bps: Option<u64>,
    pub state: String,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub started_at_unix_ms: i64,
    pub finished_at_unix_ms: Option<i64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LogEntry {
    pub ts_unix_ms: i64,
    pub level: String,
    pub message: String,
}

/// Build a forensic report from already-fetched data. Pure — no I/O.
pub fn build_report(burn: &BurnRecord, logs: &[BurnLogRow], host: HostInfo) -> ForensicReport {
    let burn_section = BurnSection {
        job_id: burn.job_id.clone(),
        image_name: burn.image_name.clone(),
        image_path: burn.image_path.clone(),
        image_size_bytes: burn.image_bytes,
        target_device: burn.target_device.clone(),
        source_sha256: burn.source_sha256.clone(),
        readback_sha256: burn.readback_sha256.clone(),
        verify_match: burn.verify_match,
        bytes_written: burn.bytes_written,
        elapsed_ms: burn.elapsed_ms,
        avg_write_bps: burn.avg_write_bps,
        avg_verify_bps: burn.avg_verify_bps,
        state: burn.state.clone(),
        error_code: burn.error_code.clone(),
        error_message: burn.error_message.clone(),
        started_at_unix_ms: burn.started_at,
        finished_at_unix_ms: burn.finished_at,
    };
    let log_entries: Vec<LogEntry> = logs
        .iter()
        .map(|l| LogEntry {
            ts_unix_ms: l.ts,
            level: l.level.clone(),
            message: l.message.clone(),
        })
        .collect();

    let mut report = ForensicReport {
        schema_version: 1,
        host,
        burn: burn_section,
        logs: log_entries,
        digest_sha256: String::new(),
    };
    report.digest_sha256 = compute_digest(&report);
    report
}

/// Canonical JSON of the report excluding its own digest. We sort the
/// keys so the digest is deterministic regardless of the JSON library
/// version's field ordering, and we serialise the digest separately
/// from the body so a recompute can verify it without comparing the
/// digest against itself.
pub fn compute_digest(report: &ForensicReport) -> String {
    // Build a value tree without the digest field, then re-serialise
    // with sorted keys.
    let mut as_value = serde_json::to_value(report).expect("serializable");
    if let Some(obj) = as_value.as_object_mut() {
        obj.remove("digest_sha256");
    }
    let canonical = canonical_json(&as_value);
    let mut hasher = Sha256::new();
    hasher.update(canonical.as_bytes());
    hex(hasher.finalize())
}

/// Serialise a JSON value with object keys sorted alphabetically, no
/// extra whitespace. Used as the canonical form for the digest.
fn canonical_json(v: &serde_json::Value) -> String {
    use serde_json::Value;
    match v {
        Value::Object(map) => {
            let sorted: BTreeMap<&String, &Value> = map.iter().collect();
            let mut out = String::from("{");
            let mut first = true;
            for (k, val) in sorted {
                if !first {
                    out.push(',');
                }
                first = false;
                out.push_str(&serde_json::to_string(k).unwrap());
                out.push(':');
                out.push_str(&canonical_json(val));
            }
            out.push('}');
            out
        }
        Value::Array(items) => {
            let mut out = String::from("[");
            let mut first = true;
            for item in items {
                if !first {
                    out.push(',');
                }
                first = false;
                out.push_str(&canonical_json(item));
            }
            out.push(']');
            out
        }
        other => serde_json::to_string(other).unwrap(),
    }
}

/// Pretty-printed JSON for human inspection. Includes the digest in
/// the trailing field — the digest is computed over the report *minus*
/// itself, so re-running `compute_digest` on a parsed copy of the
/// pretty form yields the same hash.
pub fn to_pretty_json(report: &ForensicReport) -> String {
    serde_json::to_string_pretty(report).expect("serializable")
}

/// Render a human-readable Markdown summary, suitable for printing,
/// pasting into a Jira ticket, or feeding to `pandoc` for PDF. Doesn't
/// include the digest (that's a JSON-only construct) but mentions
/// where to find it.
pub fn to_markdown(report: &ForensicReport) -> String {
    let mut s = String::new();
    s.push_str("# Disk Cutter — Burn Report\n\n");
    s.push_str(&format!("- **Job ID:** {}\n", report.burn.job_id));
    s.push_str(&format!("- **State:** {}\n", report.burn.state));
    s.push_str(&format!(
        "- **Started:** {} (unix ms)\n",
        report.burn.started_at_unix_ms
    ));
    if let Some(t) = report.burn.finished_at_unix_ms {
        s.push_str(&format!("- **Finished:** {t} (unix ms)\n"));
    }
    s.push_str("\n## Source image\n");
    s.push_str(&format!("- Name: `{}`\n", report.burn.image_name));
    s.push_str(&format!("- Path: `{}`\n", report.burn.image_path));
    s.push_str(&format!("- Size: {} bytes\n", report.burn.image_size_bytes));
    if let Some(h) = &report.burn.source_sha256 {
        s.push_str(&format!("- sha256: `{h}`\n"));
    }
    s.push_str("\n## Target device\n");
    s.push_str(&format!("- Path: `{}`\n", report.burn.target_device));
    if let Some(bw) = report.burn.bytes_written {
        s.push_str(&format!("- Bytes written: {bw}\n"));
    }
    if let Some(elapsed) = report.burn.elapsed_ms {
        s.push_str(&format!("- Elapsed: {elapsed} ms\n"));
    }
    s.push_str("\n## Verification\n");
    if let Some(rh) = &report.burn.readback_sha256 {
        s.push_str(&format!("- readback sha256: `{rh}`\n"));
    }
    if let Some(m) = report.burn.verify_match {
        s.push_str(&format!("- Verify match: {}\n", if m { "✓" } else { "✗" }));
    }
    if let Some(code) = &report.burn.error_code {
        s.push_str(&format!("- Error code: `{code}`\n"));
    }
    if let Some(msg) = &report.burn.error_message {
        s.push_str(&format!("- Error message: {msg}\n"));
    }
    s.push_str("\n## Host\n");
    s.push_str(&format!("- OS: {}\n", report.host.os));
    s.push_str(&format!("- Arch: {}\n", report.host.arch));
    s.push_str(&format!("- Hostname: {}\n", report.host.hostname));
    s.push_str(&format!(
        "- Disk Cutter version: {}\n",
        report.host.disk_cutter_version
    ));
    s.push_str(&format!("\n## Log ({} entries)\n", report.logs.len()));
    if report.logs.is_empty() {
        s.push_str("(no log entries)\n");
    } else {
        for l in &report.logs {
            s.push_str(&format!(
                "- `{}` [{}] {}\n",
                l.ts_unix_ms, l.level, l.message
            ));
        }
    }
    s.push_str(&format!(
        "\n_Digest (JSON form): `{}`_\n",
        report.digest_sha256
    ));
    s
}

fn hex(bytes: impl AsRef<[u8]>) -> String {
    let mut s = String::with_capacity(bytes.as_ref().len() * 2);
    for b in bytes.as_ref() {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_burn() -> BurnRecord {
        BurnRecord {
            id: 1,
            job_id: "job-7".into(),
            image_path: "/tmp/ubuntu.iso".into(),
            image_name: "ubuntu.iso".into(),
            image_bytes: 4_700_000_000,
            target_device: "/dev/disk5".into(),
            source_sha256: Some("abc123".into()),
            readback_sha256: Some("abc123".into()),
            verify_match: Some(true),
            bytes_written: Some(4_700_000_000),
            elapsed_ms: Some(120_000),
            avg_write_bps: Some(40_000_000),
            avg_verify_bps: Some(80_000_000),
            state: "completed".into(),
            error_code: None,
            error_message: None,
            started_at: 1_715_600_000_000,
            finished_at: Some(1_715_600_120_000),
        }
    }

    fn sample_host() -> HostInfo {
        HostInfo {
            os: "macos".into(),
            arch: "aarch64".into(),
            hostname: "test-host".into(),
            disk_cutter_version: "0.4.0-alpha".into(),
        }
    }

    #[test]
    fn build_report_copies_fields_from_burn_record() {
        let r = build_report(&sample_burn(), &[], sample_host());
        assert_eq!(r.schema_version, 1);
        assert_eq!(r.burn.job_id, "job-7");
        assert_eq!(r.burn.image_name, "ubuntu.iso");
        assert_eq!(r.burn.image_size_bytes, 4_700_000_000);
        assert_eq!(r.burn.target_device, "/dev/disk5");
        assert_eq!(r.burn.source_sha256.as_deref(), Some("abc123"));
        assert_eq!(r.burn.verify_match, Some(true));
    }

    #[test]
    fn build_report_collects_logs() {
        let logs = vec![
            BurnLogRow {
                id: 1,
                burn_id: 1,
                ts: 1000,
                level: "info".into(),
                message: "start".into(),
            },
            BurnLogRow {
                id: 2,
                burn_id: 1,
                ts: 2000,
                level: "info".into(),
                message: "done".into(),
            },
        ];
        let r = build_report(&sample_burn(), &logs, sample_host());
        assert_eq!(r.logs.len(), 2);
        assert_eq!(r.logs[1].message, "done");
    }

    #[test]
    fn digest_is_deterministic_for_same_inputs() {
        let r1 = build_report(&sample_burn(), &[], sample_host());
        let r2 = build_report(&sample_burn(), &[], sample_host());
        assert_eq!(r1.digest_sha256, r2.digest_sha256);
        assert_eq!(r1.digest_sha256.len(), 64); // sha256 hex
    }

    #[test]
    fn digest_changes_when_any_field_changes() {
        let r1 = build_report(&sample_burn(), &[], sample_host());
        let mut alt = sample_burn();
        alt.bytes_written = Some(4_700_000_001);
        let r2 = build_report(&alt, &[], sample_host());
        assert_ne!(r1.digest_sha256, r2.digest_sha256);
    }

    #[test]
    fn digest_is_tamper_evident_for_logs() {
        let logs = vec![BurnLogRow {
            id: 1,
            burn_id: 1,
            ts: 1000,
            level: "info".into(),
            message: "ok".into(),
        }];
        let r1 = build_report(&sample_burn(), &logs, sample_host());

        let altered = vec![BurnLogRow {
            id: 1,
            burn_id: 1,
            ts: 1000,
            level: "info".into(),
            message: "tampered".into(),
        }];
        let r2 = build_report(&sample_burn(), &altered, sample_host());
        assert_ne!(r1.digest_sha256, r2.digest_sha256);
    }

    #[test]
    fn compute_digest_recovers_same_value_after_json_round_trip() {
        let r1 = build_report(&sample_burn(), &[], sample_host());
        let json = to_pretty_json(&r1);
        let r2: ForensicReport = serde_json::from_str(&json).unwrap();
        let recomputed = compute_digest(&r2);
        assert_eq!(recomputed, r1.digest_sha256);
    }

    #[test]
    fn canonical_json_sorts_object_keys() {
        let v: serde_json::Value =
            serde_json::from_str(r#"{"b": 1, "a": 2, "c": [3, 4]}"#).unwrap();
        assert_eq!(canonical_json(&v), r#"{"a":2,"b":1,"c":[3,4]}"#);
    }

    #[test]
    fn to_pretty_json_contains_all_top_level_sections() {
        let r = build_report(&sample_burn(), &[], sample_host());
        let s = to_pretty_json(&r);
        for needle in &[
            "schema_version",
            "host",
            "burn",
            "logs",
            "digest_sha256",
            "ubuntu.iso",
            "/dev/disk5",
        ] {
            assert!(s.contains(needle), "missing {needle} in pretty JSON");
        }
    }

    #[test]
    fn to_markdown_renders_human_readable_report() {
        let r = build_report(&sample_burn(), &[], sample_host());
        let md = to_markdown(&r);
        assert!(md.starts_with("# Disk Cutter — Burn Report"));
        assert!(md.contains("Job ID:** job-7"));
        assert!(md.contains("ubuntu.iso"));
        assert!(md.contains("/dev/disk5"));
        assert!(md.contains("Verify match: ✓"));
        assert!(md.contains(&r.digest_sha256));
    }

    #[test]
    fn to_markdown_renders_failed_burn_with_error() {
        let mut b = sample_burn();
        b.state = "failed".into();
        b.verify_match = None;
        b.error_code = Some("EIO".into());
        b.error_message = Some("disk gone".into());
        let r = build_report(&b, &[], sample_host());
        let md = to_markdown(&r);
        assert!(md.contains("Error code: `EIO`"));
        assert!(md.contains("Error message: disk gone"));
    }

    #[test]
    fn host_info_current_populates_os_and_arch() {
        let h = HostInfo::current();
        assert!(!h.os.is_empty());
        assert!(!h.arch.is_empty());
        assert!(!h.disk_cutter_version.is_empty());
    }
}
