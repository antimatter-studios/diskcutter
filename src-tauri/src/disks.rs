use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::db::{self, Db};
use crate::pipeline::{self, BurnError, VerifyMismatch};
use crate::source;
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
    let info = source::probe(p).ok_or_else(|| format!("unsupported image format: {path}"))?;
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

/// Tracks burns currently in flight so a window-close request can ask "is
/// anything writing right now?" without round-tripping through the DB. Both
/// the in-process and the elevated-helper paths register here on entry to
/// `start_write` and deregister on terminal event.
#[derive(Default)]
pub struct ActiveBurns(pub Mutex<HashMap<String, ActiveBurn>>);

#[derive(Clone, Debug)]
pub struct ActiveBurn {
    pub job_id: String,
    pub target: String,
    #[allow(dead_code)]
    pub kind: BurnKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BurnKind {
    /// Same-process: cancel via the `CancelRegistry` atomic flag.
    InProcess,
    /// Out-of-process root helper: cancel via filesystem marker the helper
    /// polls. The helper itself runs as root and cannot be signalled by us.
    Elevated,
}

impl ActiveBurns {
    pub fn insert(&self, burn: ActiveBurn) {
        if let Ok(mut g) = self.0.lock() {
            g.insert(burn.job_id.clone(), burn);
        }
    }
    pub fn remove(&self, job_id: &str) {
        if let Ok(mut g) = self.0.lock() {
            g.remove(job_id);
        }
    }
    pub fn snapshot(&self) -> Vec<ActiveBurn> {
        self.0
            .lock()
            .map(|g| g.values().cloned().collect())
            .unwrap_or_default()
    }
    pub fn is_empty(&self) -> bool {
        self.0.lock().map(|g| g.is_empty()).unwrap_or(true)
    }
}

/// Tracks elevated-burn jobs currently in flight. Used to reject duplicate
/// `start_write` calls for the same job_id — otherwise every Retry click
/// spawns a fresh osascript + helper, racing for the same /dev/diskN,
/// stacking password prompts, and clobbering the progress JSONL file.
#[derive(Default)]
pub struct ElevatedJobs(pub Mutex<HashMap<String, u32>>);

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

/// Find disk-cutter helper subprocesses still running from a previous session.
/// These can hold devices open (esp. /dev/diskN) and block new burns. Returns
/// the PIDs so the frontend can offer to clean them up.
#[tauri::command]
pub fn find_orphan_helpers() -> Vec<u32> {
    let me = std::process::id();
    let out = match std::process::Command::new("ps")
        .args(["-A", "-o", "pid=,user=,command="])
        .output()
    {
        Ok(o) => o,
        Err(_) => return Vec::new(),
    };
    let stdout = String::from_utf8_lossy(&out.stdout);
    let mut found = Vec::new();
    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if !line.contains("disk-cutter") || !line.contains("--helper-burn") {
            continue;
        }
        let mut parts = line.split_whitespace();
        let pid: u32 = match parts.next().and_then(|p| p.parse().ok()) {
            Some(p) => p,
            None => continue,
        };
        if pid == me {
            continue;
        }
        let user = match parts.next() {
            Some(u) => u,
            None => continue,
        };
        if user != "root" {
            continue;
        }
        found.push(pid);
    }
    found
}

/// Kill orphan helper PIDs via osascript admin (they're root-owned).
#[tauri::command]
pub fn kill_orphan_helpers(pids: Vec<u32>) -> Result<(), String> {
    if pids.is_empty() {
        return Ok(());
    }
    #[cfg(target_os = "macos")]
    {
        let args = pids
            .iter()
            .map(|p| p.to_string())
            .collect::<Vec<_>>()
            .join(" ");
        let script = format!(
            "do shell script \"/bin/kill -9 {}\" with prompt \"Disk Cutter needs administrator access to clean up an orphaned helper process from a previous session.\" with administrator privileges",
            args
        );
        std::process::Command::new("osascript")
            .args(["-e", &script])
            .output()
            .map_err(|e| e.to_string())?;
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = pids;
        Err("not implemented on this platform".to_string())
    }
}

/// Heuristic FDA probe: stat a TCC-protected file. macOS only grants `stat`
/// on TCC.db to processes with Full Disk Access. Mirrors the doctor check
/// but lives here so we can run it on the burn hot-path without pulling in
/// the doctor module. Returns `true` when FDA appears to be granted.
pub fn fda_granted() -> bool {
    #[cfg(target_os = "macos")]
    {
        std::path::Path::new("/Library/Application Support/com.apple.TCC/TCC.db")
            .metadata()
            .is_ok()
    }
    #[cfg(not(target_os = "macos"))]
    {
        true
    }
}

#[tauri::command]
pub fn check_fda() -> bool {
    fda_granted()
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
    #[cfg(target_os = "linux")]
    {
        enumerate_linux().unwrap_or_default()
    }
    #[cfg(target_os = "windows")]
    {
        enumerate_windows().unwrap_or_default()
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        Vec::new()
    }
}

#[cfg(target_os = "macos")]
fn enumerate_macos() -> Option<Vec<Disk>> {
    use std::process::Command;

    let list = Command::new("diskutil")
        .args(["list", "-plist"])
        .output()
        .ok()?;
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

#[cfg(any(target_os = "macos", test))]
fn is_whole_disk(id: &str) -> bool {
    id.strip_prefix("disk")
        .map(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
        .unwrap_or(false)
}

#[cfg(any(target_os = "macos", test))]
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

#[cfg(any(target_os = "macos", test))]
fn parse_disk_info_plist(bytes: &[u8], device_path: String) -> Option<Disk> {
    let val: plist::Value = plist::from_bytes(bytes).ok()?;
    let dict = val.as_dictionary()?;

    let s = |k: &str| {
        dict.get(k)
            .and_then(|v| v.as_string())
            .map(|s| s.to_string())
    };
    let u = |k: &str| {
        dict.get(k)
            .and_then(|v| v.as_unsigned_integer())
            .unwrap_or(0)
    };
    let b = |k: &str| dict.get(k).and_then(|v| v.as_boolean()).unwrap_or(false);

    let model = s("MediaName")
        .or_else(|| s("IORegistryEntryName"))
        .unwrap_or_else(|| "UNKNOWN".to_string());
    let bytes_total = u("TotalSize");
    let bus = s("BusProtocol")
        .unwrap_or_else(|| "UNKNOWN".to_string())
        .to_uppercase();
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

#[cfg(target_os = "linux")]
fn enumerate_linux() -> Option<Vec<Disk>> {
    let entries = std::fs::read_dir("/sys/block").ok()?;
    let mut out = Vec::new();
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if !is_linux_block_disk(&name) {
            continue;
        }
        let dir = e.path();
        if let Some(d) = build_linux_disk(&name, &dir) {
            out.push(d);
        }
    }
    Some(out)
}

/// Reject virtual / non-disk block devices that show up in /sys/block but
/// we never want to burn to: loopbacks, ramdisks, device-mapper, CD-ROMs,
/// floppies, MD/zram. Keep ATA/USB/NVMe/MMC/virtio.
#[cfg(any(target_os = "linux", test))]
fn is_linux_block_disk(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    const SKIP: &[&str] = &["loop", "ram", "dm-", "sr", "fd", "md", "zram"];
    !SKIP.iter().any(|p| name.starts_with(p))
}

#[cfg(target_os = "linux")]
fn build_linux_disk(name: &str, sys_dir: &Path) -> Option<Disk> {
    let read = |rel: &str| -> Option<String> {
        std::fs::read_to_string(sys_dir.join(rel))
            .ok()
            .map(|s| s.trim().to_string())
    };
    let size_sectors: u64 = read("size").and_then(|s| s.parse().ok()).unwrap_or(0);
    let bytes = size_sectors.saturating_mul(512);
    let removable = read("removable").as_deref() == Some("1");
    let vendor = read("device/vendor").unwrap_or_default();
    let model = read("device/model").unwrap_or_default();
    let combined = format!("{vendor} {model}").trim().to_string();
    let model_str = if combined.is_empty() {
        "UNKNOWN".to_string()
    } else {
        combined
    };
    let bus = read_linux_bus(sys_dir).unwrap_or_else(|| "UNKNOWN".to_string());
    let partitions = list_linux_partitions(sys_dir);
    let mut flags = Vec::new();
    if removable {
        flags.push("REMOVABLE".to_string());
    } else {
        flags.push("INTERNAL".to_string());
    }
    Some(Disk {
        device: format!("/dev/{name}"),
        model: model_str.to_uppercase(),
        capacity: format_capacity(bytes),
        bytes,
        bus: bus.to_uppercase(),
        partitions,
        flags,
    })
}

/// Walk the canonical `device/` symlink to figure out the host bus —
/// `/sys/block/sda/device` resolves through `…/ata1/host0/…` for SATA,
/// `…/usb1/1-1/…` for USB, `…/nvme0/…` for NVMe, etc. Matching on the
/// path segments is more reliable than parsing uevent files because
/// uevent shapes vary across kernel versions.
#[cfg(target_os = "linux")]
fn read_linux_bus(sys_dir: &Path) -> Option<String> {
    let target = std::fs::canonicalize(sys_dir.join("device")).ok()?;
    let s = target.to_string_lossy();
    if s.contains("/usb") {
        Some("USB".to_string())
    } else if s.contains("/nvme") {
        Some("NVME".to_string())
    } else if s.contains("/mmc") {
        Some("MMC".to_string())
    } else if s.contains("/virtio") {
        Some("VIRTIO".to_string())
    } else if s.contains("/ata") || s.contains("/sata") {
        Some("SATA".to_string())
    } else if s.contains("/scsi") {
        Some("SCSI".to_string())
    } else {
        None
    }
}

#[cfg(target_os = "linux")]
fn list_linux_partitions(sys_dir: &Path) -> String {
    let Ok(entries) = std::fs::read_dir(sys_dir) else {
        return "UNFORMATTED".to_string();
    };
    let mut parts: Vec<String> = entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| {
            // Partition subdir name starts with the parent name; e.g.
            // `/sys/block/sda/sda1`. Reject `holders`, `slaves`, etc.
            let parent = sys_dir
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or_default();
            n.starts_with(parent) && n.len() > parent.len()
        })
        .collect();
    parts.sort();
    if parts.is_empty() {
        "UNFORMATTED".to_string()
    } else if parts.len() == 1 {
        "1 PARTITION".to_string()
    } else {
        format!("{} PARTITIONS", parts.len())
    }
}

#[cfg(target_os = "windows")]
fn enumerate_windows() -> Option<Vec<Disk>> {
    use std::process::Command;
    let out = Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            "Get-CimInstance Win32_DiskDrive | Select-Object DeviceID,Model,Size,InterfaceType,MediaType | ConvertTo-Json -Depth 2 -Compress",
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_windows_diskdrive_json(&out.stdout)
}

#[cfg(any(target_os = "windows", test))]
fn parse_windows_diskdrive_json(bytes: &[u8]) -> Option<Vec<Disk>> {
    let text = std::str::from_utf8(bytes).ok()?.trim();
    if text.is_empty() {
        return Some(Vec::new());
    }
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    // ConvertTo-Json emits a bare object when there's one result, an array
    // otherwise. Normalize.
    let array = match value {
        serde_json::Value::Array(a) => a,
        single => vec![single],
    };
    let mut out = Vec::new();
    for entry in array {
        if let Some(d) = windows_disk_from_json(&entry) {
            out.push(d);
        }
    }
    Some(out)
}

#[cfg(any(target_os = "windows", test))]
fn windows_disk_from_json(v: &serde_json::Value) -> Option<Disk> {
    let device = v.get("DeviceID").and_then(|x| x.as_str())?.to_string();
    let model = v
        .get("Model")
        .and_then(|x| x.as_str())
        .unwrap_or("UNKNOWN")
        .trim()
        .to_string();
    let bytes = v
        .get("Size")
        .and_then(|x| {
            x.as_u64()
                .or_else(|| x.as_str().and_then(|s| s.parse().ok()))
        })
        .unwrap_or(0);
    let bus = v
        .get("InterfaceType")
        .and_then(|x| x.as_str())
        .unwrap_or("UNKNOWN")
        .to_string();
    let media_type = v
        .get("MediaType")
        .and_then(|x| x.as_str())
        .unwrap_or_default();
    let mut flags = Vec::new();
    let removable = media_type.to_ascii_lowercase().contains("removable");
    if removable {
        flags.push("REMOVABLE".to_string());
    } else {
        flags.push("INTERNAL".to_string());
    }
    Some(Disk {
        device,
        model: model.to_uppercase(),
        capacity: format_capacity(bytes),
        bytes,
        bus: bus.to_uppercase(),
        partitions: "UNKNOWN".to_string(),
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

#[cfg(any(target_os = "macos", test))]
fn derive_partitions(dict: &plist::Dictionary) -> String {
    let fs = dict
        .get("FilesystemType")
        .and_then(|v| v.as_string())
        .or_else(|| dict.get("Content").and_then(|v| v.as_string()))
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
    active: State<'_, ActiveBurns>,
    elevated: State<'_, ElevatedJobs>,
    job_id: String,
    image_path: String,
    target_device: String,
) -> Result<(), String> {
    let p = Path::new(&image_path);
    let info = source::probe(p).ok_or_else(|| format!("unsupported image format: {image_path}"))?;
    let image_name = p
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();

    {
        // Idempotent w.r.t. enqueue_burn the frontend may have already
        // called; this only inserts if no open row exists. Lifecycle
        // flips to 'running' separately:
        //   - in-process path: at the top of the burn thread below
        //   - elevated path:   after osascript spawn, with helper_pid
        //                      and progress_file populated
        let db = app.state::<Db>();
        db::record_burn_queued(
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
        // Parent-side FDA preflight: stat'ing TCC.db requires Full Disk
        // Access, so a stat failure is a strong signal the helper would
        // immediately bail with ENEEDS_FDA. Surface that without paying the
        // osascript+password round-trip.
        if !fda_granted() {
            let msg = "Full Disk Access not granted to Disk Cutter".to_string();
            db::record_burn_failed(&app.state::<Db>(), &job_id, "ENEEDS_FDA", &msg);
            let _ = app.emit(
                "disk-cutter://job-error",
                JobFailure {
                    job_id: job_id.clone(),
                    error_code: "ENEEDS_FDA".into(),
                    error_message: msg,
                },
            );
            return Ok(());
        }
        active.insert(ActiveBurn {
            job_id: job_id.clone(),
            target: target_device.clone(),
            kind: BurnKind::Elevated,
        });
        return spawn_elevated_burn(app, &elevated, job_id, image_path, target_device);
    }

    let flag = Arc::new(AtomicBool::new(false));
    cancel
        .0
        .lock()
        .unwrap()
        .insert(job_id.clone(), flag.clone());
    active.insert(ActiveBurn {
        job_id: job_id.clone(),
        target: target_device.clone(),
        kind: BurnKind::InProcess,
    });

    let app = app.clone();
    let id = job_id;
    let image = image_path;
    let target = target_device;

    std::thread::spawn(move || {
        // Flip queued → running at the moment the burn thread starts.
        // No helper_pid/progress_file because this is in-process.
        db::record_burn_started(&app.state::<Db>(), &id, None, None);
        let outcome = run_job(&app, &id, &image, &target, &flag);
        let _ = std::fs::remove_file(cancel_sentinel_path(&id));
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
                app.state::<ActiveBurns>().remove(&complete.job_id);
                let _ = app.emit("disk-cutter://job-complete", complete);
            }
            Err(failure) => {
                db::record_burn_failed(
                    &db,
                    &failure.job_id,
                    &failure.error_code,
                    &failure.error_message,
                );
                app.state::<ActiveBurns>().remove(&failure.job_id);
                let _ = app.emit("disk-cutter://job-error", failure);
            }
        }
    });

    Ok(())
}

fn spawn_elevated_burn(
    app: AppHandle,
    elevated: &State<'_, ElevatedJobs>,
    job_id: String,
    image_path: String,
    target: String,
) -> Result<(), String> {
    // Idempotency: if an elevated job for this id is already in flight, do
    // not spawn another osascript/helper. Each duplicate spawn would prompt
    // for the password again, race over the device, and clobber the progress
    // JSONL — the multi-prompt storm we are fixing. Check-and-reserve in one
    // critical section so two near-simultaneous Retry clicks can't both win.
    {
        let mut guard = elevated.0.lock().unwrap();
        if guard.contains_key(&job_id) {
            return Err("burn already in flight for this job".to_string());
        }
        // Reserve with sentinel pid 0; real pid is filled in after spawn.
        guard.insert(job_id.clone(), 0);
    }
    let result = spawn_elevated_burn_inner(app.clone(), &job_id, image_path, target);
    if result.is_err() {
        // Spawn never made it to tail_helper, which is what would normally
        // remove the registry entry. Clean up here so a future retry can
        // proceed.
        if let Some(reg) = app.try_state::<ElevatedJobs>() {
            reg.0.lock().unwrap().remove(&job_id);
        }
    }
    result
}

fn spawn_elevated_burn_inner(
    app: AppHandle,
    job_id: &str,
    image_path: String,
    target: String,
) -> Result<(), String> {
    let job_id = job_id.to_string();
    let exe = std::env::current_exe().map_err(|e| {
        let msg = e.to_string();
        db::record_burn_failed(&app.state::<Db>(), &job_id, "EHELPER", &msg);
        app.state::<ActiveBurns>().remove(&job_id);
        msg
    })?;
    let progress_path = format!("/tmp/disk-cutter-progress-{job_id}.jsonl");
    // Do NOT unlink the progress file here. If a prior helper for this id is
    // still alive (e.g. retry-storm survivor) its open fd points at the old
    // inode; removing the path orphans its writes to a deleted inode while
    // the new tail_helper reads a fresh empty file — UI freezes. Lifecycle
    // ownership now sits in tail_helper, which deletes on its own exit.
    // Clear any stale cancel sentinel from a previous job with the same id.
    let _ = std::fs::remove_file(cancel_sentinel_path(&job_id));

    // Look up the configured writer impl + perf tunables, if any. The DB may
    // not have been initialised (see lib.rs "continuing without persistence");
    // fall through silently in that case and let the helper apply its own
    // defaults.
    let read_config = |key: &str| -> Option<String> {
        app.try_state::<Db>().and_then(|db| {
            let conn = db.0.lock().ok()?;
            conn.query_row("SELECT value FROM config WHERE key = ?1", [key], |r| {
                r.get::<_, String>(0)
            })
            .ok()
        })
    };
    let writer_impl = read_config("writer.impl");
    // Validate numerics by round-tripping through parse(); drop anything we
    // can't interpret so the helper applies its built-in default.
    let chunk_bytes = read_config("chunk.bytes").and_then(|v| v.parse::<u64>().ok());
    let workers_count = read_config("workers.count").and_then(|v| v.parse::<usize>().ok());
    let queue_depth = read_config("queue.depth").and_then(|v| v.parse::<usize>().ok());
    // Bool: only the literal "true" enables skip; everything else (incl.
    // missing) keeps verify on.
    let skip_verify = read_config("verify.skip")
        .map(|v| v == "true")
        .unwrap_or(false);
    let debug_logging = read_config("debug.logging")
        .map(|v| v == "true")
        .unwrap_or(false);

    let helper_cmd = build_helper_command(
        &exe.to_string_lossy(),
        &image_path,
        &target,
        &job_id,
        &progress_path,
        writer_impl.as_deref(),
        chunk_bytes,
        workers_count,
        queue_depth,
        skip_verify,
        debug_logging,
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
            app.state::<ActiveBurns>().remove(&job_id);
            msg
        })?;

    // Record osascript pid into the in-flight registry so cancel/duplicate
    // checks have a real pid to work with. tail_helper removes the entry on
    // exit.
    if let Some(reg) = app.try_state::<ElevatedJobs>() {
        reg.0.lock().unwrap().insert(job_id.clone(), child.id());
    }
    // Flip queued → running in the DB and stamp the reattach
    // breadcrumbs (helper_pid + progress_file). If the parent app
    // dies after this point, on next startup the reattach scan can
    // find this row, confirm the pid is still alive, and re-spawn
    // a tail_helper against the same progress file.
    db::record_burn_started(
        &app.state::<Db>(),
        &job_id,
        Some(&progress_path),
        Some(child.id()),
    );

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

#[allow(clippy::too_many_arguments)]
fn build_helper_command(
    exe: &str,
    image: &str,
    target: &str,
    job_id: &str,
    progress: &str,
    writer_impl: Option<&str>,
    chunk_bytes: Option<u64>,
    workers: Option<usize>,
    queue_depth: Option<usize>,
    skip_verify: bool,
    debug_logging: bool,
) -> String {
    let mut cmd = format!(
        "'{}' --helper-burn --image='{}' --target='{}' --job='{}' --progress='{}'",
        sq(exe),
        sq(image),
        sq(target),
        sq(job_id),
        sq(progress),
    );
    if let Some(w) = writer_impl {
        cmd.push_str(&format!(" --writer='{}'", sq(w)));
    }
    if let Some(n) = chunk_bytes {
        cmd.push_str(&format!(" --chunk-bytes={n}"));
    }
    if let Some(n) = workers {
        cmd.push_str(&format!(" --workers={n}"));
    }
    if let Some(n) = queue_depth {
        cmd.push_str(&format!(" --queue-depth={n}"));
    }
    if skip_verify {
        cmd.push_str(" --skip-verify=true");
    }
    if debug_logging {
        cmd.push_str(" --debug=true");
    }
    cmd
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
                    app.state::<ActiveBurns>().remove(&job_id);
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
    let _ = std::fs::remove_file(cancel_sentinel_path(&job_id));
    // Release the in-flight slot. After this point a Retry for the same
    // job_id is allowed to spawn a fresh osascript+helper again.
    if let Some(reg) = app.try_state::<ElevatedJobs>() {
        reg.0.lock().unwrap().remove(&job_id);
    }
}

enum HelperEvent {
    Progress(JobUpdate),
    Complete(JobComplete),
    Failure(JobFailure),
    Log { level: String, message: String },
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
            let bps = val
                .get("bytes_per_sec")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let state = val
                .get("state")
                .and_then(|v| v.as_str())
                .unwrap_or("writing");
            Some(HelperEvent::Progress(make_job_update(
                job_id,
                state,
                bytes_done,
                bytes_total,
                bps,
            )))
        }
        "complete" => {
            let mismatches: Vec<crate::pipeline::VerifyMismatch> = serde_json::from_value(
                val.get("mismatches")
                    .cloned()
                    .unwrap_or(serde_json::Value::Array(vec![])),
            )
            .unwrap_or_default();
            Some(HelperEvent::Complete(JobComplete {
                job_id: job_id.to_string(),
                bytes_written: val
                    .get("bytes_written")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0),
                source_sha256: val
                    .get("source_sha256")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                readback_sha256: val
                    .get("readback_sha256")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                verify_match: val
                    .get("verify_match")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
                mismatches,
                elapsed_ms: val.get("elapsed_ms").and_then(|v| v.as_u64()).unwrap_or(0),
                avg_write_bps: val
                    .get("avg_write_bps")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0),
                avg_verify_bps: val
                    .get("avg_verify_bps")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0),
            }))
        }
        "error" => Some(HelperEvent::Failure(JobFailure {
            job_id: job_id.to_string(),
            error_code: val
                .get("error_code")
                .and_then(|v| v.as_str())
                .unwrap_or("EUNKNOWN")
                .to_string(),
            error_message: val
                .get("error_message")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        })),
        "log" => {
            let level = val
                .get("level")
                .and_then(|v| v.as_str())
                .unwrap_or("info")
                .to_string();
            let message = val
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Some(HelperEvent::Log { level, message })
        }
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
            app.state::<ActiveBurns>().remove(&complete.job_id);
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
            app.state::<ActiveBurns>().remove(&failure.job_id);
            let _ = app.emit("disk-cutter://job-error", failure);
            Some("error")
        }
        HelperEvent::Log { level, message } => {
            db::append_log(&app.state::<Db>(), job_id, &level, &message);
            Some("log")
        }
    }
}

/// Cross-process cancel sentinel path. The elevated helper subprocess runs as
/// root via osascript (unix) or an elevated shim (windows) and can't be
/// reached through the parent's in-memory `CancelRegistry`; it polls this
/// file instead. Lives under `std::env::temp_dir()` so it resolves to
/// `/tmp/` on unix and `%TEMP%` on Windows — `/tmp` does not exist on
/// Windows runners (or default Windows installs), so the prior hard-coded
/// path produced silent write failures and a broken cancel channel there.
pub fn cancel_sentinel_path(job_id: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("disk-cutter-cancel-{job_id}.flag"))
}

#[tauri::command]
pub fn cancel_write(cancel: State<'_, CancelRegistry>, job_id: String) -> Result<(), String> {
    if let Some(flag) = cancel.0.lock().unwrap().get(&job_id) {
        flag.store(true, Ordering::Relaxed);
    }
    // Elevated jobs don't appear in the registry — write a sentinel the helper
    // polls. Harmless for in-process jobs (no helper looks for it; tail_helper
    // or run_job cleanup removes any stragglers).
    let _ = std::fs::write(cancel_sentinel_path(&job_id), b"1");
    Ok(())
}

/// Cancel every active burn at once. Used by the window-close handler so a
/// quit request gracefully unwinds every in-flight write before the process
/// exits. Returns the count signalled (snapshot at time of call) so the
/// caller can pick a sensible grace window.
pub fn cancel_all_burns(active: &ActiveBurns, cancel: &CancelRegistry) -> usize {
    let snap = active.snapshot();
    let count = snap.len();
    if let Ok(reg) = cancel.0.lock() {
        for burn in &snap {
            if let Some(flag) = reg.get(&burn.job_id) {
                flag.store(true, Ordering::Relaxed);
            }
        }
    }
    for burn in &snap {
        let _ = std::fs::write(cancel_sentinel_path(&burn.job_id), b"1");
    }
    count
}

/// Polls `ActiveBurns` until empty or `timeout` elapses. Returns true if all
/// burns cleared cleanly, false if the timeout fired with some still active.
pub fn wait_for_burns_to_clear(active: &ActiveBurns, timeout: std::time::Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if active.is_empty() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    active.is_empty()
}

#[tauri::command]
pub fn has_active_burns(active: State<'_, ActiveBurns>) -> bool {
    !active.is_empty()
}

/// Cancel every active burn, wait briefly for cooperative shutdown, then
/// quit the app. Returns immediately; the frontend listens for
/// `disk-cutter://shutdown-progress` to track the wait. If helpers don't
/// exit within the grace window, the orphan-sweep at next launch picks up
/// any survivors (see `find_orphan_helpers`/`kill_orphan_helpers`).
#[tauri::command]
pub fn abort_and_quit(app: AppHandle) -> Result<(), String> {
    let app2 = app.clone();
    std::thread::spawn(move || {
        let active = app2.state::<ActiveBurns>();
        let cancel = app2.state::<CancelRegistry>();
        let count = cancel_all_burns(&active, &cancel);
        let _ = app2.emit(
            "disk-cutter://shutdown-progress",
            serde_json::json!({ "phase": "cancelling", "count": count }),
        );
        let cleared = wait_for_burns_to_clear(&active, std::time::Duration::from_secs(8));
        let _ = app2.emit(
            "disk-cutter://shutdown-progress",
            serde_json::json!({ "phase": "exiting", "cleared": cleared }),
        );
        app2.exit(0);
    });
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

    // Snapshot the debug toggle at burn start so toggling mid-run doesn't
    // half-apply.
    let job_log = crate::joblog::db_logger_for(app, job_id);

    let (mut reader, info) = source::open_streaming_with_log(image, &job_log)
        .map_err(|e| fail(job_id, "EUNSUPPORTED", &format!("open image: {e}")))?;
    let total_bytes = info.uncompressed_bytes;

    let device_io: Box<dyn DeviceIo> = pick_device_io(target_device);
    let writer = device_io
        .open_write(target)
        .map_err(|e| fail(job_id, "ETARGET", &format!("open target: {e}")))?;

    let burn_id = job_id.to_string();
    let burn_app = app.clone();
    let burn = pipeline::burn(
        &mut *reader,
        total_bytes,
        writer,
        pipeline::DEFAULT_CHUNK,
        cancel,
        |p| {
            let _ = burn_app.emit(
                "disk-cutter://job-update",
                make_job_update(
                    &burn_id,
                    "writing",
                    p.bytes_done,
                    p.bytes_total,
                    p.bytes_per_sec,
                ),
            );
        },
    )
    .map_err(|e| fail_for_burn_error(job_id, &e))?;

    let (mut reader2, _) = source::open_streaming_with_log(image, &job_log)
        .map_err(|e| fail(job_id, "EIMAGE", &format!("reopen image: {e}")))?;
    let mut device_reader = device_io
        .open_read(target)
        .map_err(|e| fail(job_id, "ETARGET", &format!("reopen target: {e}")))?;

    let verify_id = job_id.to_string();
    let verify_app = app.clone();
    let verify = pipeline::verify(
        &mut *reader2,
        total_bytes,
        &mut *device_reader,
        pipeline::DEFAULT_CHUNK,
        cancel,
        |p| {
            let _ = verify_app.emit(
                "disk-cutter://job-update",
                make_job_update(
                    &verify_id,
                    "verifying",
                    p.bytes_done,
                    p.bytes_total,
                    p.bytes_per_sec,
                ),
            );
        },
    )
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

// `kill -0 <pid>` probes for process existence without sending a real
// signal — works for root-owned osascript pids because the parent app
// doesn't need send-signal rights, only the existence check. The
// alternative (libc::kill via the nix crate) would pull in another
// dependency for one syscall.
fn pid_alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Reattach the UI to helper processes that outlived the parent app.
/// Called once at startup after the DB is open. Walks every
/// non-terminal burn_jobs row; for the running ones, checks the
/// recorded helper_pid + progress_file are both still alive and re-
/// spawns a tail thread against the existing JSONL. Rows whose helper
/// is gone get marked EORPHAN so the UI doesn't show them as eternally
/// running.
pub fn reattach_running_helpers(app: &AppHandle) {
    use std::path::Path;
    let db = app.state::<Db>();
    let rows = match db::burn_jobs_reattachable_rows(&db) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("reattach: scan failed: {e}");
            return;
        }
    };
    for row in rows {
        if row.state != "running" {
            // Queued rows just rehydrate into the frontend queue and
            // wait for the operator to press Start.
            continue;
        }
        let job_id = row.job_id.clone();
        let Some(pid_i) = row.helper_pid else {
            db::record_burn_failed(
                &db,
                &job_id,
                "EORPHAN",
                "running row missing helper_pid; cannot reattach",
            );
            continue;
        };
        let pid = pid_i as u32;
        let Some(progress_path) = row.progress_file.clone() else {
            db::record_burn_failed(
                &db,
                &job_id,
                "EORPHAN",
                "running row missing progress_file; cannot reattach",
            );
            continue;
        };
        if !pid_alive(pid) {
            db::record_burn_failed(
                &db,
                &job_id,
                "EORPHAN",
                "recorded helper pid is no longer alive",
            );
            continue;
        }
        if !Path::new(&progress_path).exists() {
            db::record_burn_failed(
                &db,
                &job_id,
                "EORPHAN",
                "progress file no longer exists; helper may have crashed",
            );
            continue;
        }
        // Helper is still live. Re-register in the in-memory caches
        // start_write would have populated, then spawn a reattach
        // tail thread that polls pid liveness instead of waiting on
        // an owned Child handle.
        if let Some(reg) = app.try_state::<ElevatedJobs>() {
            reg.0.lock().unwrap().insert(job_id.clone(), pid);
        }
        if let Some(active) = app.try_state::<ActiveBurns>() {
            active.insert(ActiveBurn {
                job_id: job_id.clone(),
                target: row.target_device.clone(),
                kind: BurnKind::Elevated,
            });
        }
        let app_for_tail = app.clone();
        let job_for_tail = job_id.clone();
        let path_for_tail = progress_path;
        std::thread::spawn(move || {
            tail_helper_reattach(app_for_tail, job_for_tail, path_for_tail, pid);
        });
    }
}

// Reattach variant of tail_helper. Differs only in liveness detection:
// the original takes ownership of a `Child` so it can call try_wait();
// after a parent restart we have no Child handle, so we fall back to
// kill(0) probes against the recorded pid. JSONL parsing/emission is
// otherwise identical — emit_helper_line handles both ends.
fn tail_helper_reattach(app: AppHandle, job_id: String, path: String, pid: u32) {
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
        if !pid_alive(pid) {
            // Helper exited without writing a terminal record.
            db::record_burn_failed(
                &app.state::<Db>(),
                &job_id,
                "EORPHAN",
                "helper exited without finishing",
            );
            app.state::<ActiveBurns>().remove(&job_id);
            let _ = app.emit(
                "disk-cutter://job-error",
                JobFailure {
                    job_id: job_id.clone(),
                    error_code: "EORPHAN".into(),
                    error_message: "helper exited without finishing".into(),
                },
            );
            break;
        }
        if start.elapsed() > Duration::from_secs(3600) {
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(cancel_sentinel_path(&job_id));
    if let Some(reg) = app.try_state::<ElevatedJobs>() {
        reg.0.lock().unwrap().remove(&job_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_burn(id: &str, kind: BurnKind) -> ActiveBurn {
        ActiveBurn {
            job_id: id.into(),
            target: "/dev/disk5".into(),
            kind,
        }
    }

    #[test]
    fn active_burns_insert_remove_snapshot_roundtrip() {
        let reg = ActiveBurns::default();
        assert!(reg.is_empty());
        reg.insert(mk_burn("j1", BurnKind::Elevated));
        reg.insert(mk_burn("j2", BurnKind::InProcess));
        assert!(!reg.is_empty());
        let snap = reg.snapshot();
        assert_eq!(snap.len(), 2);
        reg.remove("j1");
        let snap = reg.snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].job_id, "j2");
        reg.remove("j2");
        assert!(reg.is_empty());
    }

    #[test]
    fn cancel_all_burns_writes_markers_for_every_active_job() {
        let reg = ActiveBurns::default();
        let cancel = CancelRegistry::default();
        let id1 = format!("test-cancel-all-{}-a", std::process::id());
        let id2 = format!("test-cancel-all-{}-b", std::process::id());
        // Flag exists only for the in-process job — cancel_all_burns must still
        // write a marker for the elevated one even when no flag is registered.
        let flag1 = Arc::new(AtomicBool::new(false));
        cancel.0.lock().unwrap().insert(id1.clone(), flag1.clone());
        reg.insert(mk_burn(&id1, BurnKind::InProcess));
        reg.insert(mk_burn(&id2, BurnKind::Elevated));

        let count = cancel_all_burns(&reg, &cancel);
        assert_eq!(count, 2);
        assert!(flag1.load(Ordering::Relaxed), "in-process flag flipped");
        let m1 = cancel_sentinel_path(&id1);
        let m2 = cancel_sentinel_path(&id2);
        assert!(m1.exists(), "sentinel for in-process burn");
        assert!(m2.exists(), "sentinel for elevated burn");
        let _ = std::fs::remove_file(&m1);
        let _ = std::fs::remove_file(&m2);
    }

    #[test]
    fn wait_for_burns_to_clear_returns_false_on_timeout() {
        let reg = ActiveBurns::default();
        reg.insert(mk_burn("stuck", BurnKind::Elevated));
        let cleared = wait_for_burns_to_clear(&reg, std::time::Duration::from_millis(150));
        assert!(!cleared);
    }

    #[test]
    fn wait_for_burns_to_clear_returns_true_when_drained() {
        let reg = std::sync::Arc::new(ActiveBurns::default());
        reg.insert(mk_burn("j1", BurnKind::Elevated));
        let reg2 = reg.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(50));
            reg2.remove("j1");
        });
        let cleared = wait_for_burns_to_clear(&reg, std::time::Duration::from_secs(2));
        assert!(cleared);
    }

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
        assert_eq!(code_for(&BurnError::Io(io::Error::other("x"))), "EIO");
    }

    #[test]
    fn fail_for_burn_error_assembles_code_and_debug_message() {
        let f = fail_for_burn_error("job-1", &BurnError::Cancelled);
        assert_eq!(f.job_id, "job-1");
        assert_eq!(f.error_code, "ECANCELLED");
        assert!(
            f.error_message.contains("Cancelled"),
            "got {}",
            f.error_message
        );
    }

    #[test]
    fn fail_for_burn_error_maps_io_variant() {
        use std::io;
        let e = BurnError::Io(io::Error::other("x"));
        let f = fail_for_burn_error("j", &e);
        assert_eq!(f.error_code, "EIO");
    }

    #[test]
    fn fail_for_burn_error_maps_size_mismatch_variant() {
        let f = fail_for_burn_error(
            "j",
            &BurnError::SizeMismatch {
                expected: 1,
                actual: 2,
            },
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
    fn parse_helper_line_log_yields_log_event() {
        let line =
            r#"{"kind":"log","level":"debug","message":"decoder_chain: matched layer 0 = xz"}"#;
        let ev = parse_helper_line("j", line).expect("parsed");
        match ev {
            HelperEvent::Log { level, message } => {
                assert_eq!(level, "debug");
                assert_eq!(message, "decoder_chain: matched layer 0 = xz");
            }
            _ => panic!("expected Log"),
        }
    }

    #[test]
    fn parse_helper_line_log_defaults_missing_fields() {
        // Robustness: a malformed helper line shouldn't kill the tail
        // thread. Missing level defaults to info, missing message to "".
        let line = r#"{"kind":"log"}"#;
        let ev = parse_helper_line("j", line).expect("parsed");
        match ev {
            HelperEvent::Log { level, message } => {
                assert_eq!(level, "info");
                assert_eq!(message, "");
            }
            _ => panic!("expected Log"),
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
            None,
            None,
            None,
            None,
            false,
            false,
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
            None,
            None,
            None,
            None,
            false,
            false,
        );
        assert!(s.contains("--image='/tmp/it'\\''s a test.iso'"), "got {s}");
    }

    #[test]
    fn build_helper_command_appends_perf_tunables_when_provided() {
        let s = build_helper_command(
            "/exe",
            "/tmp/x.iso",
            "/dev/disk5",
            "j",
            "/tmp/p",
            Some("pipelined"),
            Some(2_097_152),
            Some(8),
            Some(31),
            true,
            true,
        );
        assert!(s.contains("--writer='pipelined'"), "got {s}");
        assert!(s.contains("--chunk-bytes=2097152"), "got {s}");
        assert!(s.contains("--workers=8"), "got {s}");
        assert!(s.contains("--queue-depth=31"), "got {s}");
        assert!(s.contains("--skip-verify=true"), "got {s}");
        assert!(s.contains("--debug=true"), "got {s}");
    }

    #[test]
    fn build_helper_command_omits_unset_perf_tunables() {
        let s = build_helper_command(
            "/exe",
            "/tmp/x.iso",
            "/dev/disk5",
            "j",
            "/tmp/p",
            None,
            None,
            None,
            None,
            false,
            false,
        );
        assert!(!s.contains("--chunk-bytes="), "got {s}");
        assert!(!s.contains("--workers="), "got {s}");
        assert!(!s.contains("--queue-depth="), "got {s}");
        assert!(!s.contains("--skip-verify="), "got {s}");
        assert!(!s.contains("--debug="), "got {s}");
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
        // Label shape changed in Phase 3 (decoder-chain migration): the
        // legacy "ISO 9660 / RAW" combo label is gone — raw ISO sources
        // now surface as a single "ISO 9660" string. Compression layered
        // on top would surface as e.g. "XZ" instead of "ISO 9660 / XZ".
        assert_eq!(info.format, "ISO 9660");
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
        let xml = info_plist(&[("Internal", "true", "")]);
        let d = parse_disk_info_plist(&xml, "/dev/disk1".into()).unwrap();
        assert!(d.flags.contains(&"INTERNAL".to_string()));
    }

    #[test]
    fn parse_disk_info_plist_treats_any_removable_alias_as_removable() {
        for key in [
            "Removable",
            "RemovableMedia",
            "RemovableMediaOrExternalDevice",
        ] {
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

    #[test]
    fn is_linux_block_disk_accepts_real_devices() {
        assert!(is_linux_block_disk("sda"));
        assert!(is_linux_block_disk("nvme0n1"));
        assert!(is_linux_block_disk("vda"));
        assert!(is_linux_block_disk("mmcblk0"));
    }

    #[test]
    fn is_linux_block_disk_rejects_virtual_kinds() {
        assert!(!is_linux_block_disk("loop0"));
        assert!(!is_linux_block_disk("ram0"));
        assert!(!is_linux_block_disk("dm-0"));
        assert!(!is_linux_block_disk("sr0"));
        assert!(!is_linux_block_disk("fd0"));
        assert!(!is_linux_block_disk("md0"));
        assert!(!is_linux_block_disk("zram0"));
        assert!(!is_linux_block_disk(""));
    }

    #[test]
    fn parse_windows_diskdrive_json_handles_single_object() {
        let json = br#"{"DeviceID":"\\\\.\\PHYSICALDRIVE0","Model":"Samsung SSD 980","Size":500107862016,"InterfaceType":"SCSI","MediaType":"Fixed hard disk media"}"#;
        let disks = parse_windows_diskdrive_json(json).unwrap();
        assert_eq!(disks.len(), 1);
        let d = &disks[0];
        assert_eq!(d.device, r"\\.\PHYSICALDRIVE0");
        assert_eq!(d.model, "SAMSUNG SSD 980");
        assert_eq!(d.bytes, 500_107_862_016);
        assert_eq!(d.bus, "SCSI");
        assert!(d.flags.contains(&"INTERNAL".to_string()));
    }

    #[test]
    fn parse_windows_diskdrive_json_handles_array_with_removable() {
        let json = br#"[
            {"DeviceID":"\\\\.\\PHYSICALDRIVE0","Model":"NVMe","Size":1000204886016,"InterfaceType":"SCSI","MediaType":"Fixed hard disk media"},
            {"DeviceID":"\\\\.\\PHYSICALDRIVE1","Model":"USB Flash","Size":16000000000,"InterfaceType":"USB","MediaType":"Removable Media"}
        ]"#;
        let disks = parse_windows_diskdrive_json(json).unwrap();
        assert_eq!(disks.len(), 2);
        assert!(disks[1].flags.contains(&"REMOVABLE".to_string()));
        assert_eq!(disks[1].bus, "USB");
    }

    #[test]
    fn parse_windows_diskdrive_json_accepts_size_as_string() {
        // PowerShell sometimes serialises u64 Size as a string when it
        // exceeds the JavaScript-safe integer range.
        let json = br#"{"DeviceID":"\\\\.\\PHYSICALDRIVE0","Model":"X","Size":"4000787030016","InterfaceType":"SCSI","MediaType":"Fixed hard disk media"}"#;
        let disks = parse_windows_diskdrive_json(json).unwrap();
        assert_eq!(disks[0].bytes, 4_000_787_030_016);
    }

    #[test]
    fn parse_windows_diskdrive_json_returns_empty_on_blank_input() {
        let disks = parse_windows_diskdrive_json(b"").unwrap();
        assert!(disks.is_empty());
    }

    #[test]
    fn parse_windows_diskdrive_json_returns_none_for_invalid_json() {
        assert!(parse_windows_diskdrive_json(b"not json").is_none());
    }

    #[test]
    fn windows_disk_from_json_defaults_unknown_when_fields_missing() {
        let v: serde_json::Value =
            serde_json::from_str(r#"{"DeviceID":"\\\\.\\PHYSICALDRIVE9"}"#).unwrap();
        let d = windows_disk_from_json(&v).unwrap();
        assert_eq!(d.device, r"\\.\PHYSICALDRIVE9");
        assert_eq!(d.model, "UNKNOWN");
        assert_eq!(d.bus, "UNKNOWN");
        assert_eq!(d.bytes, 0);
        assert_eq!(d.capacity, "—");
        assert!(d.flags.contains(&"INTERNAL".to_string()));
    }

    #[test]
    fn windows_disk_from_json_returns_none_without_device_id() {
        let v: serde_json::Value = serde_json::from_str(r#"{"Model":"X"}"#).unwrap();
        assert!(windows_disk_from_json(&v).is_none());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn build_linux_disk_reads_sysfs_layout() {
        use std::fs;
        let dir = tempfile::tempdir().unwrap();
        let block = dir.path().join("sda");
        let dev = block.join("device");
        fs::create_dir_all(&dev).unwrap();
        fs::write(block.join("size"), "1024\n").unwrap();
        fs::write(block.join("removable"), "0\n").unwrap();
        fs::write(dev.join("vendor"), "SAMSUNG\n").unwrap();
        fs::write(dev.join("model"), "SSD 980 EVO\n").unwrap();
        fs::create_dir(block.join("sda1")).unwrap();
        fs::create_dir(block.join("sda2")).unwrap();
        // Sibling dirs that should be ignored.
        fs::create_dir(block.join("holders")).unwrap();
        fs::create_dir(block.join("slaves")).unwrap();
        let d = build_linux_disk("sda", &block).unwrap();
        assert_eq!(d.device, "/dev/sda");
        assert_eq!(d.bytes, 1024 * 512);
        assert_eq!(d.partitions, "2 PARTITIONS");
        assert!(d.model.contains("SAMSUNG"));
        assert!(d.flags.contains(&"INTERNAL".to_string()));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn build_linux_disk_marks_removable_when_flag_set() {
        use std::fs;
        let dir = tempfile::tempdir().unwrap();
        let block = dir.path().join("sdb");
        let dev = block.join("device");
        fs::create_dir_all(&dev).unwrap();
        fs::write(block.join("size"), "0").unwrap();
        fs::write(block.join("removable"), "1").unwrap();
        let d = build_linux_disk("sdb", &block).unwrap();
        assert!(d.flags.contains(&"REMOVABLE".to_string()));
        assert!(!d.flags.contains(&"INTERNAL".to_string()));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn list_linux_partitions_reports_unformatted_when_no_partition_dirs() {
        use std::fs;
        let dir = tempfile::tempdir().unwrap();
        let block = dir.path().join("sdc");
        fs::create_dir_all(&block).unwrap();
        fs::create_dir(block.join("holders")).unwrap();
        assert_eq!(list_linux_partitions(&block), "UNFORMATTED");
    }
}
