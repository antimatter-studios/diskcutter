use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::db::{self, Db};
use crate::pipeline::{self, BurnError, VerifyMismatch};
use crate::readers::ImageReaderRegistry;
#[cfg(unix)]
use crate::writers::RawDeviceIo;
use crate::writers::{DeviceIo, PlainFileDeviceIo};

#[derive(Serialize, Clone)]
pub struct ImageDetails {
    pub path: String,
    pub name: String,
    pub format: String,
    pub source_bytes: u64,
    pub uncompressed_bytes: u64,
    pub sectors: u64,
    pub sha256: Option<String>,
}

#[tauri::command]
pub fn inspect_image(path: String) -> Result<ImageDetails, String> {
    let p = Path::new(&path);
    let registry = ImageReaderRegistry::with_defaults();
    let (info, _factory) = registry
        .probe(p)
        .ok_or_else(|| format!("unsupported image format: {path}"))?;
    let name = p
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();
    Ok(ImageDetails {
        path: info.path.to_string_lossy().to_string(),
        name,
        format: info.format_label,
        source_bytes: info.source_bytes,
        uncompressed_bytes: info.uncompressed_bytes,
        sectors: info.uncompressed_bytes / 512,
        sha256: None,
    })
}

#[derive(Serialize, Clone)]
pub struct Disk {
    pub device: String,
    pub model: String,
    pub capacity: String,
    pub bytes: u64,
    pub bus: String,
    pub partitions: String,
    pub flags: Vec<String>,
}

#[derive(Serialize, Clone)]
pub struct JobUpdate {
    pub job_id: String,
    pub state: String,
    pub progress: f32,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub speed: String,
    pub eta: String,
    pub message: Option<String>,
}

#[derive(Serialize, Clone)]
pub struct JobComplete {
    pub job_id: String,
    pub bytes_written: u64,
    pub source_sha256: String,
    pub readback_sha256: String,
    pub verify_match: bool,
    pub mismatches: Vec<VerifyMismatch>,
    pub elapsed_ms: u64,
    pub avg_write_bps: u64,
    pub avg_verify_bps: u64,
}

#[derive(Serialize, Clone)]
pub struct JobFailure {
    pub job_id: String,
    pub error_code: String,
    pub error_message: String,
}

#[derive(Default)]
pub struct CancelRegistry(pub Mutex<HashMap<String, Arc<AtomicBool>>>);

#[derive(Serialize, Clone)]
pub struct AppInfo {
    pub version: String,
    pub os: String,
    pub arch: String,
    pub is_privileged: bool,
}

#[tauri::command]
pub fn app_info() -> AppInfo {
    AppInfo {
        version: env!("CARGO_PKG_VERSION").to_string(),
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        is_privileged: is_privileged(),
    }
}

#[tauri::command]
pub fn open_fda_settings() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles")
            .spawn()
            .map_err(|e| e.to_string())?;
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err("not implemented on this platform".to_string())
    }
}

fn is_privileged() -> bool {
    #[cfg(unix)]
    {
        unsafe { libc::geteuid() == 0 }
    }
    #[cfg(not(unix))]
    {
        false
    }
}

#[tauri::command]
pub fn list_disks() -> Vec<Disk> {
    #[cfg(target_os = "macos")]
    {
        enumerate_macos().unwrap_or_default()
    }
    #[cfg(not(target_os = "macos"))]
    {
        // TODO: real enumeration for Linux (/sys/block) and Windows (SetupDi).
        Vec::new()
    }
}

#[cfg(target_os = "macos")]
fn enumerate_macos() -> Option<Vec<Disk>> {
    use std::process::Command;

    let list = Command::new("diskutil").args(["list", "-plist"]).output().ok()?;
    if !list.status.success() {
        return None;
    }
    let ids = parse_disks_plist(&list.stdout)?;

    let mut out = Vec::new();
    for id in ids {
        if let Some(d) = info_for_macos(&id) {
            out.push(d);
        }
    }
    Some(out)
}

fn is_whole_disk(id: &str) -> bool {
    id.strip_prefix("disk")
        .map(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
        .unwrap_or(false)
}

fn parse_disks_plist(bytes: &[u8]) -> Option<Vec<String>> {
    let val: plist::Value = plist::from_bytes(bytes).ok()?;
    let all = val.as_dictionary()?.get("AllDisks")?.as_array()?;
    Some(
        all.iter()
            .filter_map(|e| e.as_string())
            .filter(|id| is_whole_disk(id))
            .map(|id| id.to_string())
            .collect(),
    )
}

#[cfg(target_os = "macos")]
fn info_for_macos(id: &str) -> Option<Disk> {
    use std::process::Command;

    let path = format!("/dev/{id}");
    let out = Command::new("diskutil")
        .args(["info", "-plist", &path])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_disk_info_plist(&out.stdout, path)
}

fn parse_disk_info_plist(bytes: &[u8], device_path: String) -> Option<Disk> {
    let val: plist::Value = plist::from_bytes(bytes).ok()?;
    let dict = val.as_dictionary()?;

    let s = |k: &str| dict.get(k).and_then(|v| v.as_string()).map(|s| s.to_string());
    let u = |k: &str| dict.get(k).and_then(|v| v.as_unsigned_integer()).unwrap_or(0);
    let b = |k: &str| dict.get(k).and_then(|v| v.as_boolean()).unwrap_or(false);

    let model = s("MediaName").or_else(|| s("IORegistryEntryName")).unwrap_or_else(|| "UNKNOWN".to_string());
    let bytes_total = u("TotalSize");
    let bus = s("BusProtocol").unwrap_or_else(|| "UNKNOWN".to_string()).to_uppercase();
    let internal = b("Internal");
    let removable = b("Removable") || b("RemovableMedia") || b("RemovableMediaOrExternalDevice");

    let mut flags = Vec::new();
    if internal {
        flags.push("INTERNAL".to_string());
    }
    if removable {
        flags.push("REMOVABLE".to_string());
    }

    Some(Disk {
        device: device_path,
        model: model.to_uppercase(),
        capacity: format_capacity(bytes_total),
        bytes: bytes_total,
        bus,
        partitions: derive_partitions(dict),
        flags,
    })
}

fn format_capacity(bytes: u64) -> String {
    let b = bytes as f64;
    if b >= 1e12 {
        format!("{:.2} TB", b / 1e12)
    } else if b >= 1e9 {
        format!("{:.2} GB", b / 1e9)
    } else if b >= 1e6 {
        format!("{:.1} MB", b / 1e6)
    } else if bytes == 0 {
        "—".to_string()
    } else {
        format!("{bytes} B")
    }
}

fn derive_partitions(dict: &plist::Dictionary) -> String {
    let fs = dict
        .get("FilesystemType")
        .and_then(|v| v.as_string())
        .or_else(|| {
            dict.get("Content")
                .and_then(|v| v.as_string())
        })
        .unwrap_or("")
        .to_uppercase();
    let name = dict
        .get("VolumeName")
        .and_then(|v| v.as_string())
        .unwrap_or("")
        .to_uppercase();
    match (name.is_empty(), fs.is_empty()) {
        (false, false) => format!("{name} · {fs}"),
        (true, false) => format!("1 PARTITION · {fs}"),
        (false, true) => name,
        (true, true) => "UNFORMATTED".to_string(),
    }
}

#[tauri::command]
pub fn start_write(
    app: AppHandle,
    cancel: State<'_, CancelRegistry>,
    job_id: String,
    image_path: String,
    target_device: String,
) -> Result<(), String> {
    let p = Path::new(&image_path);
    let registry = ImageReaderRegistry::with_defaults();
    let (info, _factory) = registry
        .probe(p)
        .ok_or_else(|| format!("unsupported image format: {image_path}"))?;
    let image_name = p
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();

    {
        let db = app.state::<Db>();
        db::record_burn_started(
            &db,
            &job_id,
            &image_path,
            &image_name,
            info.uncompressed_bytes,
            &target_device,
        );
    }

    let needs_elevation = target_device.starts_with("/dev/") && !is_privileged();
    if needs_elevation {
        return spawn_elevated_burn(app, job_id, image_path, target_device);
    }

    let flag = Arc::new(AtomicBool::new(false));
    cancel
        .0
        .lock()
        .unwrap()
        .insert(job_id.clone(), flag.clone());

    let app = app.clone();
    let id = job_id;
    let image = image_path;
    let target = target_device;

    std::thread::spawn(move || {
        let outcome = run_job(&app, &id, &image, &target, &flag);
        let db = app.state::<Db>();
        match outcome {
            Ok(complete) => {
                db::record_burn_completed(
                    &db,
                    &complete.job_id,
                    &complete.source_sha256,
                    &complete.readback_sha256,
                    complete.verify_match,
                    complete.bytes_written,
                    complete.elapsed_ms,
                    complete.avg_write_bps,
                    complete.avg_verify_bps,
                );
                let _ = app.emit("disk-cutter://job-complete", complete);
            }
            Err(failure) => {
                db::record_burn_failed(
                    &db,
                    &failure.job_id,
                    &failure.error_code,
                    &failure.error_message,
                );
                let _ = app.emit("disk-cutter://job-error", failure);
            }
        }
    });

    Ok(())
}

fn spawn_elevated_burn(
    app: AppHandle,
    job_id: String,
    image_path: String,
    target: String,
) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| {
        let msg = e.to_string();
        db::record_burn_failed(&app.state::<Db>(), &job_id, "EHELPER", &msg);
        msg
    })?;
    let progress_path = format!("/tmp/disk-cutter-progress-{job_id}.jsonl");
    let _ = std::fs::remove_file(&progress_path);

    let helper_cmd = build_helper_command(
        &exe.to_string_lossy(),
        &image_path,
        &target,
        &job_id,
        &progress_path,
    );
    let prompt = "Disk Cutter needs administrator access to write the disk image directly to the device you selected.";
    let script = build_osascript_script(&helper_cmd, prompt);

    let child = std::process::Command::new("osascript")
        .args(["-e", &script])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| {
            let msg = format!("osascript spawn failed: {e}");
            db::record_burn_failed(&app.state::<Db>(), &job_id, "EHELPER", &msg);
            msg
        })?;

    let app_for_tail = app.clone();
    let job_for_tail = job_id.clone();
    let progress_for_tail = progress_path.clone();
    std::thread::spawn(move || {
        tail_helper(app_for_tail, job_for_tail, progress_for_tail, child);
    });

    Ok(())
}

fn sq(s: &str) -> String {
    // single-quote escape for bash inside AppleScript double-quoted string
    s.replace('\'', "'\\''")
}

fn build_helper_command(
    exe: &str,
    image: &str,
    target: &str,
    job_id: &str,
    progress: &str,
) -> String {
    format!(
        "'{}' --helper-burn --image='{}' --target='{}' --job='{}' --progress='{}'",
        sq(exe),
        sq(image),
        sq(target),
        sq(job_id),
        sq(progress),
    )
}

fn build_osascript_script(helper_cmd: &str, prompt: &str) -> String {
    format!(
        "do shell script \"{}\" with prompt \"{}\" with administrator privileges",
        helper_cmd.replace('\\', "\\\\").replace('"', "\\\""),
        prompt
    )
}

fn tail_helper(app: AppHandle, job_id: String, path: String, mut child: std::process::Child) {
    use std::io::{BufRead, BufReader, Seek, SeekFrom};
    use std::time::{Duration, Instant};

    let start = Instant::now();
    let mut last_pos: u64 = 0;
    let mut terminal_seen = false;

    loop {
        if std::path::Path::new(&path).exists() {
            if let Ok(mut file) = std::fs::File::open(&path) {
                let _ = file.seek(SeekFrom::Start(last_pos));
                let mut reader = BufReader::new(file);
                loop {
                    let mut line = String::new();
                    match reader.read_line(&mut line) {
                        Ok(0) => break,
                        Ok(n) => {
                            last_pos += n as u64;
                            if let Some(kind) = emit_helper_line(&app, &job_id, &line) {
                                if kind == "complete" || kind == "error" {
                                    terminal_seen = true;
                                }
                            }
                        }
                        Err(_) => break,
                    }
                }
            }
        }
        if terminal_seen {
            break;
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                // osascript exited but no terminal message — likely auth cancelled.
                if !terminal_seen {
                    let code = if status.success() { "EHELPER" } else { "EAUTH" };
                    let msg = if status.success() {
                        "helper exited without progress".to_string()
                    } else {
                        "authorization cancelled or helper failed".to_string()
                    };
                    db::record_burn_failed(&app.state::<Db>(), &job_id, code, &msg);
                    let _ = app.emit(
                        "disk-cutter://job-error",
                        JobFailure {
                            job_id: job_id.clone(),
                            error_code: code.into(),
                            error_message: msg,
                        },
                    );
                }
                break;
            }
            Ok(None) => {}
            Err(_) => break,
        }
        if start.elapsed() > Duration::from_secs(3600) {
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    let _ = std::fs::remove_file(&path);
}

enum HelperEvent {
    Progress(JobUpdate),
    Complete(JobComplete),
    Failure(JobFailure),
}

fn parse_helper_line(job_id: &str, line: &str) -> Option<HelperEvent> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    let val: serde_json::Value = serde_json::from_str(trimmed).ok()?;
    let kind = val.get("kind")?.as_str()?;
    match kind {
        "progress" => {
            let bytes_done = val.get("bytes_done").and_then(|v| v.as_u64()).unwrap_or(0);
            let bytes_total = val.get("bytes_total").and_then(|v| v.as_u64()).unwrap_or(0);
            let bps = val.get("bytes_per_sec").and_then(|v| v.as_u64()).unwrap_or(0);
            let state = val.get("state").and_then(|v| v.as_str()).unwrap_or("writing");
            Some(HelperEvent::Progress(make_job_update(
                job_id, state, bytes_done, bytes_total, bps,
            )))
        }
        "complete" => {
            let mismatches: Vec<crate::pipeline::VerifyMismatch> =
                serde_json::from_value(val.get("mismatches").cloned().unwrap_or(serde_json::Value::Array(vec![])))
                    .unwrap_or_default();
            Some(HelperEvent::Complete(JobComplete {
                job_id: job_id.to_string(),
                bytes_written: val.get("bytes_written").and_then(|v| v.as_u64()).unwrap_or(0),
                source_sha256: val.get("source_sha256").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                readback_sha256: val.get("readback_sha256").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                verify_match: val.get("verify_match").and_then(|v| v.as_bool()).unwrap_or(false),
                mismatches,
                elapsed_ms: val.get("elapsed_ms").and_then(|v| v.as_u64()).unwrap_or(0),
                avg_write_bps: val.get("avg_write_bps").and_then(|v| v.as_u64()).unwrap_or(0),
                avg_verify_bps: val.get("avg_verify_bps").and_then(|v| v.as_u64()).unwrap_or(0),
            }))
        }
        "error" => Some(HelperEvent::Failure(JobFailure {
            job_id: job_id.to_string(),
            error_code: val.get("error_code").and_then(|v| v.as_str()).unwrap_or("EUNKNOWN").to_string(),
            error_message: val.get("error_message").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        })),
        _ => None,
    }
}

fn emit_helper_line(app: &AppHandle, job_id: &str, line: &str) -> Option<&'static str> {
    match parse_helper_line(job_id, line)? {
        HelperEvent::Progress(update) => {
            let _ = app.emit("disk-cutter://job-update", update);
            Some("progress")
        }
        HelperEvent::Complete(complete) => {
            db::record_burn_completed(
                &app.state::<Db>(),
                &complete.job_id,
                &complete.source_sha256,
                &complete.readback_sha256,
                complete.verify_match,
                complete.bytes_written,
                complete.elapsed_ms,
                complete.avg_write_bps,
                complete.avg_verify_bps,
            );
            let _ = app.emit("disk-cutter://job-complete", complete);
            Some("complete")
        }
        HelperEvent::Failure(failure) => {
            db::record_burn_failed(
                &app.state::<Db>(),
                &failure.job_id,
                &failure.error_code,
                &failure.error_message,
            );
            let _ = app.emit("disk-cutter://job-error", failure);
            Some("error")
        }
    }
}

#[tauri::command]
pub fn cancel_write(cancel: State<'_, CancelRegistry>, job_id: String) -> Result<(), String> {
    if let Some(flag) = cancel.0.lock().unwrap().get(&job_id) {
        flag.store(true, Ordering::Relaxed);
    }
    Ok(())
}

#[tauri::command]
pub fn verify_image(_job_id: String) -> Result<(), String> {
    // Verify runs automatically after a successful write inside `start_write`.
    // Kept as a no-op for compatibility while the frontend transitions.
    Ok(())
}

fn make_job_update(
    job_id: &str,
    state: &str,
    bytes_done: u64,
    bytes_total: u64,
    bytes_per_sec: u64,
) -> JobUpdate {
    JobUpdate {
        job_id: job_id.to_string(),
        state: state.to_string(),
        progress: pct(bytes_done, bytes_total),
        bytes_done,
        bytes_total,
        speed: format_speed(bytes_per_sec),
        eta: format_eta(bytes_done, bytes_total, bytes_per_sec),
        message: None,
    }
}

fn run_job(
    app: &AppHandle,
    job_id: &str,
    image_path: &str,
    target_device: &str,
    cancel: &AtomicBool,
) -> Result<JobComplete, JobFailure> {
    let image = Path::new(image_path);
    let target = Path::new(target_device);

    let registry = ImageReaderRegistry::with_defaults();
    let (_info, factory) = registry
        .probe(image)
        .ok_or_else(|| fail(job_id, "EUNSUPPORTED", "unsupported image format"))?;

    let mut reader = factory
        .open(image)
        .map_err(|e| fail(job_id, "EIMAGE", &format!("open image: {e}")))?;

    let device_io: Box<dyn DeviceIo> = pick_device_io(target_device);
    let writer = device_io
        .open_write(target)
        .map_err(|e| fail(job_id, "ETARGET", &format!("open target: {e}")))?;

    let burn_id = job_id.to_string();
    let burn_app = app.clone();
    let burn = pipeline::burn(&mut *reader, writer, cancel, |p| {
        let _ = burn_app.emit(
            "disk-cutter://job-update",
            make_job_update(&burn_id, "writing", p.bytes_done, p.bytes_total, p.bytes_per_sec),
        );
    })
    .map_err(|e| fail_for_burn_error(job_id, &e))?;

    let mut reader2 = factory
        .open(image)
        .map_err(|e| fail(job_id, "EIMAGE", &format!("reopen image: {e}")))?;
    let mut device_reader = device_io
        .open_read(target)
        .map_err(|e| fail(job_id, "ETARGET", &format!("reopen target: {e}")))?;

    let verify_id = job_id.to_string();
    let verify_app = app.clone();
    let verify = pipeline::verify(&mut *reader2, &mut *device_reader, cancel, |p| {
        let _ = verify_app.emit(
            "disk-cutter://job-update",
            make_job_update(&verify_id, "verifying", p.bytes_done, p.bytes_total, p.bytes_per_sec),
        );
    })
    .map_err(|e| fail_for_burn_error(job_id, &e))?;

    Ok(summarize_burn_complete(job_id, burn, verify))
}

fn fail(job_id: &str, code: &str, msg: &str) -> JobFailure {
    JobFailure {
        job_id: job_id.to_string(),
        error_code: code.to_string(),
        error_message: msg.to_string(),
    }
}

fn pick_device_io(target: &str) -> Box<dyn DeviceIo> {
    if target.starts_with("/dev/") && is_privileged() {
        #[cfg(unix)]
        {
            return Box::new(RawDeviceIo);
        }
        #[cfg(not(unix))]
        {
            return Box::new(PlainFileDeviceIo);
        }
    }
    Box::new(PlainFileDeviceIo)
}

fn code_for(e: &BurnError) -> &'static str {
    match e {
        BurnError::Cancelled => "ECANCELLED",
        BurnError::SizeMismatch { .. } => "ESIZEMISMATCH",
        BurnError::Io(_) => "EIO",
    }
}

fn fail_for_burn_error(job_id: &str, e: &BurnError) -> JobFailure {
    fail(job_id, code_for(e), &format!("{e:?}"))
}

fn summarize_burn_complete(
    job_id: &str,
    burn: pipeline::BurnResult,
    verify: pipeline::VerifyResult,
) -> JobComplete {
    let elapsed_ms = (burn.elapsed.as_millis() + verify.elapsed.as_millis()) as u64;
    JobComplete {
        job_id: job_id.to_string(),
        bytes_written: burn.bytes_written,
        source_sha256: burn.source_sha256,
        readback_sha256: verify.readback_sha256,
        verify_match: verify.match_,
        mismatches: verify.mismatches,
        elapsed_ms,
        avg_write_bps: burn.avg_bytes_per_sec,
        avg_verify_bps: verify.avg_bytes_per_sec,
    }
}

fn pct(done: u64, total: u64) -> f32 {
    if total == 0 {
        return 0.0;
    }
    (done as f64 / total as f64 * 100.0) as f32
}

fn format_speed(bps: u64) -> String {
    let b = bps as f64;
    if b >= 1e9 {
        format!("{:.2} GB/s", b / 1e9)
    } else if b >= 1e6 {
        format!("{:.1} MB/s", b / 1e6)
    } else if b >= 1e3 {
        format!("{:.0} kB/s", b / 1e3)
    } else {
        format!("{bps} B/s")
    }
}

fn format_eta(done: u64, total: u64, bps: u64) -> String {
    if bps == 0 || total <= done {
        return "00:00".to_string();
    }
    let remaining = (total - done) / bps.max(1);
    let m = remaining / 60;
    let s = remaining % 60;
    if m >= 100 {
        format!("{m}m")
    } else {
        format!("{m:02}:{s:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_whole_disk_accepts_disk_with_trailing_digits() {
        assert!(is_whole_disk("disk0"));
        assert!(is_whole_disk("disk42"));
    }

    #[test]
    fn is_whole_disk_rejects_partitions_and_rdisk_and_others() {
        assert!(!is_whole_disk("disk0s1"));
        assert!(!is_whole_disk("rdisk0"));
        assert!(!is_whole_disk("disk"));
        assert!(!is_whole_disk(""));
        assert!(!is_whole_disk("sda"));
        assert!(!is_whole_disk("disk5a"));
    }

    #[test]
    fn format_capacity_selects_correct_unit() {
        assert_eq!(format_capacity(0), "—");
        assert_eq!(format_capacity(512), "512 B");
        assert_eq!(format_capacity(1_500_000), "1.5 MB");
        assert_eq!(format_capacity(2_000_000_000), "2.00 GB");
        assert_eq!(format_capacity(3_000_000_000_000), "3.00 TB");
    }

    #[test]
    fn format_speed_selects_correct_unit() {
        assert_eq!(format_speed(0), "0 B/s");
        assert_eq!(format_speed(500), "500 B/s");
        assert_eq!(format_speed(4_000), "4 kB/s");
        assert_eq!(format_speed(2_500_000), "2.5 MB/s");
        assert_eq!(format_speed(3_000_000_000), "3.00 GB/s");
    }

    #[test]
    fn format_eta_emits_double_zero_for_no_progress() {
        assert_eq!(format_eta(0, 100, 0), "00:00");
        assert_eq!(format_eta(100, 100, 50), "00:00");
        assert_eq!(format_eta(200, 100, 50), "00:00");
    }

    #[test]
    fn format_eta_renders_minutes_seconds() {
        assert_eq!(format_eta(0, 600, 10), "01:00");
        assert_eq!(format_eta(0, 100, 10), "00:10");
    }

    #[test]
    fn format_eta_collapses_above_100_minutes() {
        assert_eq!(format_eta(0, 6_000_000, 1_000), "100m");
    }

    #[test]
    fn pct_returns_zero_when_total_zero() {
        assert_eq!(pct(0, 0), 0.0);
        assert_eq!(pct(50, 0), 0.0);
    }

    #[test]
    fn pct_computes_percentage() {
        assert!((pct(50, 100) - 50.0).abs() < f32::EPSILON);
        assert!((pct(100, 200) - 50.0).abs() < f32::EPSILON);
    }

    #[test]
    fn fail_assembles_job_failure() {
        let f = fail("job-7", "EIO", "I/O blew up");
        assert_eq!(f.job_id, "job-7");
        assert_eq!(f.error_code, "EIO");
        assert_eq!(f.error_message, "I/O blew up");
    }

    #[test]
    fn code_for_maps_each_burn_error_variant() {
        use std::io;
        assert_eq!(code_for(&BurnError::Cancelled), "ECANCELLED");
        assert_eq!(
            code_for(&BurnError::SizeMismatch {
                expected: 1,
                actual: 2,
            }),
            "ESIZEMISMATCH"
        );
        assert_eq!(
            code_for(&BurnError::Io(io::Error::new(io::ErrorKind::Other, "x"))),
            "EIO"
        );
    }

    #[test]
    fn fail_for_burn_error_assembles_code_and_debug_message() {
        let f = fail_for_burn_error("job-1", &BurnError::Cancelled);
        assert_eq!(f.job_id, "job-1");
        assert_eq!(f.error_code, "ECANCELLED");
        assert!(f.error_message.contains("Cancelled"), "got {}", f.error_message);
    }

    #[test]
    fn fail_for_burn_error_maps_io_variant() {
        use std::io;
        let e = BurnError::Io(io::Error::new(io::ErrorKind::Other, "x"));
        let f = fail_for_burn_error("j", &e);
        assert_eq!(f.error_code, "EIO");
    }

    #[test]
    fn fail_for_burn_error_maps_size_mismatch_variant() {
        let f = fail_for_burn_error(
            "j",
            &BurnError::SizeMismatch { expected: 1, actual: 2 },
        );
        assert_eq!(f.error_code, "ESIZEMISMATCH");
    }

    #[test]
    fn summarize_burn_complete_aggregates_burn_and_verify_results() {
        use std::time::Duration;
        let burn = pipeline::BurnResult {
            bytes_written: 1024,
            source_sha256: "abc".into(),
            elapsed: Duration::from_millis(500),
            avg_bytes_per_sec: 100,
        };
        let verify = pipeline::VerifyResult {
            source_sha256: "abc".into(),
            readback_sha256: "abc".into(),
            match_: true,
            bytes_checked: 1024,
            bytes_total: 1024,
            mismatches: vec![],
            elapsed: Duration::from_millis(300),
            avg_bytes_per_sec: 200,
        };
        let c = summarize_burn_complete("job-9", burn, verify);
        assert_eq!(c.job_id, "job-9");
        assert_eq!(c.bytes_written, 1024);
        assert_eq!(c.source_sha256, "abc");
        assert_eq!(c.readback_sha256, "abc");
        assert!(c.verify_match);
        assert_eq!(c.elapsed_ms, 800);
        assert_eq!(c.avg_write_bps, 100);
        assert_eq!(c.avg_verify_bps, 200);
        assert!(c.mismatches.is_empty());
    }

    #[test]
    fn summarize_burn_complete_carries_mismatches_when_verify_failed() {
        use std::time::Duration;
        let burn = pipeline::BurnResult {
            bytes_written: 8,
            source_sha256: "s".into(),
            elapsed: Duration::from_millis(0),
            avg_bytes_per_sec: 0,
        };
        let verify = pipeline::VerifyResult {
            source_sha256: "s".into(),
            readback_sha256: "d".into(),
            match_: false,
            bytes_checked: 8,
            bytes_total: 8,
            mismatches: vec![pipeline::VerifyMismatch {
                lba: "0x1".into(),
                byte_offset: "+0x0".into(),
                expected: "AA".into(),
                actual: "BB".into(),
            }],
            elapsed: Duration::from_millis(0),
            avg_bytes_per_sec: 0,
        };
        let c = summarize_burn_complete("j", burn, verify);
        assert!(!c.verify_match);
        assert_eq!(c.mismatches.len(), 1);
    }

    #[test]
    fn make_job_update_carries_id_and_state_and_formats_speed() {
        let u = make_job_update("job-9", "writing", 500, 1000, 2_500_000);
        assert_eq!(u.job_id, "job-9");
        assert_eq!(u.state, "writing");
        assert_eq!(u.bytes_done, 500);
        assert_eq!(u.bytes_total, 1000);
        assert!((u.progress - 50.0).abs() < f32::EPSILON);
        assert_eq!(u.speed, "2.5 MB/s");
        assert!(u.message.is_none());
    }

    #[test]
    fn make_job_update_pct_handles_zero_total() {
        let u = make_job_update("j", "writing", 0, 0, 0);
        assert_eq!(u.progress, 0.0);
        assert_eq!(u.eta, "00:00");
        assert_eq!(u.speed, "0 B/s");
    }

    #[test]
    fn make_job_update_eta_uses_remaining_bytes_over_bps() {
        // 100 remaining at 10 B/s = 10s → "00:10"
        let u = make_job_update("j", "verifying", 0, 100, 10);
        assert_eq!(u.eta, "00:10");
    }

    #[test]
    fn parse_helper_line_returns_none_for_empty_or_blank() {
        assert!(parse_helper_line("job-1", "").is_none());
        assert!(parse_helper_line("job-1", "   ").is_none());
        assert!(parse_helper_line("job-1", "\n").is_none());
    }

    #[test]
    fn parse_helper_line_returns_none_for_non_json() {
        assert!(parse_helper_line("job-1", "not json").is_none());
    }

    #[test]
    fn parse_helper_line_returns_none_for_missing_kind() {
        assert!(parse_helper_line("job-1", r#"{"other":1}"#).is_none());
    }

    #[test]
    fn parse_helper_line_returns_none_for_unknown_kind() {
        assert!(parse_helper_line("job-1", r#"{"kind":"weird"}"#).is_none());
    }

    #[test]
    fn parse_helper_line_progress_carries_through_fields_and_formats() {
        let line = r#"{"kind":"progress","state":"writing","bytes_done":500,"bytes_total":1000,"bytes_per_sec":2500000}"#;
        let ev = parse_helper_line("job-7", line).unwrap();
        match ev {
            HelperEvent::Progress(u) => {
                assert_eq!(u.job_id, "job-7");
                assert_eq!(u.state, "writing");
                assert_eq!(u.bytes_done, 500);
                assert_eq!(u.bytes_total, 1000);
                assert!((u.progress - 50.0).abs() < f32::EPSILON);
                assert_eq!(u.speed, "2.5 MB/s");
                assert!(u.message.is_none());
            }
            _ => panic!("expected Progress"),
        }
    }

    #[test]
    fn parse_helper_line_progress_defaults_state_to_writing() {
        let line = r#"{"kind":"progress","bytes_done":1,"bytes_total":10}"#;
        let ev = parse_helper_line("j", line).unwrap();
        match ev {
            HelperEvent::Progress(u) => assert_eq!(u.state, "writing"),
            _ => panic!("expected Progress"),
        }
    }

    #[test]
    fn parse_helper_line_complete_populates_all_fields() {
        let line = r#"{
            "kind":"complete",
            "bytes_written":2048,
            "source_sha256":"src",
            "readback_sha256":"dev",
            "verify_match":true,
            "mismatches":[],
            "elapsed_ms":1234,
            "avg_write_bps":100,
            "avg_verify_bps":200
        }"#;
        let ev = parse_helper_line("job-c", line).unwrap();
        match ev {
            HelperEvent::Complete(c) => {
                assert_eq!(c.job_id, "job-c");
                assert_eq!(c.bytes_written, 2048);
                assert_eq!(c.source_sha256, "src");
                assert_eq!(c.readback_sha256, "dev");
                assert!(c.verify_match);
                assert_eq!(c.elapsed_ms, 1234);
                assert_eq!(c.avg_write_bps, 100);
                assert_eq!(c.avg_verify_bps, 200);
                assert!(c.mismatches.is_empty());
            }
            _ => panic!("expected Complete"),
        }
    }

    #[test]
    fn parse_helper_line_complete_parses_mismatches_when_present() {
        let line = r#"{
            "kind":"complete",
            "mismatches":[{"lba":"0x1","byte_offset":"+0x0","expected":"AA","actual":"BB"}]
        }"#;
        let ev = parse_helper_line("j", line).unwrap();
        match ev {
            HelperEvent::Complete(c) => {
                assert_eq!(c.mismatches.len(), 1);
                assert_eq!(c.mismatches[0].lba, "0x1");
            }
            _ => panic!("expected Complete"),
        }
    }

    #[test]
    fn parse_helper_line_complete_defaults_missing_fields() {
        let ev = parse_helper_line("j", r#"{"kind":"complete"}"#).unwrap();
        match ev {
            HelperEvent::Complete(c) => {
                assert_eq!(c.bytes_written, 0);
                assert_eq!(c.source_sha256, "");
                assert_eq!(c.readback_sha256, "");
                assert!(!c.verify_match);
                assert!(c.mismatches.is_empty());
            }
            _ => panic!("expected Complete"),
        }
    }

    #[test]
    fn parse_helper_line_error_uses_eunknown_when_code_missing() {
        let ev = parse_helper_line("j", r#"{"kind":"error"}"#).unwrap();
        match ev {
            HelperEvent::Failure(f) => {
                assert_eq!(f.error_code, "EUNKNOWN");
                assert_eq!(f.error_message, "");
            }
            _ => panic!("expected Failure"),
        }
    }

    #[test]
    fn parse_helper_line_error_passes_code_and_message_through() {
        let line = r#"{"kind":"error","error_code":"EIO","error_message":"disk gone"}"#;
        let ev = parse_helper_line("j", line).unwrap();
        match ev {
            HelperEvent::Failure(f) => {
                assert_eq!(f.error_code, "EIO");
                assert_eq!(f.error_message, "disk gone");
            }
            _ => panic!("expected Failure"),
        }
    }

    #[test]
    fn build_helper_command_quotes_each_arg() {
        let s = build_helper_command(
            "/Apps/DC.app/Contents/MacOS/dc",
            "/tmp/boot.iso",
            "/dev/disk5",
            "job-7",
            "/tmp/p.jsonl",
        );
        assert_eq!(
            s,
            "'/Apps/DC.app/Contents/MacOS/dc' --helper-burn \
             --image='/tmp/boot.iso' --target='/dev/disk5' \
             --job='job-7' --progress='/tmp/p.jsonl'"
                .replace("             ", "")
        );
    }

    #[test]
    fn build_helper_command_escapes_embedded_single_quotes_in_paths() {
        let s = build_helper_command(
            "/exe",
            "/tmp/it's a test.iso",
            "/dev/disk5",
            "job-1",
            "/tmp/p.jsonl",
        );
        assert!(s.contains("--image='/tmp/it'\\''s a test.iso'"), "got {s}");
    }

    #[test]
    fn build_osascript_script_wraps_command_with_administrator_privileges() {
        let s = build_osascript_script("ls /tmp", "Need access");
        assert!(s.contains("do shell script \"ls /tmp\""), "got {s}");
        assert!(s.contains("with prompt \"Need access\""));
        assert!(s.ends_with("with administrator privileges"));
    }

    #[test]
    fn build_osascript_script_escapes_double_quotes_in_helper_cmd() {
        let s = build_osascript_script("echo \"hi\"", "Prompt");
        // Inner quotes must be backslash-escaped so AppleScript sees a single string.
        assert!(s.contains("echo \\\"hi\\\""), "got {s}");
    }

    #[test]
    fn build_osascript_script_escapes_backslashes_before_quotes() {
        let s = build_osascript_script("path\\with\\back", "P");
        assert!(s.contains("path\\\\with\\\\back"), "got {s}");
    }

    #[test]
    fn sq_escapes_embedded_single_quotes() {
        assert_eq!(sq("plain"), "plain");
        assert_eq!(sq("it's a test"), "it'\\''s a test");
        assert_eq!(sq("'leading"), "'\\''leading");
        assert_eq!(sq(""), "");
    }

    #[test]
    fn is_privileged_does_not_panic() {
        let _ = is_privileged();
    }

    #[test]
    fn pick_device_io_returns_plain_for_file_paths() {
        let io = pick_device_io("/tmp/foo.img");
        assert_eq!(io.name(), "plain-file");
    }

    #[test]
    fn pick_device_io_returns_plain_for_dev_when_not_root() {
        if is_privileged() {
            return;
        }
        let io = pick_device_io("/dev/disk5");
        assert_eq!(io.name(), "plain-file");
    }

    #[test]
    fn app_info_reports_environment() {
        let info = app_info();
        assert_eq!(info.version, env!("CARGO_PKG_VERSION"));
        assert!(!info.os.is_empty());
        assert!(!info.arch.is_empty());
    }

    #[test]
    fn verify_image_command_is_a_noop_ok() {
        assert!(verify_image("any".into()).is_ok());
    }

    #[test]
    fn list_disks_returns_without_panic() {
        let _ = list_disks();
    }

    #[test]
    fn inspect_image_errors_on_unknown_extension() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("file.unknownext");
        std::fs::write(&p, b"hello").unwrap();
        let result = inspect_image(p.to_string_lossy().into_owned());
        assert!(result.is_err());
    }

    #[test]
    fn inspect_image_reports_iso_details() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("boot.iso");
        std::fs::write(&p, vec![0u8; 4096]).unwrap();
        let info = inspect_image(p.to_string_lossy().into_owned()).unwrap();
        assert_eq!(info.name, "boot.iso");
        assert_eq!(info.format, "ISO 9660 / RAW");
        assert_eq!(info.source_bytes, 4096);
        assert_eq!(info.uncompressed_bytes, 4096);
        assert_eq!(info.sectors, 4096 / 512);
        assert!(info.sha256.is_none());
    }

    #[test]
    fn derive_partitions_renders_name_and_filesystem() {
        let mut d = plist::Dictionary::new();
        d.insert("VolumeName".into(), plist::Value::String("Boot".into()));
        d.insert("FilesystemType".into(), plist::Value::String("apfs".into()));
        assert_eq!(derive_partitions(&d), "BOOT · APFS");
    }

    #[test]
    fn derive_partitions_falls_back_to_partition_count() {
        let mut d = plist::Dictionary::new();
        d.insert("FilesystemType".into(), plist::Value::String("ntfs".into()));
        assert_eq!(derive_partitions(&d), "1 PARTITION · NTFS");
    }

    #[test]
    fn derive_partitions_uses_name_only_when_no_filesystem() {
        let mut d = plist::Dictionary::new();
        d.insert("VolumeName".into(), plist::Value::String("Recovery".into()));
        assert_eq!(derive_partitions(&d), "RECOVERY");
    }

    #[test]
    fn derive_partitions_reports_unformatted_when_empty() {
        let d = plist::Dictionary::new();
        assert_eq!(derive_partitions(&d), "UNFORMATTED");
    }

    #[test]
    fn derive_partitions_falls_back_to_content_when_filesystem_absent() {
        let mut d = plist::Dictionary::new();
        d.insert("VolumeName".into(), plist::Value::String("Data".into()));
        d.insert("Content".into(), plist::Value::String("ExFAT".into()));
        assert_eq!(derive_partitions(&d), "DATA · EXFAT");
    }

    fn disks_plist(disk_ids: &[&str]) -> Vec<u8> {
        let mut s = String::from(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
             <plist version=\"1.0\">\n<dict>\n  <key>AllDisks</key>\n  <array>\n"
        );
        for id in disk_ids {
            s.push_str(&format!("    <string>{id}</string>\n"));
        }
        s.push_str("  </array>\n</dict>\n</plist>\n");
        s.into_bytes()
    }

    #[test]
    fn parse_disks_plist_extracts_whole_disk_ids() {
        let xml = disks_plist(&["disk0", "disk0s1", "disk1", "disk2s3"]);
        let ids = parse_disks_plist(&xml).unwrap();
        assert_eq!(ids, vec!["disk0", "disk1"]);
    }

    #[test]
    fn parse_disks_plist_returns_empty_when_no_disks() {
        let xml = disks_plist(&[]);
        let ids = parse_disks_plist(&xml).unwrap();
        assert!(ids.is_empty());
    }

    #[test]
    fn parse_disks_plist_returns_none_for_invalid_bytes() {
        assert!(parse_disks_plist(b"not a plist").is_none());
    }

    #[test]
    fn parse_disks_plist_returns_none_when_alldisks_missing() {
        let xml = b"<?xml version=\"1.0\"?>\n<plist version=\"1.0\"><dict><key>Other</key><string>x</string></dict></plist>";
        assert!(parse_disks_plist(xml).is_none());
    }

    fn info_plist(entries: &[(&str, &str, &str)]) -> Vec<u8> {
        // entries: (key, kind, value) where kind is "string" | "integer" | "true" | "false"
        let mut s = String::from(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
             <plist version=\"1.0\">\n<dict>\n"
        );
        for (k, kind, v) in entries {
            s.push_str(&format!("  <key>{k}</key>\n"));
            match *kind {
                "string" => s.push_str(&format!("  <string>{v}</string>\n")),
                "integer" => s.push_str(&format!("  <integer>{v}</integer>\n")),
                "true" => s.push_str("  <true/>\n"),
                "false" => s.push_str("  <false/>\n"),
                _ => unreachable!(),
            }
        }
        s.push_str("</dict>\n</plist>\n");
        s.into_bytes()
    }

    #[test]
    fn parse_disk_info_plist_populates_all_fields() {
        let xml = info_plist(&[
            ("MediaName", "string", "SanDisk Ultra"),
            ("TotalSize", "integer", "16000000000"),
            ("BusProtocol", "string", "USB"),
            ("Removable", "true", ""),
            ("VolumeName", "string", "MyDrive"),
            ("FilesystemType", "string", "exfat"),
        ]);
        let d = parse_disk_info_plist(&xml, "/dev/disk5".into()).unwrap();
        assert_eq!(d.device, "/dev/disk5");
        assert_eq!(d.model, "SANDISK ULTRA");
        assert_eq!(d.bytes, 16_000_000_000);
        assert_eq!(d.capacity, "16.00 GB");
        assert_eq!(d.bus, "USB");
        assert_eq!(d.partitions, "MYDRIVE · EXFAT");
        assert!(d.flags.contains(&"REMOVABLE".to_string()));
        assert!(!d.flags.contains(&"INTERNAL".to_string()));
    }

    #[test]
    fn parse_disk_info_plist_falls_back_to_io_registry_name() {
        let xml = info_plist(&[
            ("IORegistryEntryName", "string", "AppleAPFSMedia"),
            ("TotalSize", "integer", "0"),
        ]);
        let d = parse_disk_info_plist(&xml, "/dev/disk0".into()).unwrap();
        assert_eq!(d.model, "APPLEAPFSMEDIA");
    }

    #[test]
    fn parse_disk_info_plist_defaults_unknown_when_no_name_or_bus() {
        let xml = info_plist(&[]);
        let d = parse_disk_info_plist(&xml, "/dev/diskX".into()).unwrap();
        assert_eq!(d.model, "UNKNOWN");
        assert_eq!(d.bus, "UNKNOWN");
        assert_eq!(d.bytes, 0);
        assert_eq!(d.capacity, "—");
        assert_eq!(d.partitions, "UNFORMATTED");
        assert!(d.flags.is_empty());
    }

    #[test]
    fn parse_disk_info_plist_marks_internal_when_internal_flag_set() {
        let xml = info_plist(&[
            ("Internal", "true", ""),
        ]);
        let d = parse_disk_info_plist(&xml, "/dev/disk1".into()).unwrap();
        assert!(d.flags.contains(&"INTERNAL".to_string()));
    }

    #[test]
    fn parse_disk_info_plist_treats_any_removable_alias_as_removable() {
        for key in ["Removable", "RemovableMedia", "RemovableMediaOrExternalDevice"] {
            let xml = info_plist(&[(key, "true", "")]);
            let d = parse_disk_info_plist(&xml, "/dev/diskZ".into()).unwrap();
            assert!(
                d.flags.contains(&"REMOVABLE".to_string()),
                "key {key} should yield REMOVABLE",
            );
        }
    }

    #[test]
    fn parse_disk_info_plist_returns_none_for_invalid_bytes() {
        assert!(parse_disk_info_plist(b"garbage", "/dev/x".into()).is_none());
    }
}
