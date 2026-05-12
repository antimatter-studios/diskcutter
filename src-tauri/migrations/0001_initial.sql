-- 0001_initial.sql
-- Creates the initial schema: config (key/value), burn_history (one row per
-- write attempt), burn_logs (per-burn event stream, FK → burn_history).

CREATE TABLE config (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
);

CREATE TABLE burn_history (
  id              INTEGER PRIMARY KEY AUTOINCREMENT,
  job_id          TEXT    NOT NULL,
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
  started_at      INTEGER NOT NULL,
  finished_at     INTEGER
);

CREATE INDEX idx_burn_history_job_id  ON burn_history(job_id);
CREATE INDEX idx_burn_history_started ON burn_history(started_at DESC);

CREATE TABLE burn_logs (
  id      INTEGER PRIMARY KEY AUTOINCREMENT,
  burn_id INTEGER NOT NULL REFERENCES burn_history(id) ON DELETE CASCADE,
  ts      INTEGER NOT NULL,
  level   TEXT    NOT NULL,
  message TEXT    NOT NULL
);

CREATE INDEX idx_burn_logs_burn_id ON burn_logs(burn_id, ts);
