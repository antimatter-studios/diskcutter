pub mod migrations;

use std::collections::HashMap;
use std::sync::Mutex;

use rusqlite::{params, Connection};
use serde::Serialize;
use tauri::{AppHandle, Manager, State};

pub struct Db(pub Mutex<Connection>);

pub fn open(app: &AppHandle) -> rusqlite::Result<Connection> {
    let dir = app
        .path()
        .app_data_dir()
        .expect("app_data_dir resolves on supported platforms");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("disk-cutter.sqlite");
    let mut conn = Connection::open(&path)?;
    conn.execute_batch("PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL;")?;
    migrations::run(&mut conn)?;
    Ok(conn)
}

// One row in `burn_jobs`. The table is the source of truth for both
// the live queue and the burn history: every job that ever enters the
// queue lives here from the moment it's enqueued (`state='queued'`)
// through to a terminal state. `started_at`, `progress_file`, and
// `helper_pid` are only populated once a helper is actually spawned —
// they're the breadcrumbs the parent app uses to reattach to a still-
// running helper after a dev-server / app restart.
#[derive(Serialize, Clone)]
pub struct BurnJob {
    pub id: i64,
    pub job_id: String,
    pub image_path: String,
    pub image_name: String,
    pub image_bytes: u64,
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
    pub queued_at: i64,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    pub progress_file: Option<String>,
    pub helper_pid: Option<i64>,
}

#[derive(Serialize, Clone)]
pub struct BurnLogRow {
    pub id: i64,
    pub burn_id: i64,
    pub ts: i64,
    pub level: String,
    pub message: String,
}

/// Cached deep-scan results for a disk image. Lookup key is `image_path`;
/// `file_size` + `file_mtime` are the freshness check (read returns None
/// when they don't match the live file). All JSON-typed fields are stored
/// as strings the frontend deserialises.
#[derive(Serialize, Clone)]
pub struct ImageScanRow {
    pub id: i64,
    pub image_path: String,
    pub file_size: i64,
    pub file_mtime: i64,
    pub scanned_at: i64,
    pub scan_complete: bool,
    pub format_chain: Option<String>,
    pub uncompressed_bytes: Option<i64>,
    pub image_sha256: Option<String>,
    pub validation_result: Option<String>,
    pub validation_detail: Option<String>,
    pub partition_table: Option<String>,
    pub boot_sources: String,
    pub partition_offsets: String,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// Most-recent non-terminal row for a job_id. Used by the
// queued→running and running→terminal transitions to find the row to
// update without callers needing to track the burn_jobs.id themselves.
fn find_open_id(conn: &Connection, job_id: &str) -> Option<i64> {
    conn.query_row(
        "SELECT id FROM burn_jobs
         WHERE job_id = ?1 AND state IN ('queued','running')
         ORDER BY queued_at DESC LIMIT 1",
        params![job_id],
        |r| r.get::<_, i64>(0),
    )
    .ok()
}

// Specifically the running row (post-record_burn_started). Used by
// completion recorders so they can't accidentally close out a still-
// queued row that never got a started_at.
fn find_running_id(conn: &Connection, job_id: &str) -> Option<i64> {
    conn.query_row(
        "SELECT id FROM burn_jobs
         WHERE job_id = ?1 AND state = 'running'
         ORDER BY queued_at DESC LIMIT 1",
        params![job_id],
        |r| r.get::<_, i64>(0),
    )
    .ok()
}

// Insert a 'queued' row for this job_id, or return the id of an
// existing non-terminal row (idempotent w.r.t. re-enqueues during the
// same lifecycle). Returns the burn_jobs.id on success.
pub fn record_burn_queued(
    db: &Db,
    job_id: &str,
    image_path: &str,
    image_name: &str,
    image_bytes: u64,
    target_device: &str,
) -> Option<i64> {
    let conn = db.0.lock().ok()?;
    insert_queued_row(
        &conn,
        job_id,
        image_path,
        image_name,
        image_bytes,
        target_device,
    )
}

fn insert_queued_row(
    conn: &Connection,
    job_id: &str,
    image_path: &str,
    image_name: &str,
    image_bytes: u64,
    target_device: &str,
) -> Option<i64> {
    if let Some(existing) = find_open_id(conn, job_id) {
        return Some(existing);
    }
    conn.execute(
        "INSERT INTO burn_jobs (job_id, image_path, image_name, image_bytes,
            target_device, state, queued_at)
         VALUES (?1, ?2, ?3, ?4, ?5, 'queued', ?6)",
        params![
            job_id,
            image_path,
            image_name,
            image_bytes as i64,
            target_device,
            now_ms()
        ],
    )
    .ok()?;
    let id = conn.last_insert_rowid();
    let _ = conn.execute(
        "INSERT INTO burn_logs (burn_id, ts, level, message) VALUES (?1, ?2, 'info', ?3)",
        params![
            id,
            now_ms(),
            format!("queued: {image_name} → {target_device}")
        ],
    );
    Some(id)
}

// Transition the most recent open row for this job_id from queued →
// running, stamping `started_at` and (for the elevated path)
// `progress_file` / `helper_pid` so a future reattach can find the
// IPC sink. If no open row exists (e.g. legacy callers that never
// enqueued), this is a no-op — callers should enqueue first.
pub fn record_burn_started(
    db: &Db,
    job_id: &str,
    progress_file: Option<&str>,
    helper_pid: Option<u32>,
) {
    let conn = match db.0.lock() {
        Ok(c) => c,
        Err(_) => return,
    };
    let Some(id) = find_open_id(&conn, job_id) else {
        return;
    };
    let pid_i: Option<i64> = helper_pid.map(|p| p as i64);
    let _ = conn.execute(
        "UPDATE burn_jobs
         SET state='running',
             started_at=COALESCE(started_at, ?1),
             progress_file=COALESCE(?2, progress_file),
             helper_pid=COALESCE(?3, helper_pid)
         WHERE id=?4",
        params![now_ms(), progress_file, pid_i, id],
    );
    let _ = conn.execute(
        "INSERT INTO burn_logs (burn_id, ts, level, message) VALUES (?1, ?2, 'info', 'burn started')",
        params![id, now_ms()],
    );
}

#[allow(clippy::too_many_arguments)]
pub fn record_burn_completed(
    db: &Db,
    job_id: &str,
    source_sha256: &str,
    readback_sha256: &str,
    verify_match: bool,
    bytes_written: u64,
    elapsed_ms: u64,
    avg_write_bps: u64,
    avg_verify_bps: u64,
) {
    let conn = match db.0.lock() {
        Ok(c) => c,
        Err(_) => return,
    };
    let Some(id) = find_running_id(&conn, job_id) else {
        return;
    };
    let state = if verify_match { "success" } else { "error" };
    let err_code: Option<&str> = if verify_match {
        None
    } else {
        Some("EHASHMISMATCH")
    };
    let _ = conn.execute(
        "UPDATE burn_jobs
         SET source_sha256=?1, readback_sha256=?2, verify_match=?3,
             bytes_written=?4, elapsed_ms=?5, avg_write_bps=?6, avg_verify_bps=?7,
             state=?8, error_code=?9, finished_at=?10
         WHERE id=?11",
        params![
            source_sha256,
            readback_sha256,
            verify_match as i32,
            bytes_written as i64,
            elapsed_ms as i64,
            avg_write_bps as i64,
            avg_verify_bps as i64,
            state,
            err_code,
            now_ms(),
            id
        ],
    );
    let _ = conn.execute(
        "INSERT INTO burn_logs (burn_id, ts, level, message) VALUES (?1, ?2, ?3, ?4)",
        params![
            id,
            now_ms(),
            if verify_match { "info" } else { "error" },
            format!(
                "completed: verify_match={verify_match} bytes={bytes_written} elapsed_ms={elapsed_ms} \
                 write_bps={avg_write_bps} verify_bps={avg_verify_bps}"
            )
        ],
    );
}

pub fn record_burn_failed(db: &Db, job_id: &str, code: &str, message: &str) {
    let conn = match db.0.lock() {
        Ok(c) => c,
        Err(_) => return,
    };
    // Accept failure against either queued or running rows: failures
    // can land before the helper actually starts (FDA preflight, spawn
    // error) and we still want them recorded against the queued row.
    let Some(id) = find_open_id(&conn, job_id) else {
        return;
    };
    let state = if code == "ECANCELLED" {
        "cancelled"
    } else {
        "error"
    };
    let _ = conn.execute(
        "UPDATE burn_jobs
         SET state=?1, error_code=?2, error_message=?3, finished_at=?4
         WHERE id=?5",
        params![state, code, message, now_ms(), id],
    );
    let _ = conn.execute(
        "INSERT INTO burn_logs (burn_id, ts, level, message) VALUES (?1, ?2, 'error', ?3)",
        params![id, now_ms(), format!("{code}: {message}")],
    );
}

// ----------------------------------------------------------------------------
// image_scans cache. Keyed by image_path; freshness is verified on read via
// file_size + file_mtime. Helpers below all take `&Db` directly so scan
// workers (which run on `std::thread::spawn` and don't hold a Tauri State)
// can call them through the captured AppHandle.
// ----------------------------------------------------------------------------

const IMAGE_SCAN_COLS: &str = "id, image_path, file_size, file_mtime, scanned_at,
    scan_complete, format_chain, uncompressed_bytes, image_sha256,
    validation_result, validation_detail, partition_table, boot_sources,
    partition_offsets";

fn map_image_scan_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<ImageScanRow> {
    Ok(ImageScanRow {
        id: r.get(0)?,
        image_path: r.get(1)?,
        file_size: r.get(2)?,
        file_mtime: r.get(3)?,
        scanned_at: r.get(4)?,
        scan_complete: r.get::<_, i64>(5)? != 0,
        format_chain: r.get(6)?,
        uncompressed_bytes: r.get(7)?,
        image_sha256: r.get(8)?,
        validation_result: r.get(9)?,
        validation_detail: r.get(10)?,
        partition_table: r.get(11)?,
        boot_sources: r.get(12)?,
        partition_offsets: r.get(13)?,
    })
}

/// Look up the cached scan for an image path. Returns `None` if no row
/// exists. Returns `Some(row)` regardless of whether the cached `file_size`
/// + `file_mtime` still match the live file — callers do their own
/// freshness check via `image_scan_is_fresh` so a stale row isn't silently
/// discarded (the partition probe might still be useful for a quick view
/// even when the file has been touched).
pub fn image_scan_get(db: &Db, image_path: &str) -> Option<ImageScanRow> {
    let conn = db.0.lock().ok()?;
    let sql = format!("SELECT {IMAGE_SCAN_COLS} FROM image_scans WHERE image_path = ?1");
    conn.query_row(&sql, params![image_path], map_image_scan_row)
        .ok()
}

/// True when the cached scan's `file_size` + `file_mtime` still match the
/// live file on disk. Used by callers to decide whether to short-circuit
/// a fresh scan.
pub fn image_scan_is_fresh(row: &ImageScanRow) -> bool {
    let Ok(meta) = std::fs::metadata(&row.image_path) else {
        return false;
    };
    let size = meta.len() as i64;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    size == row.file_size && mtime == row.file_mtime
}

/// Mark a scan as in-progress for `image_path`. Overwrites any stale row.
/// Called at the start of a scan so the UI can read partial data via the
/// progress events while the worker continues.
#[allow(clippy::too_many_arguments)]
pub fn image_scan_begin(db: &Db, image_path: &str, file_size: i64, file_mtime: i64) -> Option<i64> {
    let conn = db.0.lock().ok()?;
    conn.execute(
        "INSERT INTO image_scans (image_path, file_size, file_mtime, scanned_at,
            scan_complete, boot_sources, partition_offsets)
         VALUES (?1, ?2, ?3, ?4, 0, '[]', '{}')
         ON CONFLICT(image_path) DO UPDATE SET
            file_size = excluded.file_size,
            file_mtime = excluded.file_mtime,
            scanned_at = excluded.scanned_at,
            scan_complete = 0,
            format_chain = NULL,
            uncompressed_bytes = NULL,
            image_sha256 = NULL,
            validation_result = NULL,
            validation_detail = NULL,
            partition_table = NULL,
            boot_sources = '[]',
            partition_offsets = '{}'",
        params![image_path, file_size, file_mtime, now_ms()],
    )
    .ok()?;
    Some(conn.last_insert_rowid())
}

/// Patch the cached scan with a partial update. Each `Option` field is
/// applied only when `Some(...)` — call sites can land one field at a
/// time as the scan worker discovers it. Always updates `scanned_at` so
/// freshness reflects the most recent write.
#[derive(Default, Debug, Clone)]
pub struct ImageScanPatch {
    pub format_chain: Option<String>,
    pub uncompressed_bytes: Option<i64>,
    pub image_sha256: Option<String>,
    pub validation_result: Option<String>,
    pub validation_detail: Option<String>,
    pub partition_table: Option<String>,
    pub boot_sources: Option<String>,
    pub partition_offsets: Option<String>,
    pub scan_complete: Option<bool>,
}

pub fn image_scan_patch(db: &Db, image_path: &str, patch: ImageScanPatch) {
    let Ok(conn) = db.0.lock() else { return };
    let mut sets: Vec<String> = Vec::new();
    let mut vals: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    if let Some(v) = patch.format_chain {
        sets.push(format!("format_chain = ?{}", sets.len() + 1));
        vals.push(Box::new(v));
    }
    if let Some(v) = patch.uncompressed_bytes {
        sets.push(format!("uncompressed_bytes = ?{}", sets.len() + 1));
        vals.push(Box::new(v));
    }
    if let Some(v) = patch.image_sha256 {
        sets.push(format!("image_sha256 = ?{}", sets.len() + 1));
        vals.push(Box::new(v));
    }
    if let Some(v) = patch.validation_result {
        sets.push(format!("validation_result = ?{}", sets.len() + 1));
        vals.push(Box::new(v));
    }
    if let Some(v) = patch.validation_detail {
        sets.push(format!("validation_detail = ?{}", sets.len() + 1));
        vals.push(Box::new(v));
    }
    if let Some(v) = patch.partition_table {
        sets.push(format!("partition_table = ?{}", sets.len() + 1));
        vals.push(Box::new(v));
    }
    if let Some(v) = patch.boot_sources {
        sets.push(format!("boot_sources = ?{}", sets.len() + 1));
        vals.push(Box::new(v));
    }
    if let Some(v) = patch.partition_offsets {
        sets.push(format!("partition_offsets = ?{}", sets.len() + 1));
        vals.push(Box::new(v));
    }
    if let Some(v) = patch.scan_complete {
        sets.push(format!("scan_complete = ?{}", sets.len() + 1));
        vals.push(Box::new(v as i64));
    }
    sets.push(format!("scanned_at = ?{}", sets.len() + 1));
    vals.push(Box::new(now_ms()));
    if sets.is_empty() {
        return;
    }
    let sql = format!(
        "UPDATE image_scans SET {} WHERE image_path = ?{}",
        sets.join(", "),
        sets.len() + 1
    );
    vals.push(Box::new(image_path.to_string()));
    let params_dyn: Vec<&dyn rusqlite::ToSql> = vals.iter().map(|b| b.as_ref()).collect();
    let _ = conn.execute(&sql, params_dyn.as_slice());
}

/// Delete any cached scan row for `image_path`. Used by the frontend's
/// REFRESH action so a re-expand triggers a fresh scan.
pub fn image_scan_invalidate(db: &Db, image_path: &str) {
    let Ok(conn) = db.0.lock() else { return };
    let _ = conn.execute(
        "DELETE FROM image_scans WHERE image_path = ?1",
        params![image_path],
    );
}

#[tauri::command]
pub fn image_scan_lookup(db: State<'_, Db>, image_path: String) -> Option<ImageScanRow> {
    let row = image_scan_get(&db, &image_path)?;
    if image_scan_is_fresh(&row) {
        Some(row)
    } else {
        None
    }
}

#[tauri::command]
pub fn image_scan_clear(db: State<'_, Db>, image_path: String) -> Result<(), String> {
    image_scan_invalidate(&db, &image_path);
    Ok(())
}

#[allow(dead_code)]
pub fn append_log(db: &Db, job_id: &str, level: &str, message: &str) {
    let conn = match db.0.lock() {
        Ok(c) => c,
        Err(_) => return,
    };
    if let Some(id) = find_open_id(&conn, job_id) {
        let _ = conn.execute(
            "INSERT INTO burn_logs (burn_id, ts, level, message) VALUES (?1, ?2, ?3, ?4)",
            params![id, now_ms(), level, message],
        );
    }
}

#[tauri::command]
pub fn config_get(db: State<'_, Db>, key: String) -> Option<String> {
    let conn = db.0.lock().ok()?;
    conn.query_row("SELECT value FROM config WHERE key=?1", params![key], |r| {
        r.get::<_, String>(0)
    })
    .ok()
}

#[tauri::command]
pub fn config_set(db: State<'_, Db>, key: String, value: String) -> Result<(), String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    conn.execute(
        "INSERT INTO config (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        params![key, value],
    )
    .map(|_| ())
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn config_all(db: State<'_, Db>) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let Ok(conn) = db.0.lock() else { return out };
    if let Ok(mut stmt) = conn.prepare("SELECT key, value FROM config") {
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)));
        if let Ok(rows) = rows {
            for kv in rows.flatten() {
                out.insert(kv.0, kv.1);
            }
        }
    }
    out
}

const BURN_JOB_COLS: &str = "id, job_id, image_path, image_name, image_bytes, target_device,
    source_sha256, readback_sha256, verify_match,
    bytes_written, elapsed_ms, avg_write_bps, avg_verify_bps,
    state, error_code, error_message,
    queued_at, started_at, finished_at,
    progress_file, helper_pid";

fn map_burn_job_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<BurnJob> {
    Ok(BurnJob {
        id: r.get(0)?,
        job_id: r.get(1)?,
        image_path: r.get(2)?,
        image_name: r.get(3)?,
        image_bytes: r.get::<_, i64>(4)? as u64,
        target_device: r.get(5)?,
        source_sha256: r.get(6)?,
        readback_sha256: r.get(7)?,
        verify_match: r.get::<_, Option<i32>>(8)?.map(|v| v != 0),
        bytes_written: r.get::<_, Option<i64>>(9)?.map(|v| v as u64),
        elapsed_ms: r.get::<_, Option<i64>>(10)?.map(|v| v as u64),
        avg_write_bps: r.get::<_, Option<i64>>(11)?.map(|v| v as u64),
        avg_verify_bps: r.get::<_, Option<i64>>(12)?.map(|v| v as u64),
        state: r.get(13)?,
        error_code: r.get(14)?,
        error_message: r.get(15)?,
        queued_at: r.get(16)?,
        started_at: r.get(17)?,
        finished_at: r.get(18)?,
        progress_file: r.get(19)?,
        helper_pid: r.get(20)?,
    })
}

// Full table dump for the history view. Newest-first.
#[tauri::command]
pub fn burn_jobs_list(db: State<'_, Db>, limit: Option<u32>) -> Result<Vec<BurnJob>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let lim = limit.unwrap_or(200) as i64;
    let sql = format!("SELECT {BURN_JOB_COLS} FROM burn_jobs ORDER BY queued_at DESC LIMIT ?1");
    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![lim], map_burn_job_row)
        .map_err(|e| e.to_string())?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

// Queue hydrate: every row that hasn't reached the 'success' terminal.
// Includes queued / running / error / cancelled — the queue UI keeps
// failures visible until the user dismisses them. Oldest-first so
// callers can rebuild insertion order.
#[tauri::command]
pub fn burn_jobs_active(db: State<'_, Db>) -> Result<Vec<BurnJob>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let sql = format!(
        "SELECT {BURN_JOB_COLS} FROM burn_jobs
         WHERE state <> 'success'
         ORDER BY queued_at ASC"
    );
    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], map_burn_job_row)
        .map_err(|e| e.to_string())?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

// Reattach scan: rows whose helper might still be live. The caller
// cross-references `helper_pid` against the OS process list and re-
// spawns a tail_helper for the matching `progress_file`. Includes
// queued rows too — if the app crashed between enqueue and
// start_write, the row is salvageable as a still-queued job rather
// than treating it as a failure.
pub fn burn_jobs_reattachable_rows(db: &Db) -> Result<Vec<BurnJob>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let sql = format!(
        "SELECT {BURN_JOB_COLS} FROM burn_jobs
         WHERE state IN ('queued','running')
         ORDER BY queued_at ASC"
    );
    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], map_burn_job_row)
        .map_err(|e| e.to_string())?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

#[tauri::command]
pub fn burn_jobs_clear(db: State<'_, Db>) -> Result<(), String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    conn.execute("DELETE FROM burn_jobs", [])
        .map_err(|e| e.to_string())?;
    Ok(())
}

// Frontend enqueue: inserts (or finds existing) a queued row. The
// returned id isn't needed by the frontend right now but we surface
// it for symmetry with the other recorders.
#[tauri::command]
pub fn enqueue_burn(
    db: State<'_, Db>,
    job_id: String,
    image_path: String,
    image_name: String,
    image_bytes: u64,
    target_device: String,
) -> Result<i64, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    insert_queued_row(
        &conn,
        &job_id,
        &image_path,
        &image_name,
        image_bytes,
        &target_device,
    )
    .ok_or_else(|| format!("failed to enqueue burn for {job_id}"))
}

// Hard-delete a job row. burn_logs cascades. Allowed in any state —
// the caller is responsible for having cancelled a running burn first
// via cancel_write; deleting the row while the helper is alive just
// orphans the DB from the helper, the OS-level cancellation is
// separate.
#[tauri::command]
pub fn remove_burn_job(db: State<'_, Db>, job_id: String) -> Result<(), String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    conn.execute("DELETE FROM burn_jobs WHERE job_id=?1", params![job_id])
        .map_err(|e| e.to_string())?;
    Ok(())
}

// Frontend-driven target reassignment for a queued row. No-op if the
// row is past 'queued' (re-targeting a running/finished burn would be
// nonsensical).
#[tauri::command]
pub fn set_burn_target(
    db: State<'_, Db>,
    job_id: String,
    target_device: String,
) -> Result<(), String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    conn.execute(
        "UPDATE burn_jobs SET target_device=?1
         WHERE job_id=?2 AND state='queued'",
        params![target_device, job_id],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub fn burn_logs_list(db: State<'_, Db>, burn_id: i64) -> Result<Vec<BurnLogRow>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare(
            "SELECT id, burn_id, ts, level, message FROM burn_logs
             WHERE burn_id=?1 ORDER BY ts ASC, id ASC",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![burn_id], |r| {
            Ok(BurnLogRow {
                id: r.get(0)?,
                burn_id: r.get(1)?,
                ts: r.get(2)?,
                level: r.get(3)?,
                message: r.get(4)?,
            })
        })
        .map_err(|e| e.to_string())?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

// Frontend convenience: take the user-facing string job_id and resolve to
// the integer burn_id via a join. Saves the UI from having to remember
// the int PK that lives only in the DB. Returns an empty list (not Err)
// when no burn_jobs row matches yet — fresh queued jobs hit this between
// enqueue and the first log line, and an empty panel is friendlier than
// an error toast.
#[tauri::command]
pub fn burn_logs_for_job(db: State<'_, Db>, job_id: String) -> Result<Vec<BurnLogRow>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare(
            "SELECT l.id, l.burn_id, l.ts, l.level, l.message
             FROM burn_logs l
             JOIN burn_jobs j ON j.id = l.burn_id
             WHERE j.job_id = ?1
             ORDER BY l.ts ASC, l.id ASC",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![job_id], |r| {
            Ok(BurnLogRow {
                id: r.get(0)?,
                burn_id: r.get(1)?,
                ts: r.get(2)?,
                level: r.get(3)?,
                message: r.get(4)?,
            })
        })
        .map_err(|e| e.to_string())?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

// ----------------------------------------------------------------------------
// Plain (non-Tauri-State) variants of the command bodies. The `#[tauri::command]`
// wrappers above take `State<'_, Db>`, which can't be constructed without a
// running Tauri app — too much ceremony for unit tests. These thin helpers
// hold the actual SQL so tests can exercise the same code paths against an
// in-memory `Db`.
#[cfg(test)]
fn config_get_impl(db: &Db, key: &str) -> Option<String> {
    let conn = db.0.lock().ok()?;
    conn.query_row("SELECT value FROM config WHERE key=?1", params![key], |r| {
        r.get::<_, String>(0)
    })
    .ok()
}

#[cfg(test)]
fn config_set_impl(db: &Db, key: &str, value: &str) -> Result<(), String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    conn.execute(
        "INSERT INTO config (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        params![key, value],
    )
    .map(|_| ())
    .map_err(|e| e.to_string())
}

#[cfg(test)]
fn config_all_impl(db: &Db) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let Ok(conn) = db.0.lock() else { return out };
    if let Ok(mut stmt) = conn.prepare("SELECT key, value FROM config") {
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)));
        if let Ok(rows) = rows {
            for kv in rows.flatten() {
                out.insert(kv.0, kv.1);
            }
        }
    }
    out
}

#[cfg(test)]
fn burn_jobs_list_impl(db: &Db, limit: Option<u32>) -> Result<Vec<BurnJob>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let lim = limit.unwrap_or(200) as i64;
    let sql = format!("SELECT {BURN_JOB_COLS} FROM burn_jobs ORDER BY queued_at DESC LIMIT ?1");
    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![lim], map_burn_job_row)
        .map_err(|e| e.to_string())?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

#[cfg(test)]
fn burn_jobs_active_impl(db: &Db) -> Result<Vec<BurnJob>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let sql = format!(
        "SELECT {BURN_JOB_COLS} FROM burn_jobs
         WHERE state <> 'success'
         ORDER BY queued_at ASC"
    );
    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], map_burn_job_row)
        .map_err(|e| e.to_string())?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

#[cfg(test)]
fn burn_jobs_clear_impl(db: &Db) -> Result<(), String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    conn.execute("DELETE FROM burn_jobs", [])
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
fn remove_burn_job_impl(db: &Db, job_id: &str) -> Result<(), String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    conn.execute("DELETE FROM burn_jobs WHERE job_id=?1", params![job_id])
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
fn burn_logs_list_impl(db: &Db, burn_id: i64) -> Result<Vec<BurnLogRow>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare(
            "SELECT id, burn_id, ts, level, message FROM burn_logs
             WHERE burn_id=?1 ORDER BY ts ASC, id ASC",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![burn_id], |r| {
            Ok(BurnLogRow {
                id: r.get(0)?,
                burn_id: r.get(1)?,
                ts: r.get(2)?,
                level: r.get(3)?,
                message: r.get(4)?,
            })
        })
        .map_err(|e| e.to_string())?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_db() -> Db {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        migrations::run(&mut conn).expect("migrations apply");
        Db(Mutex::new(conn))
    }

    // ---------- config_* ----------

    #[test]
    fn config_set_then_get_roundtrips() {
        let db = fresh_db();
        config_set_impl(&db, "hash.algo", "xxhash").unwrap();
        assert_eq!(
            config_get_impl(&db, "hash.algo"),
            Some("xxhash".to_string())
        );
    }

    #[test]
    fn config_set_overwrites_existing_value() {
        let db = fresh_db();
        config_set_impl(&db, "k", "v1").unwrap();
        config_set_impl(&db, "k", "v2").unwrap();
        assert_eq!(config_get_impl(&db, "k"), Some("v2".to_string()));
        let conn = db.0.lock().unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM config WHERE key='k'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn config_get_missing_key_returns_none() {
        let db = fresh_db();
        assert_eq!(config_get_impl(&db, "nope"), None);
    }

    #[test]
    fn config_all_returns_full_map() {
        let db = fresh_db();
        config_set_impl(&db, "a", "1").unwrap();
        config_set_impl(&db, "b", "2").unwrap();
        config_set_impl(&db, "c", "3").unwrap();
        let all = config_all_impl(&db);
        assert_eq!(all.len(), 3);
    }

    #[test]
    fn config_all_empty_when_no_rows() {
        let db = fresh_db();
        assert!(config_all_impl(&db).is_empty());
    }

    // ---------- burn lifecycle ----------

    #[test]
    fn record_burn_queued_inserts_queued_row_and_log() {
        let db = fresh_db();
        let id = record_burn_queued(&db, "job-1", "/tmp/x.iso", "x.iso", 12345, "/dev/disk5")
            .expect("insert returns id");
        let rows = burn_jobs_list_impl(&db, None).unwrap();
        assert_eq!(rows.len(), 1);
        let r = &rows[0];
        assert_eq!(r.id, id);
        assert_eq!(r.job_id, "job-1");
        assert_eq!(r.state, "queued");
        assert!(r.started_at.is_none());
        assert!(r.finished_at.is_none());
        assert!(r.progress_file.is_none());
        assert!(r.helper_pid.is_none());

        let logs = burn_logs_list_impl(&db, id).unwrap();
        assert_eq!(logs.len(), 1);
        assert!(logs[0].message.contains("queued"));
    }

    #[test]
    fn record_burn_queued_is_idempotent_for_open_rows() {
        let db = fresh_db();
        let id1 = record_burn_queued(&db, "job-q", "/p", "n", 1, "/dev/disk5").unwrap();
        let id2 = record_burn_queued(&db, "job-q", "/p", "n", 1, "/dev/disk5").unwrap();
        assert_eq!(id1, id2, "re-enqueue should return same row id");
        assert_eq!(burn_jobs_list_impl(&db, None).unwrap().len(), 1);
    }

    #[test]
    fn record_burn_started_flips_queued_to_running_and_stamps_helper_fields() {
        let db = fresh_db();
        let id = record_burn_queued(&db, "job-s", "/p", "n", 1, "/dev/disk5").unwrap();
        record_burn_started(&db, "job-s", Some("/tmp/p.jsonl"), Some(12345));
        let r = &burn_jobs_list_impl(&db, None).unwrap()[0];
        assert_eq!(r.id, id);
        assert_eq!(r.state, "running");
        assert!(r.started_at.is_some());
        assert_eq!(r.progress_file.as_deref(), Some("/tmp/p.jsonl"));
        assert_eq!(r.helper_pid, Some(12345));
    }

    #[test]
    fn record_burn_started_is_noop_without_queued_row() {
        let db = fresh_db();
        record_burn_started(&db, "ghost", None, None);
        assert!(burn_jobs_list_impl(&db, None).unwrap().is_empty());
    }

    #[test]
    fn record_burn_completed_marks_success() {
        let db = fresh_db();
        record_burn_queued(&db, "job-ok", "/p", "n", 100, "/dev/disk5").unwrap();
        record_burn_started(&db, "job-ok", None, None);
        record_burn_completed(&db, "job-ok", "src", "rb", true, 100, 1500, 1000, 2000);
        let r = &burn_jobs_list_impl(&db, None).unwrap()[0];
        assert_eq!(r.state, "success");
        assert_eq!(r.verify_match, Some(true));
        assert!(r.finished_at.is_some());
    }

    #[test]
    fn record_burn_completed_marks_error_on_hash_mismatch() {
        let db = fresh_db();
        record_burn_queued(&db, "job-mm", "/p", "n", 100, "/dev/disk5").unwrap();
        record_burn_started(&db, "job-mm", None, None);
        record_burn_completed(&db, "job-mm", "a", "b", false, 100, 100, 100, 100);
        let r = &burn_jobs_list_impl(&db, None).unwrap()[0];
        assert_eq!(r.state, "error");
        assert_eq!(r.error_code.as_deref(), Some("EHASHMISMATCH"));
    }

    #[test]
    fn record_burn_failed_marks_error_against_queued_or_running_row() {
        let db = fresh_db();
        record_burn_queued(&db, "job-pre", "/p", "n", 0, "/dev/disk5").unwrap();
        record_burn_failed(&db, "job-pre", "ENEEDS_FDA", "denied");
        let r = &burn_jobs_list_impl(&db, None).unwrap()[0];
        assert_eq!(r.state, "error");
        assert_eq!(r.error_code.as_deref(), Some("ENEEDS_FDA"));

        record_burn_queued(&db, "job-mid", "/p", "n", 0, "/dev/disk5").unwrap();
        record_burn_started(&db, "job-mid", None, None);
        record_burn_failed(&db, "job-mid", "EIO", "boom");
        let rows = burn_jobs_list_impl(&db, None).unwrap();
        let r = rows.iter().find(|r| r.job_id == "job-mid").unwrap();
        assert_eq!(r.state, "error");
    }

    #[test]
    fn record_burn_failed_with_cancel_code_marks_cancelled() {
        let db = fresh_db();
        record_burn_queued(&db, "job-c", "/p", "n", 0, "/dev/disk5").unwrap();
        record_burn_started(&db, "job-c", None, None);
        record_burn_failed(&db, "job-c", "ECANCELLED", "user");
        let r = &burn_jobs_list_impl(&db, None).unwrap()[0];
        assert_eq!(r.state, "cancelled");
    }

    #[test]
    fn record_burn_completed_is_noop_when_no_running_row() {
        let db = fresh_db();
        record_burn_completed(&db, "ghost", "s", "r", true, 0, 0, 0, 0);
        assert!(burn_jobs_list_impl(&db, None).unwrap().is_empty());
    }

    #[test]
    fn append_log_appends_for_open_job_only() {
        let db = fresh_db();
        let id = record_burn_queued(&db, "job-l", "/p", "n", 0, "/dev/disk5").unwrap();
        append_log(&db, "job-l", "warn", "yellow");
        append_log(&db, "no-such", "warn", "ignored");
        let logs = burn_logs_list_impl(&db, id).unwrap();
        // 1 queued log + 1 append.
        assert_eq!(logs.len(), 2);
        assert_eq!(logs[1].message, "yellow");
    }

    // ---------- list / clear / active ----------

    #[test]
    fn burn_jobs_list_orders_by_queued_at_desc() {
        let db = fresh_db();
        let conn = db.0.lock().unwrap();
        for (job, ts) in [("a", 1000_i64), ("b", 3000), ("c", 2000)] {
            conn.execute(
                "INSERT INTO burn_jobs (job_id, image_path, image_name, image_bytes,
                    target_device, state, queued_at)
                 VALUES (?1, '/p', 'n', 0, '/dev/disk5', 'queued', ?2)",
                params![job, ts],
            )
            .unwrap();
        }
        drop(conn);
        let rows = burn_jobs_list_impl(&db, None).unwrap();
        let job_ids: Vec<&str> = rows.iter().map(|r| r.job_id.as_str()).collect();
        assert_eq!(job_ids, vec!["b", "c", "a"]);
    }

    #[test]
    fn burn_jobs_active_excludes_success_and_orders_oldest_first() {
        let db = fresh_db();
        let conn = db.0.lock().unwrap();
        for (job, state, ts) in [
            ("q1", "queued", 1_i64),
            ("r1", "running", 2),
            ("ok", "success", 3),
            ("e1", "error", 4),
        ] {
            conn.execute(
                "INSERT INTO burn_jobs (job_id, image_path, image_name, image_bytes,
                    target_device, state, queued_at)
                 VALUES (?1, '/p', 'n', 0, '/dev/disk5', ?2, ?3)",
                params![job, state, ts],
            )
            .unwrap();
        }
        drop(conn);
        let rows = burn_jobs_active_impl(&db).unwrap();
        let ids: Vec<&str> = rows.iter().map(|r| r.job_id.as_str()).collect();
        assert_eq!(ids, vec!["q1", "r1", "e1"]);
    }

    #[test]
    fn burn_jobs_clear_empties_table_and_cascades_to_logs() {
        let db = fresh_db();
        let id = record_burn_queued(&db, "job-x", "/p", "n", 0, "/dev/disk5").unwrap();
        record_burn_started(&db, "job-x", None, None);
        record_burn_completed(&db, "job-x", "s", "r", true, 1, 1, 1, 1);
        assert_eq!(burn_jobs_list_impl(&db, None).unwrap().len(), 1);
        assert!(!burn_logs_list_impl(&db, id).unwrap().is_empty());
        burn_jobs_clear_impl(&db).unwrap();
        assert!(burn_jobs_list_impl(&db, None).unwrap().is_empty());
        assert!(burn_logs_list_impl(&db, id).unwrap().is_empty());
    }

    #[test]
    fn remove_burn_job_deletes_row_and_cascades_logs() {
        let db = fresh_db();
        let id = record_burn_queued(&db, "job-rm", "/p", "n", 0, "/dev/disk5").unwrap();
        assert!(!burn_logs_list_impl(&db, id).unwrap().is_empty());
        remove_burn_job_impl(&db, "job-rm").unwrap();
        assert!(burn_jobs_list_impl(&db, None).unwrap().is_empty());
        assert!(burn_logs_list_impl(&db, id).unwrap().is_empty());
    }

    #[test]
    fn burn_logs_list_filters_by_burn_id_and_orders_ascending() {
        let db = fresh_db();
        let id1 = record_burn_queued(&db, "j1", "/p", "n", 0, "/dev/disk5").unwrap();
        let id2 = record_burn_queued(&db, "j2", "/p", "n", 0, "/dev/disk6").unwrap();
        append_log(&db, "j1", "info", "one");
        append_log(&db, "j1", "info", "two");
        append_log(&db, "j2", "warn", "other");
        let l1 = burn_logs_list_impl(&db, id1).unwrap();
        let l2 = burn_logs_list_impl(&db, id2).unwrap();
        assert!(l1.iter().all(|r| r.burn_id == id1));
        assert!(l2.iter().all(|r| r.burn_id == id2));
        let ids: Vec<i64> = l1.iter().map(|r| r.id).collect();
        let mut sorted = ids.clone();
        sorted.sort();
        assert_eq!(ids, sorted);
    }

    #[test]
    fn burn_logs_list_for_unknown_burn_id_is_empty() {
        let db = fresh_db();
        assert!(burn_logs_list_impl(&db, 9_999_999).unwrap().is_empty());
    }
}
