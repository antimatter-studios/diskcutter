use rusqlite::{params, Connection};

pub struct Migration {
    pub version: u32,
    pub name: &'static str,
    pub sql: &'static str,
}

// Generated at build time by build.rs from src-tauri/migrations/*.sql.
include!(concat!(env!("OUT_DIR"), "/migrations.gen.rs"));

const BOOTSTRAP: &str = r#"
CREATE TABLE IF NOT EXISTS schema_migrations (
  version    INTEGER PRIMARY KEY,
  name       TEXT    NOT NULL,
  applied_at INTEGER NOT NULL
);
"#;

// Smoke-test queries. A migration is "successful" the moment its SQL runs
// without raising — but a bad migration can still leave the DB internally
// inconsistent (corrupt indexes, dangling FKs, missing tables we *thought*
// the migration created). These checks run after every successful migration
// pass and fail loudly so a broken upgrade aborts startup instead of
// limping forward with garbage state.
//
// Each entry: (label, sql). The sql must be a single statement that either
// errors or returns a row matching the expected predicate in `run_health`.
const REQUIRED_TABLES: &[&str] = &["config", "burn_jobs", "burn_logs", "schema_migrations"];

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn current_version(conn: &Connection) -> rusqlite::Result<u32> {
    let v: i64 = conn.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
        [],
        |r| r.get(0),
    )?;
    Ok(v as u32)
}

pub fn run(conn: &mut Connection) -> rusqlite::Result<()> {
    conn.execute_batch(BOOTSTRAP)?;
    let cur = current_version(conn)?;
    for m in MIGRATIONS {
        if m.version <= cur {
            continue;
        }
        let tx = conn.transaction()?;
        tx.execute_batch(m.sql)?;
        tx.execute(
            "INSERT INTO schema_migrations (version, name, applied_at) VALUES (?1, ?2, ?3)",
            params![m.version, m.name, now_ms()],
        )?;
        tx.commit()?;
    }
    health_check(conn)?;
    Ok(())
}

// Post-migration sanity pass. Each check is cheap enough to run on every
// startup. Anything that fails turns into a rusqlite error so `db::open`
// propagates it and the app refuses to start with a broken DB.
fn health_check(conn: &Connection) -> rusqlite::Result<()> {
    // 1. SQLite's own integrity check — catches page corruption, broken
    //    indexes, malformed b-trees. Returns the single string "ok" on
    //    success, otherwise a list of problems.
    let integrity: String = conn.query_row("PRAGMA integrity_check;", [], |r| r.get(0))?;
    if integrity != "ok" {
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CORRUPT),
            Some(format!("integrity_check failed: {integrity}")),
        ));
    }

    // 2. Foreign-key check — returns one row per FK violation. Required
    //    because PRAGMA foreign_keys=ON only enforces *new* writes; an FK
    //    relation invented by a migration can still leave orphans behind.
    let mut fk_stmt = conn.prepare("PRAGMA foreign_key_check;")?;
    let mut fk_rows = fk_stmt.query([])?;
    if fk_rows.next()?.is_some() {
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT_FOREIGNKEY),
            Some("foreign_key_check found dangling rows".into()),
        ));
    }

    // 3. Every table the runtime code reads/writes must exist AND be
    //    queryable — a CREATE TABLE that landed in sqlite_master but is
    //    corrupted on disk would explode on first SELECT, not on existence
    //    check, so we touch each one.
    for table in REQUIRED_TABLES {
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
            params![*table],
            |r| r.get(0),
        )?;
        if n != 1 {
            return Err(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ERROR),
                Some(format!("required table missing after migrations: {table}")),
            ));
        }
        // Probe queryability. `SELECT 1 FROM <t> LIMIT 1` exercises the
        // table's b-tree even when empty.
        let probe = format!("SELECT 1 FROM {table} LIMIT 1");
        let mut stmt = conn.prepare(&probe)?;
        let _ = stmt.query([])?.next()?;
    }

    // 4. schema_migrations must report the version we just installed —
    //    a migration that crashed mid-tx and rolled back without raising
    //    would leave us with stale state.
    let head = current_version(conn)?;
    let expected = MIGRATIONS.last().map(|m| m.version).unwrap_or(0);
    if head != expected {
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ERROR),
            Some(format!(
                "schema_migrations head is {head}, expected {expected}"
            )),
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_set_is_sequential_and_nonempty() {
        assert!(!MIGRATIONS.is_empty(), "no migrations were bundled");
        for (i, m) in MIGRATIONS.iter().enumerate() {
            assert_eq!(
                m.version,
                (i as u32) + 1,
                "migration at index {i} has version {} (expected {})",
                m.version,
                i + 1
            );
            assert!(!m.name.is_empty(), "migration {} has empty name", m.version);
            assert!(
                !m.sql.trim().is_empty(),
                "migration {} has empty sql",
                m.version
            );
        }
    }

    // Build-time guarantee that every migration's SQL applies cleanly against
    // an empty SQLite. Catches typos and broken FK references before ship.
    #[test]
    fn migrations_apply_to_empty_database() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        run(&mut conn).expect("migrations apply cleanly");

        let applied: u32 = conn
            .query_row("SELECT COUNT(*) FROM schema_migrations", [], |r| {
                r.get::<_, i64>(0).map(|v| v as u32)
            })
            .unwrap();
        assert_eq!(applied as usize, MIGRATIONS.len());

        // Re-running is a no-op.
        run(&mut conn).expect("re-run is idempotent");
        let applied2: u32 = conn
            .query_row("SELECT COUNT(*) FROM schema_migrations", [], |r| {
                r.get::<_, i64>(0).map(|v| v as u32)
            })
            .unwrap();
        assert_eq!(applied2, applied);
    }

    #[test]
    fn migrations_produce_expected_tables() {
        let mut conn = Connection::open_in_memory().unwrap();
        run(&mut conn).expect("migrations apply");
        for table in ["config", "burn_jobs", "burn_logs"] {
            let n: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                    params![table],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 1, "table {table} missing after migrations");
        }
        // burn_history should be gone after 0002.
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='burn_history'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0, "burn_history should have been renamed");
    }

    // burn_history → burn_jobs migration: pre-load 0001 rows, run 0002,
    // confirm rows survive with their old started_at remapped to
    // queued_at and the new columns present and NULL.
    #[test]
    fn burn_history_rows_migrate_into_burn_jobs() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        let m0001 = MIGRATIONS
            .iter()
            .find(|m| m.version == 1)
            .expect("0001 migration present");
        conn.execute_batch(BOOTSTRAP).unwrap();
        conn.execute_batch(m0001.sql).unwrap();
        conn.execute(
            "INSERT INTO burn_history (job_id, image_path, image_name, image_bytes,
                target_device, state, started_at)
             VALUES ('legacy', '/tmp/x.iso', 'x.iso', 100, '/dev/disk5', 'success', 1715)",
            [],
        )
        .unwrap();
        // Stamp 0001 as applied so run() picks up at 0002.
        conn.execute(
            "INSERT INTO schema_migrations (version, name, applied_at) VALUES (1, ?1, 0)",
            params![m0001.name],
        )
        .unwrap();
        run(&mut conn).expect("0002 applies on populated 0001 schema");
        let (job_id, queued_at, started_at, progress_file, helper_pid): (
            String,
            i64,
            Option<i64>,
            Option<String>,
            Option<i64>,
        ) = conn
            .query_row(
                "SELECT job_id, queued_at, started_at, progress_file, helper_pid
                 FROM burn_jobs WHERE job_id='legacy'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert_eq!(job_id, "legacy");
        assert_eq!(queued_at, 1715, "old started_at should become queued_at");
        assert!(started_at.is_none());
        assert!(progress_file.is_none());
        assert!(helper_pid.is_none());
    }

    #[test]
    fn health_check_passes_on_clean_db() {
        let mut conn = Connection::open_in_memory().unwrap();
        run(&mut conn).expect("migrations + health check pass");
        // Direct re-invocation should also succeed.
        super::health_check(&conn).expect("standalone health_check passes");
    }

    #[test]
    fn health_check_rejects_missing_required_table() {
        let mut conn = Connection::open_in_memory().unwrap();
        run(&mut conn).unwrap();
        // Simulate damage: drop a table the runtime depends on.
        conn.execute_batch("DROP TABLE burn_logs;").unwrap();
        let err = super::health_check(&conn).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("burn_logs"),
            "expected missing-table error to name burn_logs, got: {msg}"
        );
    }

    #[test]
    fn health_check_rejects_dangling_foreign_keys() {
        let mut conn = Connection::open_in_memory().unwrap();
        // FK enforcement OFF so we can fabricate an orphan row. Mirrors the
        // hazard of a migration that backfilled rows without their parent.
        conn.execute_batch("PRAGMA foreign_keys=OFF;").unwrap();
        run(&mut conn).unwrap();
        conn.execute(
            "INSERT INTO burn_logs (burn_id, ts, level, message)
             VALUES (9999, 0, 'info', 'orphan')",
            [],
        )
        .unwrap();
        let err = super::health_check(&conn).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("foreign_key_check"),
            "expected FK violation, got: {msg}"
        );
    }
}
