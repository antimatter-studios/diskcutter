//! Hash throughput benchmark — SHA-256 vs xxh64.
//!
//! Mirrors `examples/benchmark.rs` in spirit, but isolates the hash step:
//! generates a deterministic 512 MiB buffer, feeds it through each
//! `HashAlgo` via the `StreamingHasher` trait in 1 MiB chunks, and prints
//! a median-of-3 throughput table. The first run is kept (not discarded
//! as warmup) so the table reflects a realistic cold-cache call; the
//! median absorbs any one-off jitter.
//!
//! Run:
//!     cargo run --release --manifest-path src-tauri/Cargo.toml --example hash_bench
//!
//! Expectation on modern arm64: xxh64 lands in the multi-GB/s range,
//! SHA-256 in the hundreds of MB/s — roughly an order of magnitude gap.

use std::num::Wrapping;
use std::time::{Duration, Instant};

use diskcutter_lib::hash::{self, HashAlgo};

const BUFFER_BYTES: usize = 512 * 1024 * 1024;
const CHUNK_BYTES: usize = 1024 * 1024;
const RUNS: usize = 3;
const SEED: u64 = 0xD15C_C077_E72B_E5C4;

/// Deterministic byte stream (splitmix64-ish) — same output every run.
/// Copy-pasted from `examples/benchmark.rs`; examples don't share helpers.
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

/// One result row per algorithm.
struct Row {
    label: &'static str,
    median_ms: f64,
    mb_s: f64,
    digest: String,
}

fn throughput_mb_s(bytes: u64, elapsed: Duration) -> f64 {
    let secs = elapsed.as_secs_f64().max(1e-9);
    (bytes as f64 / 1_000_000.0) / secs
}

/// Feed the buffer through a fresh hasher in 1 MiB chunks, returning the
/// total elapsed (update + finalize) and the final hex digest.
fn run_once(algo: HashAlgo, payload: &[u8]) -> (Duration, String) {
    let mut hasher = hash::new(algo);
    let started = Instant::now();
    for chunk in payload.chunks(CHUNK_BYTES) {
        hasher.update(chunk);
    }
    let digest = hasher.finalize_hex();
    (started.elapsed(), digest)
}

fn bench(label: &'static str, algo: HashAlgo, payload: &[u8]) -> Row {
    let mut times = Vec::with_capacity(RUNS);
    let mut digest = String::new();
    for i in 0..RUNS {
        let (elapsed, hex) = run_once(algo, payload);
        println!(
            "  {label} run {} → {:.1} ms",
            i + 1,
            elapsed.as_secs_f64() * 1000.0
        );
        times.push(elapsed);
        digest = hex;
    }
    times.sort();
    let median = times[times.len() / 2];
    Row {
        label,
        median_ms: median.as_secs_f64() * 1000.0,
        mb_s: throughput_mb_s(payload.len() as u64, median),
        digest,
    }
}

fn print_table(rows: &[Row]) {
    println!("Disk Cutter hash benchmark — synthetic 512 MiB buffer");
    println!("host: {}/{}", std::env::consts::OS, std::env::consts::ARCH);
    println!("chunk size: 1 MiB, runs: {RUNS} (median reported)");
    println!();
    println!("{:<10}{:<14}{:<14}digest", "algo", "median ms", "MB/s");
    for r in rows {
        println!(
            "{:<10}{:<14.1}{:<14.1}{}",
            r.label, r.median_ms, r.mb_s, r.digest
        );
    }
}

fn main() {
    println!("Generating synthetic 512 MiB buffer (seed={:#018x})…", SEED);
    let payload = deterministic_bytes(BUFFER_BYTES, SEED);

    let mut rows = Vec::new();

    println!("Running sha256…");
    rows.push(bench("sha256", HashAlgo::Sha256, &payload));

    println!("Running xxh64…");
    rows.push(bench("xxh64", HashAlgo::Xxhash, &payload));

    println!();
    print_table(&rows);
}
