//! Tauri command glue for backup / inspect / snapshot / forensic.
//!
//! Each module already exposes pure-function-tested logic; this file
//! is the thin layer that lifts those functions into the Tauri IPC
//! surface so the frontend can invoke them. Keeping the glue in one
//! place leaves the core modules dependency-free (no tauri import in
//! `inspect.rs`, etc.) and makes the invoke_handler list trivially
//! greppable.
//!
//! Pattern per command: take String paths (Tauri serialises filesystem
//! paths as strings), convert to `Path`, call the pure backend, return
//! `Result<T, String>` so the frontend gets either the structured
//! payload or an error message it can show in a toast.

use std::path::Path;

use tauri::{AppHandle, Emitter, Manager, State};

use crate::db::{self, Db};
use crate::forensic::{self, HostInfo};
use crate::image::{BootSource, DiskImage};
use crate::inspect::PartitionSummary;
use crate::joblog::JobLogger;
use crate::snapshot::{self, RestoreResult, SnapshotResult, DEFAULT_SNAPSHOT_BYTES};

/// Probe an image source for partition table + per-partition
/// filesystem. Returns `None` (as an Err string) for images without a
/// partition table — the UI surfaces that as a "single filesystem, no
/// table" hint. Format dispatch lives in `DiskImage::open`.
#[tauri::command]
pub fn inspect_partitions(path: String) -> Result<PartitionSummary, String> {
    let p = Path::new(&path);
    let img = DiskImage::open(p).map_err(|e| e.to_string())?;
    img.partitions()
        .cloned()
        .ok_or_else(|| format!("no partition table found in {path}"))
}

/// Same probe as `inspect_partitions`, but async — spawns a worker and
/// emits `disk-cutter://image-partitioned { job_id, summary }` once
/// done. `summary` is `null` for images without a recognised table
/// (superfloppy / compressed / unrecognised) so the UI can render an
/// "extract / no layout" placeholder instead of treating it as an
/// error.
///
/// The frontend kicks this off once an image's validation result
/// comes back as `valid` — gating it on validation keeps the partition
/// probe from running on files we've already rejected as not-a-disk.
#[tauri::command]
pub fn inspect_image_partitions(app: AppHandle, job_id: i64, path: String) -> Result<(), String> {
    std::thread::spawn(move || {
        let log = crate::joblog::db_logger_for(&app, job_id);
        log.debug("scan: partition probe starting");
        // Prefer the deep-scan cache when it's fresh — its
        // partition_table covers filesystem labels for every
        // partition, including those past the prefix probe's
        // ~33 KB window. Without this, an app restart re-runs the
        // prefix-only probe and overwrites the cached full data
        // with partitions whose filesystem is null for everything
        // past the prefix, so the UI shows the partition-type
        // label ("Linux filesystem") instead of "ext4" / "FAT32".
        let cached = db::image_scan_get(&app.state::<Db>(), &path)
            .filter(|r| r.scan_complete && db::image_scan_is_fresh(r))
            .and_then(|r| r.partition_table)
            .and_then(|json| serde_json::from_str::<serde_json::Value>(&json).ok());
        if let Some(summary) = cached {
            log.info("scan: partition probe served from deep-scan cache");
            #[derive(serde::Serialize, Clone)]
            struct CachedPayload {
                job_id: i64,
                summary: serde_json::Value,
            }
            let _ = app.emit(
                "disk-cutter://image-partitioned",
                CachedPayload { job_id, summary },
            );
            return;
        }
        let summary = DiskImage::open_with_log(Path::new(&path), &log)
            .ok()
            .and_then(|img| img.partitions().cloned());
        match &summary {
            Some(s) => log.info(&format!(
                "scan: partition probe found {} partition(s), table = {}",
                s.partitions.len(),
                s.table_kind
            )),
            None => log.info("scan: partition probe found no recognised table"),
        }
        #[derive(serde::Serialize, Clone)]
        struct Payload {
            job_id: i64,
            summary: Option<PartitionSummary>,
        }
        let _ = app.emit(
            "disk-cutter://image-partitioned",
            Payload { job_id, summary },
        );
    });
    Ok(())
}

/// Async bootability probe — spawns a worker and emits
/// `disk-cutter://image-boot-checked { job_id, bootable, sources }`.
/// `sources` is the list of every boot signal the image presents (MBR
/// active partition, GPT EFI System Partition, GPT legacy-BIOS bit,
/// MBR bootloader code, ISO 9660 El Torito) so the UI can show "via
/// El Torito + ESP" detail when more than one fires.
#[tauri::command]
pub fn inspect_image_bootable(app: AppHandle, job_id: i64, path: String) -> Result<(), String> {
    std::thread::spawn(move || {
        let log = crate::joblog::db_logger_for(&app, job_id);
        log.debug("scan: bootability probe starting");
        let (bootable, sources) = match DiskImage::open_with_log(Path::new(&path), &log) {
            Ok(img) => (img.is_bootable(), img.boot_sources().to_vec()),
            Err(e) => {
                log.warn(&format!(
                    "scan: bootability probe could not open image: {e}"
                ));
                (false, Vec::new())
            }
        };
        log.info(&format!(
            "scan: bootability probe: bootable={bootable}, sources={}",
            sources.len()
        ));
        #[derive(serde::Serialize, Clone)]
        struct Payload {
            job_id: i64,
            bootable: bool,
            sources: Vec<BootSource>,
        }
        let _ = app.emit(
            "disk-cutter://image-boot-checked",
            Payload {
                job_id,
                bootable,
                sources,
            },
        );
    });
    Ok(())
}

/// Trigger a deep scan for an image attached to a queue row. Spawns a
/// worker thread that streams through the decoder chain, captures
/// per-partition filesystem samples, and emits progressive Tauri events
/// so the row UI can populate as data arrives. Cached results live in
/// `image_scans` keyed by `image_path`; subsequent calls with the same
/// path short-circuit to `image-scan-complete` if the cached row is
/// still fresh (file size + mtime unchanged).
#[tauri::command]
pub fn scan_image_for_row(app: AppHandle, job_id: i64, image_path: String) -> Result<(), String> {
    crate::image_scan::spawn_scan(app, job_id, image_path);
    Ok(())
}

/// Capture the first N bytes of `device` into a recovery file at
/// `output`. Default 4 MiB covers LBA0 + GPT primary header + first
/// part of any existing filesystem superblock. The recovery file
/// carries a sha256 digest so `restore_snapshot` can refuse a
/// corrupted file before it touches the device.
#[tauri::command]
pub fn capture_snapshot(
    device: String,
    output: String,
    bytes: Option<u64>,
) -> Result<SnapshotResult, String> {
    let n = bytes.unwrap_or(DEFAULT_SNAPSHOT_BYTES);
    snapshot::snapshot_target(Path::new(&device), Path::new(&output), n)
        .map_err(|e| format!("snapshot: {e:?}"))
}

/// Write a recovery file back to the first bytes of `device`. The
/// recovery file's sha256 is verified before any write happens; a
/// corrupt recovery file errors instead of overwriting the device
/// with garbage.
#[tauri::command]
pub fn restore_snapshot(recovery: String, device: String) -> Result<RestoreResult, String> {
    snapshot::restore_target(Path::new(&recovery), Path::new(&device))
        .map_err(|e| format!("restore: {e:?}"))
}

/// Build a forensic burn-record report for a finished job. `format`
/// is either `"json"` (canonical pretty JSON with sha256 digest) or
/// `"markdown"` (human-readable summary). The report aggregates the
/// burn_jobs row + burn_logs rows + current host info; the digest
/// is recomputed on the fly so a tampered DB row produces a digest
/// mismatch.
#[tauri::command]
pub fn export_burn_report(
    db: State<'_, Db>,
    job_id: i64,
    format: Option<String>,
) -> Result<String, String> {
    let (burn, logs) = load_burn_and_logs(&db, job_id)?;
    let host = HostInfo::current();
    let report = forensic::build_report(&burn, &logs, host);
    let fmt = format.as_deref().unwrap_or("json");
    match fmt {
        "json" => Ok(forensic::to_pretty_json(&report)),
        "markdown" | "md" => Ok(forensic::to_markdown(&report)),
        other => Err(format!(
            "unknown format: {other} (want 'json' or 'markdown')"
        )),
    }
}

/// Sync wrapper around `backup::run_to_file`. The Tauri thread blocks
/// until the backup completes; that's fine for small images and lets
/// the frontend show a single spinner without progress plumbing.
/// Larger images should be driven through `start_backup_async` (TBD)
/// to get progress events — but that's a follow-up; this command
/// unblocks the GUI today.
#[tauri::command]
pub fn run_backup(
    source: String,
    output: String,
    compression: Option<String>,
    sparse: Option<bool>,
) -> Result<BackupResultJson, String> {
    use crate::backup::{self, BackupOptions, Compression};
    use std::sync::atomic::AtomicBool;

    let compression = match compression.as_deref() {
        None => Compression::None,
        Some(s) => Compression::parse(s).ok_or_else(|| format!("unknown compression: {s}"))?,
    };
    let sparse = sparse.unwrap_or(false);
    let src = Path::new(&source);
    let source_bytes = backup::probe_source_size(src).map_err(|e| format!("probe source: {e}"))?;
    let options = BackupOptions {
        source_path: src.to_path_buf(),
        output_path: Path::new(&output).to_path_buf(),
        compression,
        chunk_size: 1024 * 1024,
        source_bytes,
        sparse,
    };
    let cancel = AtomicBool::new(false);
    // Sparse-aware fast path for qcow2 sources — skips reading zero/
    // unallocated clusters entirely. Detected by extension; the
    // allocated_extents() API in am-img-qcow2 0.3+ powers it.
    let ext = src
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase());
    let result = if matches!(ext.as_deref(), Some("qcow2") | Some("qcow")) {
        backup::run_qcow2_to_file(&options, &cancel, |_| {})
            .map_err(|e| format!("backup: {e:?}"))?
    } else {
        backup::run_to_file(&options, &cancel, |_| {}).map_err(|e| format!("backup: {e:?}"))?
    };
    Ok(BackupResultJson {
        bytes_read: result.bytes_read,
        bytes_written: result.bytes_written,
        source_sha256: result.source_sha256,
        elapsed_ms: result.elapsed.as_millis() as u64,
        avg_bytes_per_sec: result.avg_bytes_per_sec,
    })
}

#[derive(serde::Serialize, Clone)]
pub struct BackupResultJson {
    pub bytes_read: u64,
    pub bytes_written: u64,
    pub source_sha256: String,
    pub elapsed_ms: u64,
    pub avg_bytes_per_sec: u64,
}

/// Pull the burn_jobs row + burn_logs rows for a given `job_id`.
/// Pure-ish — does the DB lookup but doesn't compute anything. The
/// forensic-export command uses this to build its inputs.
fn load_burn_and_logs(db: &Db, job_id: i64) -> Result<(db::BurnJob, Vec<db::BurnLogRow>), String> {
    let conn = db.0.lock().map_err(|e| format!("db lock: {e}"))?;
    let burn = lookup_burn_by_job_id(&conn, job_id)
        .ok_or_else(|| format!("no burn_jobs row for job_id {job_id}"))?;
    let logs = lookup_logs_for_burn(&conn, job_id)?;
    Ok((burn, logs))
}

/// SELECT one row out of burn_jobs by job_id. With the integer PK
/// there is at most one matching row — the UNIQUE constraint is the
/// PRIMARY KEY itself.
fn lookup_burn_by_job_id(conn: &rusqlite::Connection, job_id: i64) -> Option<db::BurnJob> {
    let sql = r#"
        SELECT job_id, image_path, image_name, image_bytes, target_device,
               source_sha256, readback_sha256, verify_match, bytes_written,
               elapsed_ms, avg_write_bps, avg_verify_bps,
               state, error_code, error_message,
               queued_at, started_at, finished_at,
               progress_file, helper_pid
        FROM burn_jobs
        WHERE job_id = ?1
        LIMIT 1
    "#;
    conn.query_row(sql, [job_id], |r| {
        Ok(db::BurnJob {
            job_id: r.get(0)?,
            image_path: r.get(1)?,
            image_name: r.get(2)?,
            image_bytes: r.get::<_, i64>(3)? as u64,
            target_device: r.get(4)?,
            source_sha256: r.get(5)?,
            readback_sha256: r.get(6)?,
            verify_match: r.get::<_, Option<i32>>(7)?.map(|v| v != 0),
            bytes_written: r.get::<_, Option<i64>>(8)?.map(|v| v as u64),
            elapsed_ms: r.get::<_, Option<i64>>(9)?.map(|v| v as u64),
            avg_write_bps: r.get::<_, Option<i64>>(10)?.map(|v| v as u64),
            avg_verify_bps: r.get::<_, Option<i64>>(11)?.map(|v| v as u64),
            state: r.get(12)?,
            error_code: r.get(13)?,
            error_message: r.get(14)?,
            queued_at: r.get(15)?,
            started_at: r.get(16)?,
            finished_at: r.get(17)?,
            progress_file: r.get(18)?,
            helper_pid: r.get(19)?,
        })
    })
    .ok()
}

fn lookup_logs_for_burn(
    conn: &rusqlite::Connection,
    job_id: i64,
) -> Result<Vec<db::BurnLogRow>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT id, job_id, ts, level, message
             FROM burn_logs
             WHERE job_id = ?1
             ORDER BY ts ASC",
        )
        .map_err(|e| format!("prepare burn_logs: {e}"))?;
    let rows = stmt
        .query_map([job_id], |r| {
            Ok(db::BurnLogRow {
                id: r.get(0)?,
                job_id: r.get(1)?,
                ts: r.get(2)?,
                level: r.get(3)?,
                message: r.get(4)?,
            })
        })
        .map_err(|e| format!("query burn_logs: {e}"))?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| format!("collect burn_logs: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn fresh_conn() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::run(&mut conn).unwrap();
        conn
    }

    fn insert_burn_row(conn: &Connection) -> i64 {
        conn.execute(
            "INSERT INTO burn_jobs(image_path, image_name, image_bytes, target_device, state, queued_at)
             VALUES ('/tmp/x.iso', 'x.iso', 4096, '/dev/disk5', 'success', 1715600000000)",
            [],
        ).unwrap();
        conn.last_insert_rowid()
    }

    #[test]
    fn lookup_burn_by_job_id_returns_some_for_existing_row() {
        let conn = fresh_conn();
        let id = insert_burn_row(&conn);
        let r = lookup_burn_by_job_id(&conn, id).unwrap();
        assert_eq!(r.job_id, id);
        assert_eq!(r.image_bytes, 4096);
        assert_eq!(r.target_device, "/dev/disk5");
    }

    #[test]
    fn lookup_burn_by_job_id_returns_none_for_missing() {
        let conn = fresh_conn();
        assert!(lookup_burn_by_job_id(&conn, 9_999_999).is_none());
    }

    #[test]
    fn each_insert_mints_a_distinct_job_id() {
        let conn = fresh_conn();
        // With job_id as INTEGER PRIMARY KEY AUTOINCREMENT, every
        // INSERT (without specifying job_id) yields a fresh id; the
        // schema makes UNIQUE-violation duplicates impossible by
        // construction.
        let id1 = insert_burn_row(&conn);
        let id2 = insert_burn_row(&conn);
        assert_ne!(id1, id2);
        assert!(lookup_burn_by_job_id(&conn, id1).is_some());
        assert!(lookup_burn_by_job_id(&conn, id2).is_some());
    }

    #[test]
    fn lookup_logs_for_burn_returns_ascending_by_ts() {
        let conn = fresh_conn();
        let id = insert_burn_row(&conn);
        conn.execute(
            "INSERT INTO burn_logs(job_id, ts, level, message) VALUES (?1, 200, 'info', 'b')",
            [id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO burn_logs(job_id, ts, level, message) VALUES (?1, 100, 'info', 'a')",
            [id],
        )
        .unwrap();
        let rows = lookup_logs_for_burn(&conn, id).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].message, "a");
        assert_eq!(rows[1].message, "b");
    }

    #[test]
    fn lookup_logs_for_burn_empty_for_missing_job_id() {
        let conn = fresh_conn();
        let rows = lookup_logs_for_burn(&conn, 999).unwrap();
        assert!(rows.is_empty());
    }
}
