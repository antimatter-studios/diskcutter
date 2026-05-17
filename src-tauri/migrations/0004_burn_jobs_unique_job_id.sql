-- 0004_burn_jobs_unique_job_id.sql
-- Closes the data-layer hole that allowed two rows with the same job_id.
-- The old `insert_queued_row` upsert relied on a narrow SELECT
-- (state IN ('queued','running')) to decide whether to INSERT or
-- recycle — when the existing row was in 'error' or 'cancelled', the
-- SELECT missed and a duplicate INSERT happened. Adding a UNIQUE
-- constraint on burn_jobs.job_id makes that path fail loudly at the
-- DB layer instead of producing two ghost rows.
--
-- Step 1: clean up data so the constraint can be added without
-- violating it (dedup existing duplicates; remove orphan burn_logs
-- left behind by past burn_jobs deletions where foreign_keys=OFF
-- prevented cascade).
--
-- Step 2: add the UNIQUE INDEX. Drops the old non-unique index of the
-- same name first.

-- 1a. Dedupe burn_jobs by job_id: keep the most recently queued row
--     per job_id, delete the rest.
DELETE FROM burn_jobs
WHERE id NOT IN (
  SELECT id FROM burn_jobs b1
  WHERE b1.queued_at = (
    SELECT MAX(b2.queued_at) FROM burn_jobs b2 WHERE b2.job_id = b1.job_id
  )
);

-- 1b. Strip orphan burn_logs rows.
DELETE FROM burn_logs
WHERE burn_id NOT IN (SELECT id FROM burn_jobs);

-- 2. Replace the non-unique index with a UNIQUE one.
DROP INDEX IF EXISTS idx_burn_jobs_job_id;
CREATE UNIQUE INDEX idx_burn_jobs_job_id ON burn_jobs(job_id);
