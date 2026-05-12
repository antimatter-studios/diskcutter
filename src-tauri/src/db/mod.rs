mod migrations;

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

#[derive(Serialize, Clone)]
pub struct BurnRecord {
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
    pub started_at: i64,
    pub finished_at: Option<i64>,
}

#[derive(Serialize, Clone)]
pub struct BurnLogRow {
    pub id: i64,
    pub burn_id: i64,
    pub ts: i64,
    pub level: String,
    pub message: String,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn find_running_id(conn: &Connection, job_id: &str) -> Option<i64> {
    conn.query_row(
        "SELECT id FROM burn_history
         WHERE job_id = ?1 AND state = 'running'
         ORDER BY started_at DESC LIMIT 1",
        params![job_id],
        |r| r.get::<_, i64>(0),
    )
    .ok()
}

pub fn record_burn_started(
    db: &Db,
    job_id: &str,
    image_path: &str,
    image_name: &str,
    image_bytes: u64,
    target_device: &str,
) -> Option<i64> {
    let conn = db.0.lock().ok()?;
    conn.execute(
        "INSERT INTO burn_history (job_id, image_path, image_name, image_bytes,
            target_device, state, started_at)
         VALUES (?1, ?2, ?3, ?4, ?5, 'running', ?6)",
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
            format!("burn started: {image_name} → {target_device}")
        ],
    );
    Some(id)
}

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
        "UPDATE burn_history
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
    let Some(id) = find_running_id(&conn, job_id) else {
        return;
    };
    let state = if code == "ECANCELLED" {
        "cancelled"
    } else {
        "error"
    };
    let _ = conn.execute(
        "UPDATE burn_history
         SET state=?1, error_code=?2, error_message=?3, finished_at=?4
         WHERE id=?5",
        params![state, code, message, now_ms(), id],
    );
    let _ = conn.execute(
        "INSERT INTO burn_logs (burn_id, ts, level, message) VALUES (?1, ?2, 'error', ?3)",
        params![id, now_ms(), format!("{code}: {message}")],
    );
}

pub fn append_log(db: &Db, job_id: &str, level: &str, message: &str) {
    let conn = match db.0.lock() {
        Ok(c) => c,
        Err(_) => return,
    };
    if let Some(id) = find_running_id(&conn, job_id) {
        let _ = conn.execute(
            "INSERT INTO burn_logs (burn_id, ts, level, message) VALUES (?1, ?2, ?3, ?4)",
            params![id, now_ms(), level, message],
        );
    }
}

#[tauri::command]
pub fn config_get(db: State<'_, Db>, key: String) -> Option<String> {
    let conn = db.0.lock().ok()?;
    conn.query_row(
        "SELECT value FROM config WHERE key=?1",
        params![key],
        |r| r.get::<_, String>(0),
    )
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

#[tauri::command]
pub fn burn_history_list(
    db: State<'_, Db>,
    limit: Option<u32>,
) -> Result<Vec<BurnRecord>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let lim = limit.unwrap_or(200) as i64;
    let mut stmt = conn
        .prepare(
            "SELECT id, job_id, image_path, image_name, image_bytes, target_device,
                    source_sha256, readback_sha256, verify_match,
                    bytes_written, elapsed_ms, avg_write_bps, avg_verify_bps,
                    state, error_code, error_message, started_at, finished_at
             FROM burn_history ORDER BY started_at DESC LIMIT ?1",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![lim], |r| {
            Ok(BurnRecord {
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
                started_at: r.get(16)?,
                finished_at: r.get(17)?,
            })
        })
        .map_err(|e| e.to_string())?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

#[tauri::command]
pub fn burn_history_clear(db: State<'_, Db>) -> Result<(), String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    conn.execute("DELETE FROM burn_history", [])
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
