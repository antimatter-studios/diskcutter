//! Writer benchmark harness.
//!
//! Runs every `DeviceIo` implementation against the same deterministic
//! 256 MiB synthetic image and reports write / verify throughput plus
//! total wall time. The benchmark targets ordinary tempfiles, not real
//! block devices — the goal is to compare the I/O strategies (plain,
//! buffered block, pipelined pwrite worker pool) and to sweep
//! pipelined `(workers, queue_depth)` to find a good default.
//!
//! Run:
//!     cargo run --release --manifest-path src-tauri/Cargo.toml --example benchmark
//!
//! Note: `PipelinedRawDeviceIo` and `BlockDeviceIo` both `open()` with
//! `read+write` but no `create(true)`. The benchmark pre-allocates the
//! tempfile with `set_len(IMAGE_BYTES)` before handing it to `open_write`.

use std::fs::OpenOptions;
use std::io::{Cursor, Read};
use std::num::Wrapping;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use disk_cutter_lib::pipeline::{self, DEFAULT_CHUNK};
use disk_cutter_lib::readers::{ImageInfo, ImageReader};
use disk_cutter_lib::writers::{DeviceIo, PlainFileDeviceIo};

#[cfg(unix)]
use disk_cutter_lib::writers::{BlockDeviceIo, PipelinedRawDeviceIo};

use tempfile::tempdir;

const IMAGE_BYTES: usize = 256 * 1024 * 1024;
const SEED: u64 = 0xD15C_C077_E72B_E5C4;

/// Deterministic byte stream (splitmix64-ish) — same output every run.
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

/// In-memory `ImageReader` backed by a `Cursor<Vec<u8>>`.
struct MemImageReader {
    info: ImageInfo,
    cursor: Cursor<Vec<u8>>,
}

impl MemImageReader {
    fn new(data: Vec<u8>) -> Self {
        let len = data.len() as u64;
        Self {
            info: ImageInfo {
                path: PathBuf::from("/synthetic.img"),
                format_label: "SYNTHETIC".into(),
                source_bytes: len,
                uncompressed_bytes: len,
            },
            cursor: Cursor::new(data),
        }
    }

    fn rewind(&mut self) {
        self.cursor.set_position(0);
    }
}

impl Read for MemImageReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.cursor.read(buf)
    }
}

impl ImageReader for MemImageReader {
    fn info(&self) -> &ImageInfo {
        &self.info
    }
}

/// Result row for one device-IO impl.
struct Row {
    label: String,
    write_mb_s: f64,
    verify_mb_s: f64,
    total_secs: f64,
}

/// Pre-allocate the target tempfile to `IMAGE_BYTES` so writer impls that
/// `open()` without `O_CREAT` (block, pipelined-raw) can find it.
fn pre_allocate(path: &Path) -> std::io::Result<()> {
    let f = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)?;
    f.set_len(IMAGE_BYTES as u64)?;
    Ok(())
}

fn run_one(label: &str, io: &dyn DeviceIo, source: &mut MemImageReader) -> Row {
    let dir = tempdir().expect("tempdir");
    let target = dir.path().join("bench.img");
    pre_allocate(&target).expect("pre-allocate target");

    let cancel = AtomicBool::new(false);

    // Burn.
    source.rewind();
    let writer = io.open_write(&target).expect("open_write");
    let write_started = Instant::now();
    let burn_result = pipeline::burn(source, writer, DEFAULT_CHUNK, &cancel, |_| {})
        .expect("burn succeeds");
    let write_elapsed = write_started.elapsed();

    // Verify (hash-only read-back through the same impl).
    let mut device_reader = io.open_read(&target).expect("open_read");
    let verify_started = Instant::now();
    let _ = pipeline::verify_hash_only(
        &mut *device_reader,
        burn_result.bytes_written,
        DEFAULT_CHUNK,
        &cancel,
        |_| {},
    )
    .expect("verify succeeds");
    let verify_elapsed = verify_started.elapsed();

    Row {
        label: label.to_string(),
        write_mb_s: throughput_mb_s(burn_result.bytes_written, write_elapsed),
        verify_mb_s: throughput_mb_s(burn_result.bytes_written, verify_elapsed),
        total_secs: (write_elapsed + verify_elapsed).as_secs_f64(),
    }
}

fn throughput_mb_s(bytes: u64, elapsed: Duration) -> f64 {
    let secs = elapsed.as_secs_f64().max(0.001);
    (bytes as f64 / 1_000_000.0) / secs
}

fn print_table(rows: &[Row]) {
    println!("Disk Cutter benchmark — synthetic 256 MiB image");
    println!(
        "host: {}/{}",
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    println!("chunk size: 1 MiB");
    println!();
    println!(
        "{:<34}{:<14}{:<15}total",
        "impl", "write MB/s", "verify MB/s"
    );
    for r in rows {
        println!(
            "{:<34}{:<14.1}{:<15.1}{:.2} s",
            r.label, r.write_mb_s, r.verify_mb_s, r.total_secs
        );
    }
}

fn main() {
    println!("Generating synthetic 256 MiB image (seed={:#018x})…", SEED);
    let payload = deterministic_bytes(IMAGE_BYTES, SEED);
    let mut source = MemImageReader::new(payload);

    let mut rows = Vec::new();

    println!("Running plain…");
    rows.push(run_one("plain", &PlainFileDeviceIo, &mut source));

    #[cfg(unix)]
    {
        println!("Running block…");
        rows.push(run_one("block", &BlockDeviceIo, &mut source));

        for (workers, depth) in [(1usize, 4usize), (2, 8), (4, 15), (8, 32)] {
            let label = format!("pipelined (workers={workers}, depth={depth})");
            println!("Running {label}…");
            let io = PipelinedRawDeviceIo::new(workers, depth);
            rows.push(run_one(&label, &io, &mut source));
        }
    }

    println!();
    print_table(&rows);
}
