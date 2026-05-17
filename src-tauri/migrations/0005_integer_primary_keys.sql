-- 0005_integer_primary_keys.sql
-- Make every "row identifier" a database-assigned integer primary
-- key. burn_jobs.job_id was previously a TEXT column the frontend
-- invented (`job-<timestamp>-<counter>`) — replaced with
-- `job_id INTEGER PRIMARY KEY AUTOINCREMENT`. burn_logs.burn_id is
-- renamed `job_id` to match its referent. config drops `key TEXT
-- PRIMARY KEY` in favour of `config_id INTEGER PRIMARY KEY` with a
-- UNIQUE constraint on `key`.
--
-- SQLite can't ALTER PRIMARY KEY directly; the standard workaround is
-- the recreate-and-copy dance. `defer_foreign_keys = 1` lets the
-- intermediate burn_logs → old-burn_jobs FK live briefly within the
-- transaction; burn_logs is recreated against the new burn_jobs
-- before commit, so the FK check passes.

PRAGMA defer_foreign_keys = 1;

-- 1. Rebuild burn_jobs with integer job_id PK. Copy the existing
--    integer `id` values into the new job_id so burn_logs.burn_id
--    (which referenced old burn_jobs.id) still resolves after the
--    recreate.
CREATE TABLE burn_jobs_new (
  job_id          INTEGER PRIMARY KEY AUTOINCREMENT,
  image_path      TEXT    NOT NULL,
  image_name      TEXT    NOT NULL,
  image_bytes     INTEGER NOT NULL,
  target_device   TEXT    NOT NULL,
  source_sha256   TEXT,
  readback_sha256 TEXT,
  verify_match    INTEGER,
  bytes_written   INTEGER,
  elapsed_ms      INTEGER,
  avg_write_bps   INTEGER,
  avg_verify_bps  INTEGER,
  state           TEXT    NOT NULL,
  error_code      TEXT,
  error_message   TEXT,
  queued_at       INTEGER NOT NULL,
  started_at      INTEGER,
  finished_at     INTEGER,
  progress_file   TEXT,
  helper_pid      INTEGER
);

INSERT INTO burn_jobs_new (
  job_id, image_path, image_name, image_bytes, target_device,
  source_sha256, readback_sha256, verify_match,
  bytes_written, elapsed_ms, avg_write_bps, avg_verify_bps,
  state, error_code, error_message,
  queued_at, started_at, finished_at,
  progress_file, helper_pid
)
SELECT
  id, image_path, image_name, image_bytes, target_device,
  source_sha256, readback_sha256, verify_match,
  bytes_written, elapsed_ms, avg_write_bps, avg_verify_bps,
  state, error_code, error_message,
  queued_at, started_at, finished_at,
  progress_file, helper_pid
FROM burn_jobs;

DROP TABLE burn_jobs;
ALTER TABLE burn_jobs_new RENAME TO burn_jobs;

CREATE INDEX idx_burn_jobs_state  ON burn_jobs(state);
CREATE INDEX idx_burn_jobs_queued ON burn_jobs(queued_at);

INSERT OR REPLACE INTO sqlite_sequence(name, seq)
SELECT 'burn_jobs', COALESCE(MAX(job_id), 0) FROM burn_jobs;

-- 2. Rebuild burn_logs: rename burn_id → job_id, FK → burn_jobs(job_id).
CREATE TABLE burn_logs_new (
  id      INTEGER PRIMARY KEY AUTOINCREMENT,
  job_id  INTEGER NOT NULL REFERENCES burn_jobs(job_id) ON DELETE CASCADE,
  ts      INTEGER NOT NULL,
  level   TEXT    NOT NULL,
  message TEXT    NOT NULL
);

INSERT INTO burn_logs_new (id, job_id, ts, level, message)
SELECT id, burn_id, ts, level, message FROM burn_logs;

DROP TABLE burn_logs;
ALTER TABLE burn_logs_new RENAME TO burn_logs;

CREATE INDEX idx_burn_logs_job_id ON burn_logs(job_id, ts);

INSERT OR REPLACE INTO sqlite_sequence(name, seq)
SELECT 'burn_logs', COALESCE(MAX(id), 0) FROM burn_logs;

-- 3. Rebuild config with integer PK + UNIQUE key.
CREATE TABLE config_new (
  config_id INTEGER PRIMARY KEY AUTOINCREMENT,
  key       TEXT    NOT NULL UNIQUE,
  value     TEXT    NOT NULL
);

INSERT INTO config_new (key, value) SELECT key, value FROM config;

DROP TABLE config;
ALTER TABLE config_new RENAME TO config;

INSERT OR REPLACE INTO sqlite_sequence(name, seq)
SELECT 'config', COALESCE(MAX(config_id), 0) FROM config;
