//! End-to-end integration tests for the burn + verify pipeline.
//!
//! Each test wires together a custom in-memory `ImageReader` (the "source")
//! and the on-disk `PlainFileDeviceIo` (the "target"), runs `burn`, then
//! re-reads the target file and runs `verify_hash_only` or `verify`.
//!
//! The intent is to lock down end-to-end behaviour so future regressions
//! in any of the chunking / hashing / cancellation logic are caught before
//! they reach a real block device.

use std::io::{Cursor, Read, Seek, SeekFrom, Write};
use std::num::Wrapping;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use disk_cutter_lib::hash::HashAlgo;
use disk_cutter_lib::pipeline::{
    burn, burn_with_hash, verify, verify_hash_only, verify_hash_only_with_hash, BurnError,
    DEFAULT_CHUNK,
};
use disk_cutter_lib::readers::{ImageInfo, ImageReader};
use disk_cutter_lib::writers::{DeviceIo, PlainFileDeviceIo};

use sha2::{Digest, Sha256};
use tempfile::tempdir;

/// Deterministic byte stream from a fixed seed — same output every run.
/// Uses `Wrapping<u64>` so we never panic on overflow.
fn deterministic_bytes(len: usize, seed: u64) -> Vec<u8> {
    let mut state = Wrapping(seed);
    let mut out = Vec::with_capacity(len);
    // splitmix64-ish step; cheap, no allocations, fully deterministic.
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

fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    let digest = h.finalize();
    let mut s = String::with_capacity(digest.len() * 2);
    for b in digest {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

/// Minimal in-memory `ImageReader` for the source side of the pipeline.
struct TestImageReader {
    info: ImageInfo,
    data: Cursor<Vec<u8>>,
}

impl TestImageReader {
    fn new(data: Vec<u8>) -> Self {
        let len = data.len() as u64;
        Self {
            info: ImageInfo {
                path: PathBuf::from("/test.img"),
                format_label: "TEST".into(),
                source_bytes: len,
                uncompressed_bytes: len,
            },
            data: Cursor::new(data),
        }
    }
}

impl Read for TestImageReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.data.read(buf)
    }
}

impl ImageReader for TestImageReader {
    fn info(&self) -> &ImageInfo {
        &self.info
    }
}

#[test]
fn burn_then_verify_hash_only_matches() {
    let dir = tempdir().unwrap();
    let target = dir.path().join("target.img");

    let data = deterministic_bytes(4 * 1024 * 1024, 0xC0FF_EE00_DEAD_BEEF);
    let mut reader = TestImageReader::new(data.clone());

    let io = PlainFileDeviceIo;
    let writer = io.open_write(&target).unwrap();
    let cancel = AtomicBool::new(false);

    let burn_result = burn(&mut reader, writer, DEFAULT_CHUNK, &cancel, |_| {}).expect("burn ok");

    assert_eq!(burn_result.bytes_written, data.len() as u64);
    assert_eq!(burn_result.source_sha256, sha256_hex(&data));

    let mut device_reader = io.open_read(&target).unwrap();
    let cancel = AtomicBool::new(false);
    let hash_result = verify_hash_only(
        device_reader.as_mut(),
        data.len() as u64,
        DEFAULT_CHUNK,
        &cancel,
        |_| {},
    )
    .expect("verify_hash_only ok");

    assert_eq!(hash_result.bytes_checked, data.len() as u64);
    assert_eq!(hash_result.readback_sha256, burn_result.source_sha256);
}

#[test]
fn burn_then_verify_byte_compare_matches() {
    let dir = tempdir().unwrap();
    let target = dir.path().join("target.img");

    let data = deterministic_bytes(4 * 1024 * 1024, 0x1234_5678_9ABC_DEF0);
    let mut source_reader = TestImageReader::new(data.clone());

    let io = PlainFileDeviceIo;
    let writer = io.open_write(&target).unwrap();
    let cancel = AtomicBool::new(false);

    burn(&mut source_reader, writer, DEFAULT_CHUNK, &cancel, |_| {}).expect("burn ok");

    // Reset source for byte-compare verify.
    let mut source_reader = TestImageReader::new(data.clone());
    let mut device_reader = io.open_read(&target).unwrap();
    let cancel = AtomicBool::new(false);

    let result = verify(
        &mut source_reader,
        device_reader.as_mut(),
        DEFAULT_CHUNK,
        &cancel,
        |_| {},
    )
    .expect("verify ok");

    assert!(result.match_, "byte compare should match");
    assert!(result.mismatches.is_empty());
    assert_eq!(result.source_sha256, result.readback_sha256);
    assert_eq!(result.bytes_checked, data.len() as u64);
}

#[test]
fn burn_then_corrupt_then_verify_finds_mismatch() {
    let dir = tempdir().unwrap();
    let target = dir.path().join("target.img");

    let data = deterministic_bytes(4 * 1024 * 1024, 0xFEED_FACE_CAFE_F00D);
    let mut source_reader = TestImageReader::new(data.clone());

    let io = PlainFileDeviceIo;
    let writer = io.open_write(&target).unwrap();
    let cancel = AtomicBool::new(false);

    burn(&mut source_reader, writer, DEFAULT_CHUNK, &cancel, |_| {}).expect("burn ok");

    // Corrupt 16 bytes at offset 1 MiB.
    const CORRUPT_OFFSET: u64 = 1024 * 1024;
    {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .open(&target)
            .expect("open target for corruption");
        f.seek(SeekFrom::Start(CORRUPT_OFFSET)).unwrap();
        f.write_all(&[0xAB; 16]).unwrap();
        f.sync_all().unwrap();
    }

    let mut source_reader = TestImageReader::new(data.clone());
    let mut device_reader = io.open_read(&target).unwrap();
    let cancel = AtomicBool::new(false);

    let result = verify(
        &mut source_reader,
        device_reader.as_mut(),
        DEFAULT_CHUNK,
        &cancel,
        |_| {},
    )
    .expect("verify ok");

    assert!(!result.match_, "corrupted region must not match");
    assert!(
        !result.mismatches.is_empty(),
        "expected at least one mismatch"
    );

    // Expected LBA = CORRUPT_OFFSET / 512 = 2048 = 0x800.
    let expected_lba = format!("0x{:08X}", CORRUPT_OFFSET / 512);
    let first = &result.mismatches[0];
    assert_eq!(
        first.lba, expected_lba,
        "first mismatch LBA should sit at offset 1 MiB / 512"
    );
}

#[test]
fn cancel_mid_burn_returns_cancelled_error() {
    let dir = tempdir().unwrap();
    let target = dir.path().join("target.img");

    // 64 MiB — large enough that a 10 ms sleep lands mid-burn even on fast disks.
    let data = deterministic_bytes(64 * 1024 * 1024, 0xDEAD_BEEF_DEAD_BEEF);
    let mut reader = TestImageReader::new(data);

    let io = PlainFileDeviceIo;
    let writer = io.open_write(&target).unwrap();
    let cancel = std::sync::Arc::new(AtomicBool::new(false));

    let canceller = {
        let cancel = cancel.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(10));
            cancel.store(true, Ordering::Relaxed);
        })
    };

    let res = burn(&mut reader, writer, DEFAULT_CHUNK, cancel.as_ref(), |_| {});
    canceller.join().unwrap();

    match res {
        Err(BurnError::Cancelled) => {}
        Err(e) => panic!("expected Cancelled, got {e:?}"),
        Ok(_) => panic!("expected Cancelled error, burn completed successfully"),
    }
}

#[test]
fn cancel_mid_verify_returns_cancelled_error() {
    let dir = tempdir().unwrap();
    let target = dir.path().join("target.img");

    // 64 MiB — large enough that a 10 ms sleep lands mid-verify even on fast disks.
    let data = deterministic_bytes(64 * 1024 * 1024, 0xBADD_CAFE_BADD_CAFE);
    let mut reader = TestImageReader::new(data.clone());

    let io = PlainFileDeviceIo;
    let writer = io.open_write(&target).unwrap();
    let burn_cancel = AtomicBool::new(false);

    burn(&mut reader, writer, DEFAULT_CHUNK, &burn_cancel, |_| {}).expect("burn ok");

    let mut device_reader = io.open_read(&target).unwrap();
    let cancel = std::sync::Arc::new(AtomicBool::new(false));

    let canceller = {
        let cancel = cancel.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(10));
            cancel.store(true, Ordering::Relaxed);
        })
    };

    let res = verify_hash_only_with_hash(
        device_reader.as_mut(),
        data.len() as u64,
        DEFAULT_CHUNK,
        HashAlgo::Sha256,
        cancel.as_ref(),
        |_| {},
    );
    canceller.join().unwrap();

    match res {
        Err(BurnError::Cancelled) => {}
        Err(e) => panic!("expected Cancelled, got {e:?}"),
        Ok(_) => panic!("expected Cancelled error, verify completed successfully"),
    }
}

#[test]
fn burn_then_verify_hash_only_round_trips_with_xxhash_on_real_file() {
    // Same shape as `burn_then_verify_hash_only_matches`, but using
    // HashAlgo::Xxhash end-to-end against a real on-disk target via
    // PlainFileDeviceIo. Locks the helper-side wiring against regressions.
    let dir = tempdir().unwrap();
    let target = dir.path().join("target.img");

    let data = deterministic_bytes(4 * 1024 * 1024, 0xABAD_1DEA_DEAD_BEEF);
    let mut reader = TestImageReader::new(data.clone());

    let io = PlainFileDeviceIo;
    let writer = io.open_write(&target).unwrap();
    let cancel = AtomicBool::new(false);

    let burn_result = burn_with_hash(
        &mut reader,
        writer,
        DEFAULT_CHUNK,
        HashAlgo::Xxhash,
        &cancel,
        |_| {},
    )
    .expect("burn ok");

    // xxh64 hex is exactly 16 chars — sanity-check we didn't get a 64-char SHA-256.
    assert_eq!(burn_result.source_sha256.len(), 16);
    assert_eq!(burn_result.bytes_written, data.len() as u64);

    let mut device_reader = io.open_read(&target).unwrap();
    let cancel = AtomicBool::new(false);
    let hash_result = verify_hash_only_with_hash(
        device_reader.as_mut(),
        data.len() as u64,
        DEFAULT_CHUNK,
        HashAlgo::Xxhash,
        &cancel,
        |_| {},
    )
    .expect("verify_hash_only ok");

    assert_eq!(hash_result.readback_sha256, burn_result.source_sha256);
    assert_eq!(hash_result.bytes_checked, data.len() as u64);
}

#[test]
fn empty_image_burns_clean() {
    let dir = tempdir().unwrap();
    let target = dir.path().join("target.img");

    let mut reader = TestImageReader::new(Vec::new());

    let io = PlainFileDeviceIo;
    let writer = io.open_write(&target).unwrap();
    let cancel = AtomicBool::new(false);

    let result = burn(&mut reader, writer, DEFAULT_CHUNK, &cancel, |_| {}).expect("burn ok");

    // SHA-256 of empty string is well-known.
    const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    assert_eq!(result.bytes_written, 0);
    assert_eq!(result.source_sha256, EMPTY_SHA256);

    let meta = std::fs::metadata(&target).expect("target exists");
    assert_eq!(meta.len(), 0);
}
