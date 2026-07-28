//! End-to-end test for the block-repair orchestration.
//!
//! Lays down a "burned" target file that matches a source image except for
//! one corrupted chunk, then drives `repair::run_repair` against real files
//! (raw `.iso` source streamed via the source layer, plain-file target) and
//! asserts it rewrites only the bad chunk and converges.

use std::num::Wrapping;
use std::sync::atomic::AtomicBool;

use diskcutter_lib::hash::{self, HashAlgo};
use diskcutter_lib::joblog::NullLogger;
use diskcutter_lib::repair::{run_repair, DEFAULT_MAX_REPAIR_ROUNDS};

use tempfile::tempdir;

fn deterministic_bytes(len: usize, seed: u64) -> Vec<u8> {
    let mut state = Wrapping(seed);
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        state += Wrapping(0x9E37_79B9_7F4A_7C15);
        let mut z = state;
        z = (z ^ (z >> 30)) * Wrapping(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)) * Wrapping(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        out.extend_from_slice(&z.0.to_le_bytes());
    }
    out.truncate(len);
    out
}

#[test]
fn repair_rewrites_only_the_bad_chunk_and_converges() {
    let dir = tempdir().unwrap();
    // `.iso` is probed as a raw passthrough source, so the streamed bytes are
    // exactly the file contents.
    let source = dir.path().join("image.iso");
    let target = dir.path().join("target.img");

    let chunk = 64 * 1024usize;
    let data = deterministic_bytes(chunk * 3, 0x5EED_1234_ABCD_0001);
    std::fs::write(&source, &data).unwrap();

    // Per-chunk fingerprints, as the burn would have recorded them.
    let digests: Vec<u64> = data.chunks(chunk).map(hash::chunk_digest).collect();

    // The "burned" device: correct everywhere except chunk index 1.
    let mut dev = data.clone();
    for b in &mut dev[chunk..chunk * 2] {
        *b ^= 0x5A;
    }
    std::fs::write(&target, &dev).unwrap();

    let cancel = AtomicBool::new(false);
    let outcome = run_repair(
        source.to_str().unwrap(),
        target.to_str().unwrap(),
        &NullLogger,
        &digests,
        chunk,
        data.len() as u64,
        HashAlgo::Xxh3,
        DEFAULT_MAX_REPAIR_ROUNDS,
        &cancel,
        |_, _| {},
    )
    .expect("repair ran");

    assert!(outcome.converged, "device should match after repair");
    assert_eq!(outcome.initial_bad_chunks, 1);
    assert_eq!(outcome.repaired_chunks, 1);
    assert_eq!(outcome.remaining_bad_chunks, 0);
    assert!(!outcome.non_convergence);
    assert_eq!(outcome.bytes_rewritten, chunk as u64);

    // The whole device now equals the source — chunks 0 and 2 were untouched.
    assert_eq!(std::fs::read(&target).unwrap(), data);
}

#[test]
fn repair_is_a_clean_passthrough_when_device_already_matches() {
    let dir = tempdir().unwrap();
    let source = dir.path().join("image.iso");
    let target = dir.path().join("target.img");

    let chunk = 64 * 1024usize;
    let data = deterministic_bytes(chunk * 2, 0x0BAD_F00D_1234_5678);
    std::fs::write(&source, &data).unwrap();
    std::fs::write(&target, &data).unwrap(); // device already correct

    let digests: Vec<u64> = data.chunks(chunk).map(hash::chunk_digest).collect();
    let cancel = AtomicBool::new(false);
    let outcome = run_repair(
        source.to_str().unwrap(),
        target.to_str().unwrap(),
        &NullLogger,
        &digests,
        chunk,
        data.len() as u64,
        HashAlgo::Xxh3,
        DEFAULT_MAX_REPAIR_ROUNDS,
        &cancel,
        |_, _| {},
    )
    .expect("repair ran");

    assert!(outcome.converged);
    assert_eq!(outcome.initial_bad_chunks, 0);
    assert_eq!(outcome.rounds, 0);
    assert_eq!(outcome.bytes_rewritten, 0);
}
