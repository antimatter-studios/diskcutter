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
    /// JSON snapshot of the user-tunable write knobs that fed into
    /// this burn (writer impl, chunk size, worker count, etc.).
    /// `None` for rows written before 0002_burn_params landed.
    pub burn_params: Option<String>,
}

/// Every config key whose value would change the bytes-on-the-wire
/// behaviour of a burn. Snapshotted into `burn_history.burn_params` at
/// the start of every write so post-mortems can answer "which knobs
/// were set when this image was burned?". Adding a new write-related
/// knob means adding its key here; the column itself is a free-form
/// JSON blob so the migration doesn't need to change.
pub const BURN_PARAM_KEYS: &[&str] = &[
    "writer.impl",
    "chunk.bytes",
    "workers.count",
    "queue.depth",
    "verify.skip",
    "hash.algo",
    "max.mismatches",
    "auto.eject",
];

/// Build the JSON snapshot of every `BURN_PARAM_KEYS` value currently
/// in `config`. Missing keys are omitted (rather than serialised as
/// nulls) so the JSON only carries values the operator actually set.
pub fn collect_burn_params(db: &Db) -> String {
    let Ok(conn) = db.0.lock() else {
        return "{}".into();
    };
    let mut map = serde_json::Map::new();
    for key in BURN_PARAM_KEYS {
        if let Ok(val) = conn.query_row(
            "SELECT value FROM config WHERE key = ?1",
            params![*key],
            |r| r.get::<_, String>(0),
        ) {
            if !val.is_empty() {
                map.insert((*key).into(), serde_json::Value::String(val));
            }
        }
    }
    serde_json::Value::Object(map).to_string()
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
    burn_params: &str,
) -> Option<i64> {
    let conn = db.0.lock().ok()?;
    conn.execute(
        "INSERT INTO burn_history (job_id, image_path, image_name, image_bytes,
            target_device, state, started_at, burn_params)
         VALUES (?1, ?2, ?3, ?4, ?5, 'running', ?6, ?7)",
        params![
            job_id,
            image_path,
            image_name,
            image_bytes as i64,
            target_device,
            now_ms(),
            burn_params,
        ],
    )
    .ok()?;
    let id = conn.last_insert_rowid();
    let kickoff_message = if burn_params.is_empty() || burn_params == "{}" {
        format!("burn started: {image_name} → {target_device}")
    } else {
        // Inline the params snapshot into the kickoff line so a single
        // SELECT on burn_logs shows what the run was started with even
        // without joining burn_history.burn_params. Kept on the same
        // row to avoid doubling burn_logs storage on every burn.
        format!("burn started: {image_name} → {target_device} [params: {burn_params}]")
    };
    let _ = conn.execute(
        "INSERT INTO burn_logs (burn_id, ts, level, message) VALUES (?1, ?2, 'info', ?3)",
        params![id, now_ms(), kickoff_message],
    );
    Some(id)
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

#[allow(dead_code)]
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

#[tauri::command]
pub fn burn_history_list(db: State<'_, Db>, limit: Option<u32>) -> Result<Vec<BurnRecord>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let lim = limit.unwrap_or(200) as i64;
    let mut stmt = conn
        .prepare(
            "SELECT id, job_id, image_path, image_name, image_bytes, target_device,
                    source_sha256, readback_sha256, verify_match,
                    bytes_written, elapsed_ms, avg_write_bps, avg_verify_bps,
                    state, error_code, error_message, started_at, finished_at,
                    burn_params
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
                burn_params: r.get(18)?,
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

// ----------------------------------------------------------------------------
// Plain (non-Tauri-State) variants of the command bodies. The `#[tauri::command]`
// wrappers above take `State<'_, Db>`, which can't be constructed without a
// running Tauri app — too much ceremony for unit tests. These thin helpers
// hold the actual SQL so tests can exercise the same code paths against an
// in-memory `Db`. The command wrappers can be migrated to call into these in
// a later refactor; for now they just keep the SQL in one logical place that
// the tests can reach.
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
fn burn_history_list_impl(db: &Db, limit: Option<u32>) -> Result<Vec<BurnRecord>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let lim = limit.unwrap_or(200) as i64;
    let mut stmt = conn
        .prepare(
            "SELECT id, job_id, image_path, image_name, image_bytes, target_device,
                    source_sha256, readback_sha256, verify_match,
                    bytes_written, elapsed_ms, avg_write_bps, avg_verify_bps,
                    state, error_code, error_message, started_at, finished_at,
                    burn_params
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
                burn_params: r.get(18)?,
            })
        })
        .map_err(|e| e.to_string())?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

#[cfg(test)]
fn burn_history_clear_impl(db: &Db) -> Result<(), String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    conn.execute("DELETE FROM burn_history", [])
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
        // And only one row exists for that key.
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
        assert_eq!(all.get("a"), Some(&"1".to_string()));
        assert_eq!(all.get("b"), Some(&"2".to_string()));
        assert_eq!(all.get("c"), Some(&"3".to_string()));
    }

    #[test]
    fn config_all_empty_when_no_rows() {
        let db = fresh_db();
        assert!(config_all_impl(&db).is_empty());
    }

    // ---------- burn-params snapshot ----------

    #[test]
    fn collect_burn_params_returns_empty_object_when_no_prefs_set() {
        let db = fresh_db();
        // No config rows → JSON shape is the empty object, not "null".
        assert_eq!(collect_burn_params(&db), "{}");
    }

    #[test]
    fn collect_burn_params_emits_only_write_relevant_keys() {
        let db = fresh_db();
        // language, theme, density are non-burn knobs; only the
        // write-relevant ones (BURN_PARAM_KEYS) should appear in the
        // snapshot. Stuff that landed empty stays out, too.
        config_set_impl(&db, "language", "en").unwrap();
        config_set_impl(&db, "theme", "dark").unwrap();
        config_set_impl(&db, "writer.impl", "pipelined").unwrap();
        config_set_impl(&db, "chunk.bytes", "1048576").unwrap();
        config_set_impl(&db, "verify.skip", "").unwrap();
        let json = collect_burn_params(&db);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        let obj = v.as_object().unwrap();
        assert!(obj.contains_key("writer.impl"));
        assert!(obj.contains_key("chunk.bytes"));
        assert!(!obj.contains_key("language"));
        assert!(!obj.contains_key("theme"));
        assert!(
            !obj.contains_key("verify.skip"),
            "empty-string values stay out"
        );
    }

    #[test]
    fn record_burn_started_persists_burn_params_column() {
        let db = fresh_db();
        let snapshot = r#"{"writer.impl":"raw","chunk.bytes":"262144"}"#;
        record_burn_started(
            &db,
            "job-p",
            "/tmp/x.iso",
            "x.iso",
            0,
            "/dev/disk5",
            snapshot,
        )
        .unwrap();
        let rows = burn_history_list_impl(&db, None).unwrap();
        assert_eq!(rows[0].burn_params.as_deref(), Some(snapshot));
    }

    // ---------- burn lifecycle helpers ----------

    #[test]
    fn record_burn_started_inserts_running_row_and_log() {
        let db = fresh_db();
        let id = record_burn_started(
            &db,
            "job-1",
            "/tmp/x.iso",
            "x.iso",
            12345,
            "/dev/disk5",
            "{}",
        )
        .expect("insert returns id");
        let rows = burn_history_list_impl(&db, None).unwrap();
        assert_eq!(rows.len(), 1);
        let r = &rows[0];
        assert_eq!(r.id, id);
        assert_eq!(r.job_id, "job-1");
        assert_eq!(r.image_path, "/tmp/x.iso");
        assert_eq!(r.image_name, "x.iso");
        assert_eq!(r.image_bytes, 12345);
        assert_eq!(r.target_device, "/dev/disk5");
        assert_eq!(r.state, "running");
        assert!(r.finished_at.is_none());

        // The starter helper drops a kickoff line into burn_logs.
        let logs = burn_logs_list_impl(&db, id).unwrap();
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].level, "info");
        assert!(logs[0].message.contains("x.iso"));
    }

    #[test]
    fn record_burn_completed_marks_success_and_appends_log() {
        let db = fresh_db();
        let id = record_burn_started(
            &db,
            "job-ok",
            "/tmp/x.iso",
            "x.iso",
            100,
            "/dev/disk5",
            "{}",
        )
        .unwrap();
        record_burn_completed(
            &db, "job-ok", "src-hash", "rb-hash", true, 100, 1500, 1000, 2000,
        );
        let rows = burn_history_list_impl(&db, None).unwrap();
        assert_eq!(rows.len(), 1);
        let r = &rows[0];
        assert_eq!(r.state, "success");
        assert_eq!(r.source_sha256.as_deref(), Some("src-hash"));
        assert_eq!(r.readback_sha256.as_deref(), Some("rb-hash"));
        assert_eq!(r.verify_match, Some(true));
        assert_eq!(r.bytes_written, Some(100));
        assert_eq!(r.elapsed_ms, Some(1500));
        assert_eq!(r.avg_write_bps, Some(1000));
        assert_eq!(r.avg_verify_bps, Some(2000));
        assert!(r.error_code.is_none());
        assert!(r.finished_at.is_some());

        // One starter log + one completion log.
        let logs = burn_logs_list_impl(&db, id).unwrap();
        assert_eq!(logs.len(), 2);
        assert_eq!(logs[1].level, "info");
        assert!(logs[1].message.contains("verify_match=true"));
    }

    #[test]
    fn record_burn_completed_marks_error_on_hash_mismatch() {
        let db = fresh_db();
        let id = record_burn_started(
            &db,
            "job-mm",
            "/tmp/x.iso",
            "x.iso",
            100,
            "/dev/disk5",
            "{}",
        )
        .unwrap();
        record_burn_completed(&db, "job-mm", "a", "b", false, 100, 100, 100, 100);
        let rows = burn_history_list_impl(&db, None).unwrap();
        let r = &rows[0];
        assert_eq!(r.state, "error");
        assert_eq!(r.error_code.as_deref(), Some("EHASHMISMATCH"));
        assert_eq!(r.verify_match, Some(false));
        let logs = burn_logs_list_impl(&db, id).unwrap();
        // The mismatch log entry is logged at error level.
        assert_eq!(logs.last().unwrap().level, "error");
    }

    #[test]
    fn record_burn_failed_marks_error_and_logs() {
        let db = fresh_db();
        let id = record_burn_started(
            &db,
            "job-fail",
            "/tmp/x.iso",
            "x.iso",
            0,
            "/dev/disk5",
            "{}",
        )
        .unwrap();
        record_burn_failed(&db, "job-fail", "EIO", "boom");
        let r = &burn_history_list_impl(&db, None).unwrap()[0];
        assert_eq!(r.state, "error");
        assert_eq!(r.error_code.as_deref(), Some("EIO"));
        assert_eq!(r.error_message.as_deref(), Some("boom"));
        assert!(r.finished_at.is_some());
        let logs = burn_logs_list_impl(&db, id).unwrap();
        assert!(logs.last().unwrap().message.contains("EIO: boom"));
    }

    #[test]
    fn record_burn_failed_with_cancel_code_marks_cancelled_state() {
        let db = fresh_db();
        record_burn_started(&db, "job-c", "/tmp/x.iso", "x.iso", 0, "/dev/disk5", "{}").unwrap();
        record_burn_failed(&db, "job-c", "ECANCELLED", "user");
        let r = &burn_history_list_impl(&db, None).unwrap()[0];
        assert_eq!(r.state, "cancelled");
        assert_eq!(r.error_code.as_deref(), Some("ECANCELLED"));
    }

    #[test]
    fn record_burn_completed_is_noop_when_no_running_row() {
        let db = fresh_db();
        // No started_burn → completed should silently do nothing.
        record_burn_completed(&db, "ghost", "s", "r", true, 0, 0, 0, 0);
        assert!(burn_history_list_impl(&db, None).unwrap().is_empty());
    }

    #[test]
    fn append_log_appends_for_running_job_only() {
        let db = fresh_db();
        let id = record_burn_started(&db, "job-l", "/tmp/x.iso", "x.iso", 0, "/dev/disk5", "{}")
            .unwrap();
        append_log(&db, "job-l", "warn", "yellow");
        // No-op against an unknown job_id.
        append_log(&db, "no-such", "warn", "ignored");
        let logs = burn_logs_list_impl(&db, id).unwrap();
        assert_eq!(logs.len(), 2);
        assert_eq!(logs[1].level, "warn");
        assert_eq!(logs[1].message, "yellow");
    }

    // ---------- burn_history_list ordering / clear / logs filter ----------

    #[test]
    fn burn_history_list_orders_by_started_at_desc() {
        let db = fresh_db();
        // Insert three rows with explicit started_at to make ordering deterministic.
        let conn = db.0.lock().unwrap();
        for (job, ts) in [("a", 1000_i64), ("b", 3000), ("c", 2000)] {
            conn.execute(
                "INSERT INTO burn_history (job_id, image_path, image_name, image_bytes,
                    target_device, state, started_at)
                 VALUES (?1, '/p', 'n', 0, '/dev/disk5', 'running', ?2)",
                params![job, ts],
            )
            .unwrap();
        }
        drop(conn);
        let rows = burn_history_list_impl(&db, None).unwrap();
        let job_ids: Vec<&str> = rows.iter().map(|r| r.job_id.as_str()).collect();
        assert_eq!(job_ids, vec!["b", "c", "a"]);
    }

    #[test]
    fn burn_history_list_respects_limit() {
        let db = fresh_db();
        for i in 0..5 {
            record_burn_started(
                &db,
                &format!("job-{i}"),
                "/tmp/x.iso",
                "x.iso",
                0,
                "/dev/disk5",
                "{}",
            )
            .unwrap();
        }
        let rows = burn_history_list_impl(&db, Some(2)).unwrap();
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn burn_history_clear_empties_table_and_cascades_to_logs() {
        let db = fresh_db();
        let id = record_burn_started(&db, "job-x", "/tmp/x.iso", "x.iso", 0, "/dev/disk5", "{}")
            .unwrap();
        record_burn_completed(&db, "job-x", "s", "r", true, 1, 1, 1, 1);
        assert_eq!(burn_history_list_impl(&db, None).unwrap().len(), 1);
        assert!(!burn_logs_list_impl(&db, id).unwrap().is_empty());

        burn_history_clear_impl(&db).unwrap();
        assert!(burn_history_list_impl(&db, None).unwrap().is_empty());
        // burn_logs rows should have been removed too, either via ON DELETE CASCADE
        // or because the parent FK is gone. Either way the consumer view is empty.
        assert!(burn_logs_list_impl(&db, id).unwrap().is_empty());
    }

    #[test]
    fn burn_logs_list_filters_by_burn_id_and_orders_ascending() {
        let db = fresh_db();
        let id1 =
            record_burn_started(&db, "j1", "/tmp/x.iso", "x.iso", 0, "/dev/disk5", "{}").unwrap();
        let id2 =
            record_burn_started(&db, "j2", "/tmp/y.iso", "y.iso", 0, "/dev/disk6", "{}").unwrap();
        append_log(&db, "j1", "info", "one");
        append_log(&db, "j1", "info", "two");
        append_log(&db, "j2", "warn", "other");

        let l1 = burn_logs_list_impl(&db, id1).unwrap();
        let l2 = burn_logs_list_impl(&db, id2).unwrap();
        assert!(l1.iter().all(|r| r.burn_id == id1));
        assert!(l2.iter().all(|r| r.burn_id == id2));
        // Ordering is ts ASC, id ASC — inserts are monotonic so ids increase.
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
