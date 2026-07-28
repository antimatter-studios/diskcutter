//! Block-level repair: when read-back finds chunks that don't match the
//! source, rewrite *only* those chunks with fresh bytes and re-verify,
//! instead of failing the whole burn or re-flashing the entire card.
//!
//! This is built on the burn-time per-chunk fingerprints
//! ([`crate::pipeline::BurnResult::chunk_digests`]) and the positioned
//! [`crate::writers::DeviceWriter::write_at`]. The work-list comes from
//! [`crate::pipeline::verify_chunks`]; the rewrite from
//! [`crate::pipeline::rewrite_chunks`]; this module is the orchestration loop
//! around them, plus the guardrails that decide when a card is unrepairable.
//!
//! Why guardrails matter: rewriting a bad chunk only helps when the failure
//! was *transient* (a dropped write, a bus glitch). On worn/dying flash the
//! rewrite lands on the same bad cells and fails again; on a counterfeit
//! fake-capacity card, writes past the real capacity wrap around and corrupt
//! *other* chunks — so a naive retry loop would thrash forever and destroy
//! more data than it fixes. The convergence check below stops the moment the
//! dirty set stops shrinking or *new* chunks go bad, and reports the card as
//! unrepairable rather than laundering a dying card into a false pass.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::hash::HashAlgo;
use crate::joblog::JobLogger;
use crate::pipeline::{self, BurnError, VerifyMismatch, VerifyProgress};
use crate::source;
#[cfg(unix)]
use crate::writers::RawDeviceIo;
use crate::writers::{DeviceIo, PlainFileDeviceIo};

/// Backstop on repair rounds even when the dirty set is shrinking. A healthy
/// card with transient errors converges in one round; more than a couple of
/// rounds means the medium is marginal and we should stop and flag it.
pub const DEFAULT_MAX_REPAIR_ROUNDS: u32 = 3;

#[derive(Debug, Clone)]
pub struct RepairOutcome {
    /// The device now matches the source (the repair succeeded).
    pub converged: bool,
    /// How many repair rounds ran (0 means the re-localisation found nothing
    /// to fix — a spurious whole-image mismatch).
    pub rounds: u32,
    /// Chunks that were dirty when repair started.
    pub initial_bad_chunks: usize,
    /// Chunks that ended up clean (`initial_bad - remaining`).
    pub repaired_chunks: usize,
    /// Chunks still bad when we stopped (0 when `converged`).
    pub remaining_bad_chunks: usize,
    /// Total bytes written across all rewrite rounds.
    pub bytes_rewritten: u64,
    /// Whole-image read-back digest after the final pass (audit/report).
    pub readback_sha256: String,
    /// We bailed because the dirty set stopped shrinking or new chunks went
    /// bad — the strong "this card is failing/counterfeit" signal.
    pub non_convergence: bool,
    /// Byte-level mismatch detail for the UI, populated only when the device
    /// still doesn't match (`!converged`).
    pub mismatches: Vec<VerifyMismatch>,
}

/// Choose a positioned-write-capable device IO for `target`, independent of
/// whatever (possibly sequential, pipelined) IO the burn used. Real device
/// nodes go through the raw char device; everything else is a plain file.
fn positioned_io(target: &str) -> Box<dyn DeviceIo> {
    #[cfg(unix)]
    {
        if target.starts_with("/dev/") {
            return Box::new(RawDeviceIo);
        }
    }
    Box::new(PlainFileDeviceIo)
}

/// Decide whether a repair round failed to make progress. A round is
/// non-converging when it introduced a chunk that was previously clean, or
/// when the dirty set did not get strictly smaller. Both mean rewriting isn't
/// healing the medium — stop before thrashing the card.
fn non_converging(prev_dirty: &[usize], new_dirty: &[usize]) -> bool {
    let introduced_new = new_dirty.iter().any(|c| !prev_dirty.contains(c));
    let shrank = new_dirty.len() < prev_dirty.len();
    introduced_new || !shrank
}

/// Re-read the source and device once more and run the full byte-compare
/// verify to collect per-sector mismatch detail for the failure report. Best
/// effort: any IO error here yields an empty detail list rather than masking
/// the real "repair failed" outcome.
#[allow(clippy::too_many_arguments)]
fn collect_detail(
    image: &str,
    target: &str,
    io: &dyn DeviceIo,
    job_log: &dyn JobLogger,
    total_bytes: u64,
    chunk_size: usize,
    hash_algo: HashAlgo,
    cancel: &AtomicBool,
) -> Vec<VerifyMismatch> {
    let opened = source::open_streaming_with_log(Path::new(image), job_log);
    let (mut src, _) = match opened {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let mut dev = match io.open_read(Path::new(target)) {
        Ok(d) => d,
        Err(_) => return Vec::new(),
    };
    match pipeline::verify_with_hash(
        &mut *src,
        total_bytes,
        &mut *dev,
        chunk_size,
        hash_algo,
        cancel,
        |_| {},
    ) {
        Ok(v) => v.mismatches,
        Err(_) => Vec::new(),
    }
}

/// Localise the device's bad chunks and rewrite them from the source until it
/// matches or we judge the card unrepairable.
///
/// Callers reach here only after a whole-image read-back mismatch. `image`
/// and `target` are paths (the source is reopened/restreamed each round —
/// the only way to reach a chunk in a compressed image); `expected_digests`
/// and `chunk_size` come from the burn's [`pipeline::BurnResult`]. The
/// `on_progress` sink is called as `(state, progress)` where `state` is
/// `"verifying"` or `"repairing"`, matching the burn's progress vocabulary.
#[allow(clippy::too_many_arguments)]
pub fn run_repair(
    image: &str,
    target: &str,
    job_log: &dyn JobLogger,
    expected_digests: &[u64],
    chunk_size: usize,
    total_bytes: u64,
    hash_algo: HashAlgo,
    max_rounds: u32,
    cancel: &AtomicBool,
    mut on_progress: impl FnMut(&str, VerifyProgress),
) -> Result<RepairOutcome, BurnError> {
    let io = positioned_io(target);

    // Round 0 — localise which chunks are actually bad (device-only read).
    let first = {
        let mut dev = io.open_read(Path::new(target))?;
        pipeline::verify_chunks(
            &mut *dev,
            expected_digests,
            chunk_size,
            total_bytes,
            hash_algo,
            cancel,
            |p| on_progress("verifying", p),
        )?
    };
    let initial_bad = first.dirty.len();
    let mut readback = first.readback_sha256;
    let mut dirty = first.dirty;

    job_log.info(&format!(
        "repair: {} chunk(s) failed read-back, attempting in-place rewrite",
        initial_bad
    ));

    if dirty.is_empty() {
        // Whole-image hash differed but per-chunk localisation finds nothing —
        // the device matches now. Treat as a (zero-work) pass.
        return Ok(RepairOutcome {
            converged: true,
            rounds: 0,
            initial_bad_chunks: 0,
            repaired_chunks: 0,
            remaining_bad_chunks: 0,
            bytes_rewritten: 0,
            readback_sha256: readback,
            non_convergence: false,
            mismatches: Vec::new(),
        });
    }

    let mut bytes_rewritten = 0u64;
    let mut rounds = 0u32;

    for round in 1..=max_rounds.max(1) {
        if cancel.load(Ordering::Relaxed) {
            return Err(BurnError::Cancelled);
        }
        rounds = round;

        // Rewrite the current dirty set from a fresh source stream.
        {
            let (mut src, _) = source::open_streaming_with_log(Path::new(image), job_log)?;
            let mut writer = io.open_write_at(Path::new(target))?;
            let rw = pipeline::rewrite_chunks(
                &mut *src,
                &mut *writer,
                &dirty,
                chunk_size,
                total_bytes,
                cancel,
                |p| on_progress("repairing", p),
            )?;
            writer.finish()?; // sync to the medium before re-reading
            bytes_rewritten += rw;
        }

        // Re-verify the whole device (catches collateral damage elsewhere,
        // e.g. counterfeit wrap-around, not just the chunks we rewrote).
        let v = {
            let mut dev = io.open_read(Path::new(target))?;
            pipeline::verify_chunks(
                &mut *dev,
                expected_digests,
                chunk_size,
                total_bytes,
                hash_algo,
                cancel,
                |p| on_progress("verifying", p),
            )?
        };
        readback = v.readback_sha256;
        let new_dirty = v.dirty;

        if new_dirty.is_empty() {
            job_log.info(&format!(
                "repair: device matches after {round} round(s), {initial_bad} chunk(s) repaired"
            ));
            return Ok(RepairOutcome {
                converged: true,
                rounds,
                initial_bad_chunks: initial_bad,
                repaired_chunks: initial_bad,
                remaining_bad_chunks: 0,
                bytes_rewritten,
                readback_sha256: readback,
                non_convergence: false,
                mismatches: Vec::new(),
            });
        }

        if non_converging(&dirty, &new_dirty) {
            job_log.warn(&format!(
                "repair: not converging (was {} dirty, now {}) — medium likely failing or \
                 counterfeit, stopping",
                dirty.len(),
                new_dirty.len()
            ));
            let mismatches = collect_detail(
                image,
                target,
                &*io,
                job_log,
                total_bytes,
                chunk_size,
                hash_algo,
                cancel,
            );
            return Ok(RepairOutcome {
                converged: false,
                rounds,
                initial_bad_chunks: initial_bad,
                repaired_chunks: initial_bad.saturating_sub(new_dirty.len()),
                remaining_bad_chunks: new_dirty.len(),
                bytes_rewritten,
                readback_sha256: readback,
                non_convergence: true,
                mismatches,
            });
        }

        dirty = new_dirty;
    }

    // Ran out of rounds while still shrinking — stubborn but not clearly
    // counterfeit. Report what's left.
    let remaining = dirty.len();
    job_log.warn(&format!(
        "repair: still {remaining} chunk(s) bad after {rounds} round(s), giving up"
    ));
    let mismatches = collect_detail(
        image,
        target,
        &*io,
        job_log,
        total_bytes,
        chunk_size,
        hash_algo,
        cancel,
    );
    Ok(RepairOutcome {
        converged: false,
        rounds,
        initial_bad_chunks: initial_bad,
        repaired_chunks: initial_bad.saturating_sub(remaining),
        remaining_bad_chunks: remaining,
        bytes_rewritten,
        readback_sha256: readback,
        non_convergence: false,
        mismatches,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_converging_false_when_dirty_set_shrinks() {
        // Healthy: fewer bad chunks than last round, all a subset.
        assert!(!non_converging(&[1, 2, 3], &[2, 3]));
    }

    #[test]
    fn non_converging_true_when_set_does_not_shrink() {
        // Same chunks still bad — the cells won't take the data.
        assert!(non_converging(&[4, 5], &[4, 5]));
    }

    #[test]
    fn non_converging_true_when_new_chunk_goes_bad() {
        // A previously-clean chunk failed — counterfeit wrap-around signature,
        // even though the count happens to drop.
        assert!(non_converging(&[1, 2, 3], &[9]));
    }

    #[test]
    fn non_converging_false_when_fully_clean() {
        assert!(!non_converging(&[1, 2], &[]));
    }
}
