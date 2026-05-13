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

use tauri::State;

use crate::db::{self, Db};
use crate::forensic::{self, HostInfo};
use crate::inspect::{self, PartitionSummary};
use crate::snapshot::{self, RestoreResult, SnapshotResult, DEFAULT_SNAPSHOT_BYTES};

/// Probe an image source for partition table + per-partition
/// filesystem. Works on raw / iso / qcow2 / vhd / vhdx / vmdk via
/// `inspect::inspect_any`. Returns `None` (as an Err string) for
/// images without a partition table — the UI surfaces that as a
/// "single filesystem, no table" hint.
#[tauri::command]
pub fn inspect_partitions(path: String) -> Result<PartitionSummary, String> {
    let p = Path::new(&path);
    inspect::inspect_any(p).ok_or_else(|| format!("no partition table found in {path}"))
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
/// burn_history row + burn_logs rows + current host info; the digest
/// is recomputed on the fly so a tampered DB row produces a digest
/// mismatch.
#[tauri::command]
pub fn export_burn_report(
    db: State<'_, Db>,
    job_id: String,
    format: Option<String>,
) -> Result<String, String> {
    let (burn, logs) = load_burn_and_logs(&db, &job_id)?;
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
    let result =
        backup::run_to_file(&options, &cancel, |_| {}).map_err(|e| format!("backup: {e:?}"))?;
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

/// Pull the burn_history row + burn_logs rows for a given `job_id`.
/// Pure-ish — does the DB lookup but doesn't compute anything. The
/// forensic-export command uses this to build its inputs.
fn load_burn_and_logs(
    db: &Db,
    job_id: &str,
) -> Result<(db::BurnRecord, Vec<db::BurnLogRow>), String> {
    let conn = db.0.lock().map_err(|e| format!("db lock: {e}"))?;
    let burn = lookup_burn_by_job_id(&conn, job_id)
        .ok_or_else(|| format!("no burn_history row for job_id {job_id}"))?;
    let logs = lookup_logs_for_burn(&conn, burn.id)?;
    Ok((burn, logs))
}

/// SELECT one row out of burn_history by job_id, falling back to the
/// most recent matching row if there are multiple (e.g. a re-run with
/// the same id, which the schema allows). Pure SQL over the open
/// connection — no app-level dependencies.
fn lookup_burn_by_job_id(conn: &rusqlite::Connection, job_id: &str) -> Option<db::BurnRecord> {
    let sql = r#"
        SELECT id, job_id, image_path, image_name, image_bytes, target_device,
               source_sha256, readback_sha256, verify_match, bytes_written,
               elapsed_ms, avg_write_bps, avg_verify_bps,
               state, error_code, error_message,
               started_at, finished_at
        FROM burn_history
        WHERE job_id = ?1
        ORDER BY started_at DESC
        LIMIT 1
    "#;
    conn.query_row(sql, [job_id], |r| {
        Ok(db::BurnRecord {
            id: r.get(0)?,
            job_id: r.get(1)?,
            image_path: r.get(2)?,
            image_name: r.get(3)?,
            image_bytes: r.get::<_, i64>(4)? as u64,
            target_device: r.get(5)?,
            source_sha256: r.get(6)?,
            readback_sha256: r.get(7)?,
            verify_match: r.get(8)?,
            bytes_written: r.get::<_, Option<i64>>(9)?.map(|v| v as u64),
            elapsed_ms: r.get::<_, Option<i64>>(10)?.map(|v| v as u64),
            avg_write_bps: r.get::<_, Option<i64>>(11)?.map(|v| v as u64),
            avg_verify_bps: r.get::<_, Option<i64>>(12)?.map(|v| v as u64),
            state: r.get(13)?,
            error_code: r.get(14)?,
            error_message: r.get(15)?,
            started_at: r.get(16)?,
            finished_at: r.get(17)?,
        })
    })
    .ok()
}

fn lookup_logs_for_burn(
    conn: &rusqlite::Connection,
    burn_id: i64,
) -> Result<Vec<db::BurnLogRow>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT id, burn_id, ts, level, message
             FROM burn_logs
             WHERE burn_id = ?1
             ORDER BY ts ASC",
        )
        .map_err(|e| format!("prepare burn_logs: {e}"))?;
    let rows = stmt
        .query_map([burn_id], |r| {
            Ok(db::BurnLogRow {
                id: r.get(0)?,
                burn_id: r.get(1)?,
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

    fn insert_burn_row(conn: &Connection, job_id: &str) -> i64 {
        conn.execute(
            "INSERT INTO burn_history(job_id, image_path, image_name, image_bytes, target_device, state, started_at)
             VALUES (?1, '/tmp/x.iso', 'x.iso', 4096, '/dev/disk5', 'completed', 1715600000000)",
            [job_id],
        ).unwrap();
        conn.last_insert_rowid()
    }

    #[test]
    fn lookup_burn_by_job_id_returns_some_for_existing_row() {
        let conn = fresh_conn();
        insert_burn_row(&conn, "job-7");
        let r = lookup_burn_by_job_id(&conn, "job-7").unwrap();
        assert_eq!(r.job_id, "job-7");
        assert_eq!(r.image_bytes, 4096);
        assert_eq!(r.target_device, "/dev/disk5");
    }

    #[test]
    fn lookup_burn_by_job_id_returns_none_for_missing() {
        let conn = fresh_conn();
        assert!(lookup_burn_by_job_id(&conn, "ghost").is_none());
    }

    #[test]
    fn lookup_burn_by_job_id_prefers_most_recent_row() {
        let conn = fresh_conn();
        // Two rows with same job_id — should win the later one.
        conn.execute(
            "INSERT INTO burn_history(job_id, image_path, image_name, image_bytes, target_device, state, started_at)
             VALUES ('dup', '/tmp/x', 'x', 100, '/dev/old', 'failed', 1000)",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO burn_history(job_id, image_path, image_name, image_bytes, target_device, state, started_at)
             VALUES ('dup', '/tmp/x', 'x', 200, '/dev/new', 'completed', 2000)",
            [],
        ).unwrap();
        let r = lookup_burn_by_job_id(&conn, "dup").unwrap();
        assert_eq!(r.target_device, "/dev/new");
        assert_eq!(r.image_bytes, 200);
    }

    #[test]
    fn lookup_logs_for_burn_returns_ascending_by_ts() {
        let conn = fresh_conn();
        let id = insert_burn_row(&conn, "job-9");
        conn.execute(
            "INSERT INTO burn_logs(burn_id, ts, level, message) VALUES (?1, 200, 'info', 'b')",
            [id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO burn_logs(burn_id, ts, level, message) VALUES (?1, 100, 'info', 'a')",
            [id],
        )
        .unwrap();
        let rows = lookup_logs_for_burn(&conn, id).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].message, "a");
        assert_eq!(rows[1].message, "b");
    }

    #[test]
    fn lookup_logs_for_burn_empty_for_missing_burn_id() {
        let conn = fresh_conn();
        let rows = lookup_logs_for_burn(&conn, 999).unwrap();
        assert!(rows.is_empty());
    }
}
