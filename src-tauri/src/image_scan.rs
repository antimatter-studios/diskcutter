//! Deep image scanner.
//!
//! Single sequential pass through a disk image (raw or compressed via the
//! decoder chain) that yields:
//!
//! - Format chain labels ("xz → raw", etc.)
//! - Partition table from the prefix (MBR / GPT layout)
//! - Per-partition filesystem labels, picked up from 64 KB superblock samples
//!   captured as the stream flows past each partition's start LBA
//! - Boot signals visible within the prefix
//! - True uncompressed size *for xz only* (parsed from the stream footer);
//!   gzip / bzip2 / zstd fall back to the on-disk file size as a placeholder
//!
//! Stops at `max(partition_start) + 64 KB` so multi-GB compressed images
//! don't pay for decompressing the whole stream when all the user wants is
//! filesystem labels. The trailing portion (which would have given us
//! source SHA-256 and the true uncompressed size for non-xz formats) is
//! deliberately skipped — see `docs/status.md` for the deferred-work
//! revisit notes.
//!
//! Results are upserted into the `image_scans` cache table keyed by image
//! path. Progressive Tauri events fire so the UI populates fields one at
//! a time as they become known.

use std::path::Path;
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

use crate::db::{self, Db, ImageScanPatch};
use crate::decoder_chain::block_view::PrefixBlockView;
use crate::decoder_chain::DiskReader;
use crate::joblog::JobLogger;

/// Bytes of post-table sample we keep for each partition. Big enough for
/// ext2/3/4 superblock at offset 1024, NTFS BPB at 0, FAT BPB at 0, HFS+
/// volume header at 1024, plus headroom for anything padded.
const PER_PARTITION_SAMPLE_BYTES: usize = 64 * 1024;

/// How much of the leading stream we slurp into a `BlockRead`-friendly
/// buffer for the initial partition / boot probes. Mirrors the
/// `DEFAULT_PREFIX_BYTES` used inside the decoder_chain.
const PREFIX_BYTES: usize = 4 * 1024 * 1024;

/// How often progress events fire during the discard-and-sample walk.
const PROGRESS_THROTTLE_MS: u64 = 150;

#[derive(Serialize, Clone)]
struct ProgressPayload {
    job_id: String,
    image_path: String,
    bytes_done: u64,
    bytes_total: u64,
}

#[derive(Serialize, Clone)]
struct PartitionFsPayload {
    job_id: String,
    image_path: String,
    partition_index: u32,
    filesystem: Option<String>,
    fs_label: Option<String>,
}

#[derive(Serialize, Clone)]
struct CompletePayload {
    job_id: String,
    image_path: String,
}

/// Sparse `BlockRead` view assembled from the prefix slurp + per-partition
/// samples. Reads inside any captured range succeed; everywhere else returns
/// `OutOfBounds`. Lets `partitions::sniff` work against the same byte ranges
/// the partition probe would have seen on a fully random-access source.
struct SparseSampleView {
    ranges: Vec<(u64, Vec<u8>)>,
    end: u64,
}

impl SparseSampleView {
    fn new(ranges: Vec<(u64, Vec<u8>)>) -> Self {
        let end = ranges
            .iter()
            .map(|(o, b)| o + b.len() as u64)
            .max()
            .unwrap_or(0);
        Self { ranges, end }
    }

    /// Return the slice of the captured sample whose range contains
    /// `offset`, together with the position of `offset` inside that
    /// slice. `None` if no captured range covers it.
    fn sample_at(&self, offset: u64) -> Option<&[u8]> {
        for (sample_start, sample_bytes) in &self.ranges {
            let sample_end = sample_start + sample_bytes.len() as u64;
            if offset >= *sample_start && offset < sample_end {
                let rel = (offset - sample_start) as usize;
                return Some(&sample_bytes[rel..]);
            }
        }
        None
    }
}

/// Find the captured bytes for a partition and run the FS-label
/// parser against them. Returns `None` when we never sampled the
/// partition's start (e.g. partition begins past `stop_at`) or when
/// the parser doesn't support `kind`.
fn fs_label_for_partition(
    view: &SparseSampleView,
    partition_start: u64,
    kind: partitions::sniff::FsKind,
) -> Option<String> {
    let sample = view.sample_at(partition_start)?;
    crate::inspect::fs_label_from_sample(kind, sample)
}

impl fs_core::BlockRead for SparseSampleView {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> fs_core::Result<()> {
        let len = buf.len() as u64;
        for (sample_start, sample_bytes) in &self.ranges {
            let sample_end = sample_start + sample_bytes.len() as u64;
            if offset >= *sample_start && offset + len <= sample_end {
                let rel = (offset - sample_start) as usize;
                buf.copy_from_slice(&sample_bytes[rel..rel + buf.len()]);
                return Ok(());
            }
        }
        Err(fs_core::Error::OutOfBounds {
            offset,
            len,
            size: self.end,
        })
    }
    fn size_bytes(&self) -> u64 {
        self.end
    }
}

/// Spawn a deep-scan worker for `image_path` against the row `job_id`.
/// Idempotent: if a fresh cache row exists, fires an immediate
/// `image-scan-complete` and returns. Otherwise upserts an in-progress
/// row, runs the scan, and updates the row in-place as it discovers each
/// piece.
pub fn spawn_scan(app: AppHandle, job_id: String, image_path: String) {
    std::thread::spawn(move || {
        if let Some(row) = db::image_scan_get(&app.state::<Db>(), &image_path) {
            if row.scan_complete && db::image_scan_is_fresh(&row) {
                emit_complete(&app, &job_id, &image_path);
                return;
            }
        }
        if let Err(e) = run_scan(&app, &job_id, &image_path) {
            eprintln!("image_scan: failed for {image_path}: {e}");
        }
        emit_complete(&app, &job_id, &image_path);
    });
}

fn emit_complete(app: &AppHandle, job_id: &str, image_path: &str) {
    let _ = app.emit(
        "disk-cutter://image-scan-complete",
        CompletePayload {
            job_id: job_id.to_string(),
            image_path: image_path.to_string(),
        },
    );
}

fn run_scan(app: &AppHandle, job_id: &str, image_path: &str) -> Result<(), String> {
    let path = Path::new(image_path);
    let meta = std::fs::metadata(path).map_err(|e| format!("stat {image_path}: {e}"))?;
    let file_size = meta.len() as i64;
    let file_mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let db_state = app.state::<Db>();
    db::image_scan_begin(&db_state, image_path, file_size, file_mtime);

    let log = crate::joblog::db_logger_for(app, job_id);
    log.info(&format!("scan: starting deep scan of {image_path}"));

    // Open the chain and grab the prefix in one shot. Captures format_chain
    // and the partition table from the same bytes.
    let mut reader =
        DiskReader::open_with_log(path, &log).map_err(|e| format!("open decoder chain: {e}"))?;
    let format_chain = reader.format_chain().to_vec();
    let format_chain_json =
        serde_json::to_string(&format_chain).unwrap_or_else(|_| "[]".to_string());

    let prefix = reader
        .slurp_prefix(PREFIX_BYTES)
        .map_err(|e| format!("slurp prefix: {e}"))?;
    let prefix_view = PrefixBlockView::new(prefix.clone());
    let summary = crate::inspect::inspect_block_read(&prefix_view);
    let partition_table_json = summary
        .as_ref()
        .map(|s| serde_json::to_string(s).unwrap_or_else(|_| "null".to_string()));
    let boot_sources = crate::image::compute_boot_sources(&prefix_view, summary.as_ref());
    let boot_sources_json =
        serde_json::to_string(&boot_sources).unwrap_or_else(|_| "[]".to_string());

    // True uncompressed size — only xz exposes this without a full pass.
    // For all other compressed formats and for raw we fall back to the
    // on-disk file size as a conservative placeholder.
    let uncompressed_bytes = if matches!(format_chain.first().copied(), Some("xz")) {
        crate::xz_footer::read_total_uncompressed(path).map(|v| v as i64)
    } else {
        Some(file_size)
    };

    db::image_scan_patch(
        &db_state,
        image_path,
        ImageScanPatch {
            format_chain: Some(format_chain_json),
            uncompressed_bytes,
            partition_table: partition_table_json.clone(),
            boot_sources: Some(boot_sources_json),
            ..Default::default()
        },
    );

    // If no partition table or no partitions, the scan is effectively done
    // — there's nothing past the prefix the user is going to see.
    let Some(mut summary) = summary else {
        log.info("scan: no partition table; stopping at prefix");
        db::image_scan_patch(
            &db_state,
            image_path,
            ImageScanPatch {
                scan_complete: Some(true),
                ..Default::default()
            },
        );
        return Ok(());
    };
    if summary.partitions.is_empty() {
        db::image_scan_patch(
            &db_state,
            image_path,
            ImageScanPatch {
                scan_complete: Some(true),
                ..Default::default()
            },
        );
        return Ok(());
    }

    // Compute sample ranges (start_byte, want_bytes) per partition.
    // Skip partitions whose start is *inside* the prefix — we already
    // have their bytes in the prefix view and the sniff at the bottom
    // will use that.
    let mut sample_ranges: Vec<(u32, u64, usize)> = Vec::new();
    let mut stop_at: u64 = prefix.len() as u64;
    for p in &summary.partitions {
        let want = PER_PARTITION_SAMPLE_BYTES.min(p.length_bytes as usize);
        let end = p.start_bytes + want as u64;
        if end > stop_at {
            stop_at = end;
        }
        if p.start_bytes >= prefix.len() as u64 {
            sample_ranges.push((p.index, p.start_bytes, want));
        }
    }

    log.info(&format!(
        "scan: walking chain to stop_at={stop_at} bytes, collecting {} per-partition sample(s)",
        sample_ranges.len()
    ));

    // Walk the chain. Position = uncompressed bytes already consumed
    // (including the prefix slurp). Read in chunks; copy into per-partition
    // sample buffers when the cursor overlaps a target range; discard
    // bytes that fall outside any range.
    let bytes_total = uncompressed_bytes.map(|v| v as u64).unwrap_or(stop_at);
    let mut pos: u64 = prefix.len() as u64;
    let mut samples: Vec<(u32, u64, Vec<u8>)> = sample_ranges
        .iter()
        .map(|(idx, start, want)| (*idx, *start, Vec::with_capacity(*want)))
        .collect();

    let mut chunk = vec![0u8; 256 * 1024];
    let mut last_progress = std::time::Instant::now();

    while pos < stop_at {
        // Cap the chunk so we don't overshoot stop_at.
        let want = ((stop_at - pos) as usize).min(chunk.len());
        let n = reader
            .read(&mut chunk[..want])
            .map_err(|e| format!("read chain: {e}"))?;
        if n == 0 {
            break;
        }
        let chunk_start = pos;
        let chunk_end = pos + n as u64;

        for (_, target_start, buf) in samples.iter_mut() {
            let target_end = *target_start + buf.capacity() as u64;
            // Overlap of [chunk_start, chunk_end) with [target_start, target_end)
            let lo = chunk_start.max(*target_start);
            let hi = chunk_end.min(target_end);
            if lo < hi {
                let chunk_off = (lo - chunk_start) as usize;
                let len = (hi - lo) as usize;
                buf.extend_from_slice(&chunk[chunk_off..chunk_off + len]);
            }
        }

        pos = chunk_end;

        if last_progress.elapsed() >= Duration::from_millis(PROGRESS_THROTTLE_MS) {
            let _ = app.emit(
                "disk-cutter://image-scan-progress",
                ProgressPayload {
                    job_id: job_id.to_string(),
                    image_path: image_path.to_string(),
                    bytes_done: pos,
                    bytes_total,
                },
            );
            last_progress = std::time::Instant::now();
        }
    }

    let _ = app.emit(
        "disk-cutter://image-scan-progress",
        ProgressPayload {
            job_id: job_id.to_string(),
            image_path: image_path.to_string(),
            bytes_done: pos,
            bytes_total,
        },
    );

    // Assemble the sparse view: prefix + each filled sample.
    let mut ranges: Vec<(u64, Vec<u8>)> = Vec::new();
    ranges.push((0, prefix));
    for (idx, start, buf) in &samples {
        if !buf.is_empty() {
            ranges.push((*start, buf.clone()));
            log.debug(&format!(
                "scan: partition {idx} sample collected: {} bytes at offset {start}",
                buf.len()
            ));
        }
    }
    let view = SparseSampleView::new(ranges);

    // Re-run sniff for each partition using the assembled view; this
    // populates the per-partition filesystem label that was None in the
    // prefix-only summary.
    let parts_raw = match partitions::probe::probe(&view) {
        Ok((_, parts)) => parts,
        Err(_) => Vec::new(),
    };
    for (idx, p) in parts_raw.iter().enumerate() {
        let fs = partitions::sniff::sniff(&view, p).ok();
        let filesystem: Option<String> = fs.map(crate::inspect::format_fs_kind);
        // Pull the volume label out of the same bytes we collected for
        // this partition: either the prefix slurp (if the partition
        // starts inside the first PREFIX_BYTES) or its own per-partition
        // sample. Looking up the range directly avoids the
        // `view.read_at` OutOfBounds case for partitions whose length
        // is smaller than the requested read.
        let fs_label: Option<String> =
            fs.and_then(|kind| fs_label_for_partition(&view, p.start, kind));
        let partition_index = (idx + 1) as u32;
        if let Some(part_info) = summary
            .partitions
            .iter_mut()
            .find(|p| p.index == partition_index)
        {
            part_info.filesystem = filesystem.clone();
            part_info.fs_label = fs_label.clone();
        }
        let _ = app.emit(
            "disk-cutter://image-scan-partition-fs",
            PartitionFsPayload {
                job_id: job_id.to_string(),
                image_path: image_path.to_string(),
                partition_index,
                filesystem,
                fs_label,
            },
        );
    }

    let final_partition_json = serde_json::to_string(&summary).unwrap_or_else(|_| "null".into());
    db::image_scan_patch(
        &db_state,
        image_path,
        ImageScanPatch {
            partition_table: Some(final_partition_json),
            scan_complete: Some(true),
            ..Default::default()
        },
    );
    log.info("scan: deep scan complete");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use fs_core::BlockRead;

    #[test]
    fn sparse_view_reads_within_a_range_and_errors_outside() {
        let view = SparseSampleView::new(vec![(0, vec![1, 2, 3, 4]), (100, vec![10, 20, 30])]);
        let mut buf = [0u8; 2];
        view.read_at(0, &mut buf).unwrap();
        assert_eq!(buf, [1, 2]);
        view.read_at(101, &mut buf).unwrap();
        assert_eq!(buf, [20, 30]);
        let mut over = [0u8; 2];
        // Spans a gap — should error.
        assert!(view.read_at(3, &mut over).is_err());
    }
}
