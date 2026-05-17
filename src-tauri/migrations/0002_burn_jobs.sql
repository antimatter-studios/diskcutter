-- 0002_burn_jobs.sql
-- Rename burn_history → burn_jobs. The table now holds every queued job
-- regardless of state (queued | running | success | error | cancelled),
-- so it doubles as live-queue and history. Adds columns needed to
-- reattach the UI to a still-running helper after the parent app
-- restarts: progress_file (path to the JSONL IPC sink), helper_pid (the
-- osascript pid we spawned). started_at becomes nullable because rows
-- now exist before the helper is launched; queued_at is the new
-- monotonic ordering key.

ALTER TABLE burn_history RENAME TO burn_jobs;

-- started_at -> queued_at, then add a fresh nullable started_at. SQLite
-- forbids dropping NOT NULL via ALTER, so we re-introduce started_at as
-- a new column. Pre-existing rows are all in terminal states (the only
-- states the old code wrote), so leaving started_at NULL on those rows
-- is harmless — nothing queries it for terminal rows.
ALTER TABLE burn_jobs RENAME COLUMN started_at TO queued_at;
ALTER TABLE burn_jobs ADD COLUMN started_at    INTEGER;
ALTER TABLE burn_jobs ADD COLUMN progress_file TEXT;
ALTER TABLE burn_jobs ADD COLUMN helper_pid    INTEGER;

-- Indexes follow the table rename automatically, but SQLite has no
-- ALTER INDEX RENAME — drop and recreate so PRAGMA index_list stays
-- legible. The started-at descending index isn't useful any more —
-- queue order is queued_at ASC, history order is queued_at DESC, and
-- both are cheap full scans for the size of this table. Replace with a
-- state-keyed index, which is what reattach/hydrate scans on.
DROP INDEX idx_burn_history_started;
DROP INDEX idx_burn_history_job_id;
CREATE INDEX idx_burn_jobs_job_id ON burn_jobs(job_id);
CREATE INDEX idx_burn_jobs_state  ON burn_jobs(state);
CREATE INDEX idx_burn_jobs_queued ON burn_jobs(queued_at);

-- burn_logs.burn_id references burn_history(id). sqlite >= 3.26 rewrites
-- the referencing schema text on RENAME TO automatically, so the FK now
-- points at burn_jobs(id) without further action. The health_check pass
-- runs PRAGMA foreign_key_check after this migration and will fail loud
-- if that assumption breaks.
