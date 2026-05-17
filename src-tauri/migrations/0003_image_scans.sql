-- 0003_image_scans.sql
-- One-row-per-image cache of the deep scan results so adding the same image
-- to multiple burn_jobs only pays the decompression cost once.
--
-- Rows are keyed by `image_path` and freshness is verified on read by
-- comparing `file_size` + `file_mtime` against the live file's metadata.
-- A REFRESH on any row pointing at this image, or a file that changed out
-- from under us, invalidates the cached row.
--
-- `scan_complete = 0` rows exist *during* the scan so the UI can read
-- partial data; they flip to `1` on successful finish. A crash mid-scan
-- leaves the row at `0`, which the next scan attempt overwrites without
-- trusting partial data.

CREATE TABLE image_scans (
  id                  INTEGER PRIMARY KEY AUTOINCREMENT,
  image_path          TEXT    NOT NULL UNIQUE,
  -- Freshness key: combined with image_path, this is what tells us whether
  -- the cached scan still describes the file we're about to read.
  file_size           INTEGER NOT NULL,
  file_mtime          INTEGER NOT NULL,
  scanned_at          INTEGER NOT NULL,
  scan_complete       INTEGER NOT NULL DEFAULT 0,
  -- Decoder chain output: innermost-to-outermost JSON array (e.g. ["xz","raw"]).
  format_chain        TEXT,
  -- True decompressed size from the scan; distinct from the optimistic
  -- xz-footer estimate stored in burn_jobs.image_bytes.
  uncompressed_bytes  INTEGER,
  -- SHA-256 of the decoded source bytes, captured during the same pass.
  -- Allows the burn pipeline to skip its own source-hash computation when
  -- the cached scan covers the bytes about to be written.
  image_sha256        TEXT,
  validation_result   TEXT,
  validation_detail   TEXT,
  -- JSON blob: PartitionSummary or null. Per-partition `filesystem` fields
  -- carry the labels picked up from superblock samples during the scan.
  partition_table     TEXT,
  -- JSON array of BootSource values. Empty array = no boot signals.
  boot_sources        TEXT    NOT NULL DEFAULT '[]',
  -- JSON map: { partition_index_str: compressed_byte_offset_int }. Lets the
  -- future partition-extraction feature jump straight to a partition's
  -- bytes without rescanning the chain.
  partition_offsets   TEXT    NOT NULL DEFAULT '{}'
);

CREATE INDEX idx_image_scans_path ON image_scans(image_path);
