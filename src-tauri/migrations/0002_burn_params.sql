-- 0002_burn_params.sql
-- Adds a JSON-encoded snapshot of the user-tunable write knobs that fed
-- into a burn. Captured at record_burn_started time so post-mortems can
-- tell which writer impl / chunk size / worker count / etc. was in
-- effect for any specific burn — previously these values were live in
-- config and the DB had no record of what was actually used per row.
--
-- TEXT (nullable) rather than a structured set of columns so we can add
-- new params without another migration. Shape is `{ "writer.impl":
-- "pipelined", "chunk.bytes": 1048576, ... }` — see
-- `db::collect_burn_params`.

ALTER TABLE burn_history ADD COLUMN burn_params TEXT;
