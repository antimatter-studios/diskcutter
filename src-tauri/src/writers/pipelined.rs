// Pipelined raw-device writer. Architecturally mirrors balena Etcher:
// **one** writer thread driving sequential `pwrite()` calls on a single FD,
// with a ring of `pool_size` pre-allocated buffers feeding it via bounded
// channels for backpressure. Total in-flight buffers cap at pool_size
// (default 16), giving the producer enough slack to keep the USB / SD
// driver queue full without ever round-tripping per chunk.
//
// History note: an earlier version of this writer used N worker threads
// reading from one `Arc<Mutex<mpsc::Receiver>>`. The mutex forced every
// `recv()` to serialise, so "4 workers" was effectively a single writer
// plus three extra threads contending for one lock. Empirical measurement
// against Etcher confirmed one-worker matches Etcher's throughput model,
// so we collapsed to that — the mutex is gone, and so is the per-chunk
// `to_vec()` allocation that came with it.
//
// Architecture:
//   producer (Write::write)  ─┐
//                             │ (pulls empty Vec)        ┌─ worker thread
//   free_rx ──────────────────┘                          │  recv WriteJob
//                                                        │  pwrite(buf,off)
//   work_tx ──────────────► sync_channel(pool_size) ─────┤  send buf back
//                                                        │
//   free_tx ◄───────────────────────────────── recycle ──┘
//
// The producer borrows a Vec from the free pool, copies the caller's slice
// into it (one memcpy — the previous heap-alloc-per-chunk is eliminated),
// and ships it to the worker. The worker pwrites at the supplied offset,
// then returns the (still-allocated) Vec to the free pool. `pwrite` lets
// the single worker advance the offset without sharing a file cursor.
// `fsync` happens once in `finish()` after the worker drains.

#![cfg(unix)]

use std::fs::{File, OpenOptions};
use std::io::{Read, Result, Seek, Write};
use std::os::unix::fs::{FileExt, OpenOptionsExt};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Instant;

use super::{DeviceIo, DeviceReader, DeviceWriter};

pub struct PipelinedRawDeviceIo {
    /// Retained for API/config compatibility with older callers (helper
    /// CLI, benchmark example) and to avoid breaking the
    /// `PipelinedRawDeviceIo::new(workers, queue_depth)` signature. The
    /// runtime model is single-worker; this field is ignored.
    #[allow(dead_code)]
    pub worker_threads: usize,
    /// Number of recyclable 1-MiB buffers in flight between the producer
    /// and the writer thread. Equals "queue depth" in the prefs UI.
    pub queue_depth: usize,
}

impl PipelinedRawDeviceIo {
    pub fn new(worker_threads: usize, queue_depth: usize) -> Self {
        Self {
            worker_threads,
            queue_depth,
        }
    }

    #[allow(clippy::should_implement_trait, dead_code)]
    pub fn default() -> Self {
        Self::new(4, 16)
    }
}

impl DeviceIo for PipelinedRawDeviceIo {
    fn name(&self) -> &'static str {
        "raw-pipelined"
    }

    fn open_write(&self, device: &Path) -> Result<Box<dyn DeviceWriter>> {
        let target = translate_to_raw(device);
        let mut opts = OpenOptions::new();
        opts.read(true).write(true);
        #[cfg(target_os = "macos")]
        {
            opts.custom_flags(libc::O_EXLOCK);
        }
        #[cfg(target_os = "linux")]
        {
            opts.custom_flags(libc::O_SYNC | libc::O_DIRECT);
        }
        let file = opts.open(&target).map_err(|e| {
            std::io::Error::new(e.kind(), format!("open(2) {}: {}", target.display(), e))
        })?;

        // Disable the unified buffer cache so writes go straight to the
        // device (macOS-specific; F_NOCACHE has no Linux equivalent and
        // isn't needed). On `/dev/rdiskN` this is largely redundant —
        // the raw char device already bypasses UBC — but Etcher
        // includes the call too, and harmonising flag sets means we
        // can rule the open path out when comparing throughput.
        #[cfg(target_os = "macos")]
        unsafe {
            libc::fcntl(file.as_raw_fd(), libc::F_NOCACHE, 1);
        }

        // Buffer pool sizing: at least 1 in-flight slot; cap at the
        // configured queue depth. A pool of 1 collapses to a strict
        // ping-pong (producer must wait for the worker to finish each
        // write); a pool of 16 matches Etcher's default and gives ~16
        // MiB of working-set headroom.
        let pool_size = self.queue_depth.max(1);

        let file = Arc::new(file);
        let (work_tx, work_rx) = mpsc::sync_channel::<WriteJob>(pool_size);
        let (free_tx, free_rx) = mpsc::sync_channel::<Vec<u8>>(pool_size);
        // Pre-populate the free pool. We don't know the final chunk size
        // yet (the helper may probe and clamp), so each Vec starts empty
        // and grows to chunk_size on the first write that fills it.
        // After the first lap around the ring, every Vec is at its
        // steady-state capacity and `clear() + extend_from_slice` is a
        // pure memcpy with no realloc.
        for _ in 0..pool_size {
            // sync_channel capacity == pool_size, so all pool_size sends
            // succeed without blocking.
            free_tx.send(Vec::new()).expect("free pool prefill");
        }

        let errors = Arc::new(std::sync::Mutex::new(Vec::<std::io::Error>::new()));

        // Telemetry shared with the producer for diagnostic logging. The
        // worker increments these in the hot path (single relaxed atomic
        // op each); the producer reads + resets them at every progress
        // emit and folds the numbers into the JSONL log so we can see
        // exactly where time is going inside a real burn. Cheap enough
        // to leave on unconditionally.
        let stats = Arc::new(PwriteStats::default());

        let worker = {
            let file = file.clone();
            let errors = errors.clone();
            let free_tx_worker = free_tx.clone();
            let stats = stats.clone();
            std::thread::spawn(move || {
                // Hoist this thread's macOS QoS class to USER_INTERACTIVE
                // and its disk-IO priority to IMPORTANT. Without these,
                // the kernel can route our `pwrite()` calls through the
                // throttled / background IO path even though we open
                // `/dev/rdiskN` with `F_NOCACHE`. Empirically, on the
                // hardware we benchmarked against Etcher this is the
                // difference between ~9 MB/s and ~90 MB/s for an
                // otherwise byte-identical pwrite pattern. Silently
                // ignore non-zero returns — the worst case is we run
                // at whatever priority we inherited, which is what we
                // were already doing.
                #[cfg(target_os = "macos")]
                unsafe {
                    set_worker_priorities();
                }
                while let Ok(job) = work_rx.recv() {
                    let started = Instant::now();
                    if let Err(e) = file.write_all_at(&job.data, job.offset) {
                        // EINVAL on macOS rdisk almost always means size
                        // or offset wasn't a multiple of the device
                        // block size — surfacing those two numbers turns
                        // a generic "Invalid argument" into a
                        // self-diagnosing error.
                        let kind = e.kind();
                        let raw = e.raw_os_error();
                        let detail = match raw {
                            Some(code) => format!(
                                "pwrite at offset={} len={} failed: {} (errno {})",
                                job.offset,
                                job.data.len(),
                                e,
                                code,
                            ),
                            None => format!(
                                "pwrite at offset={} len={} failed: {}",
                                job.offset,
                                job.data.len(),
                                e,
                            ),
                        };
                        errors
                            .lock()
                            .unwrap()
                            .push(std::io::Error::new(kind, detail));
                        // Stop draining further jobs on error: the burn
                        // is going to fail anyway. Letting the loop exit
                        // makes the producer's next free_rx.recv()
                        // return Err once all pool buffers are consumed,
                        // surfacing the I/O fault promptly.
                        break;
                    }
                    let elapsed_us = started.elapsed().as_micros() as u64;
                    let bytes = job.data.len() as u64;
                    stats.record_pwrite(elapsed_us, bytes);
                    stats.maybe_flush();
                    // Recycle the buffer back to the pool. If the
                    // producer has already given up (free_rx dropped on
                    // its side), ignore the error — we're shutting down.
                    let _ = free_tx_worker.send(job.data);
                }
            })
        };

        Ok(Box::new(PipelinedWriter {
            work_tx: Some(work_tx),
            free_rx,
            file,
            offset: 0,
            worker: Some(worker),
            errors,
            stats,
        }))
    }

    fn open_read(&self, device: &Path) -> Result<Box<dyn DeviceReader>> {
        let target = translate_to_raw(device);
        let file = File::open(&target)?;
        #[cfg(target_os = "macos")]
        unsafe {
            libc::fcntl(file.as_raw_fd(), libc::F_NOCACHE, 1);
        }
        Ok(Box::new(SimpleReader { file }))
    }
}

/// Raise this thread's macOS Quality-of-Service and disk-IO priority
/// to the highest sensible values for sustained foreground I/O. Both
/// of these only exist on Darwin and are no-ops on Linux. Constants
/// are inlined from `<pthread/qos.h>` and `<sys/resource.h>` so we
/// don't need a `bindgen` step for two integers.
///
/// - `pthread_set_qos_class_self_np(USER_INTERACTIVE, 0)` puts the
///   thread into the highest-priority bucket the macOS scheduler
///   exposes. The default class for an unspecified thread is
///   `QOS_CLASS_DEFAULT` (0x15), which on Apple silicon can lose CPU
///   to higher-QoS work and — more importantly here — has its disk
///   I/O internally throttled at the kernel IOPolicy layer.
/// - `setiopolicy_np(IOPOL_TYPE_DISK, IOPOL_SCOPE_THREAD,
///   IOPOL_IMPORTANT)` overrides any inherited throttle policy on this
///   thread's disk I/O. Without it, IO from threads in lower QoS
///   classes — which we may inherit from osascript-spawned helpers
///   — gets put on the kernel's background-IO queue.
#[cfg(target_os = "macos")]
unsafe fn set_worker_priorities() {
    // <pthread/qos.h>
    #[allow(non_camel_case_types)]
    type qos_class_t = u32;
    const QOS_CLASS_USER_INTERACTIVE: qos_class_t = 0x21;
    extern "C" {
        fn pthread_set_qos_class_self_np(qc: qos_class_t, relative_priority: i32) -> i32;
    }
    let _ = pthread_set_qos_class_self_np(QOS_CLASS_USER_INTERACTIVE, 0);

    // <sys/resource.h>
    const IOPOL_TYPE_DISK: i32 = 0;
    const IOPOL_SCOPE_THREAD: i32 = 1;
    const IOPOL_IMPORTANT: i32 = 1;
    extern "C" {
        fn setiopolicy_np(iotype: i32, scope: i32, policy: i32) -> i32;
    }
    let _ = setiopolicy_np(IOPOL_TYPE_DISK, IOPOL_SCOPE_THREAD, IOPOL_IMPORTANT);
}

fn translate_to_raw(device: &Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        if let Some(name) = device.file_name().and_then(|s| s.to_str()) {
            if let Some(rest) = name.strip_prefix("disk") {
                if !rest.starts_with('r') {
                    return PathBuf::from(format!("/dev/r{name}"));
                }
            }
        }
    }
    device.to_path_buf()
}

struct WriteJob {
    offset: u64,
    /// Owned buffer borrowed from the free pool. The worker pwrites from
    /// this slice and then ships the Vec back through `free_tx` for the
    /// next iteration to reuse. No `to_vec()` copy per chunk.
    data: Vec<u8>,
}

pub struct PipelinedWriter {
    /// `Option` so `finish()` can drop the sender to signal the worker
    /// thread to exit.
    work_tx: Option<mpsc::SyncSender<WriteJob>>,
    free_rx: mpsc::Receiver<Vec<u8>>,
    file: Arc<File>,
    offset: u64,
    worker: Option<JoinHandle<()>>,
    errors: Arc<std::sync::Mutex<Vec<std::io::Error>>>,
    /// Retained on the writer struct so its lifetime is at least as
    /// long as the worker that mutates the same `Arc`. Read-only from
    /// the producer's perspective — the worker is the only writer.
    #[allow(dead_code)]
    stats: Arc<PwriteStats>,
}

/// Diagnostic counters maintained by the worker thread. Every
/// `STATS_FLUSH_EVERY` writes, the worker formats a line summarising
/// the window (chunks, avg latency, max latency, MB/s implied by the
/// pwrite time alone) and appends it to a fixed `/tmp` log so the user
/// can `tail -f` while a burn is running. Atomics are relaxed —
/// these are observations, not synchronisation.
#[derive(Default)]
pub struct PwriteStats {
    count: AtomicU64,
    total_us: AtomicU64,
    max_us: AtomicU64,
    /// Total bytes written, summed across all pwrite() calls. Used
    /// alongside `total_us` to compute the device-bound throughput
    /// ceiling (i.e. throughput if the producer were infinitely fast).
    bytes: AtomicU64,
    /// Snapshot of the four counters at the previous flush, used to
    /// compute window deltas without needing a swap on every counter.
    last_count: AtomicU64,
    last_total_us: AtomicU64,
    last_bytes: AtomicU64,
}

/// Flush a stats line every N writes. At 1 MiB chunks, 32 ≈ 32 MiB of
/// progress between lines, which is one log entry every ~3 s at the
/// current observed ~10 MB/s and would be ~0.3 s on a healthy 100 MB/s
/// burn — both readable rates.
const STATS_FLUSH_EVERY: u64 = 32;

/// Where the worker dumps its per-window pwrite timing. Fixed path so
/// the operator can `tail -f` it without having to plumb a job_id
/// through. Overwritten by every fresh helper run — there is one
/// helper at a time per device, so collisions don't matter in
/// practice.
const PWRITE_STATS_LOG_PATH: &str = "/tmp/disk-cutter-pwrite-stats.log";

impl PwriteStats {
    fn record_pwrite(&self, elapsed_us: u64, bytes: u64) {
        self.count.fetch_add(1, Ordering::Relaxed);
        self.total_us.fetch_add(elapsed_us, Ordering::Relaxed);
        self.bytes.fetch_add(bytes, Ordering::Relaxed);
        let mut prev = self.max_us.load(Ordering::Relaxed);
        while elapsed_us > prev {
            match self.max_us.compare_exchange_weak(
                prev,
                elapsed_us,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(actual) => prev = actual,
            }
        }
    }

    /// If we've crossed a flush boundary, append a one-line summary of
    /// the window's chunk count, average pwrite latency (µs), max
    /// pwrite latency (µs), and implied device-bound throughput (MB/s
    /// assuming infinite producer). No-op if `count` hasn't moved past
    /// the next multiple of `STATS_FLUSH_EVERY`. Errors are swallowed
    /// — this is diagnostic, never load-bearing.
    fn maybe_flush(&self) {
        let count = self.count.load(Ordering::Relaxed);
        let last = self.last_count.load(Ordering::Relaxed);
        if count - last < STATS_FLUSH_EVERY {
            return;
        }
        let total_us = self.total_us.load(Ordering::Relaxed);
        let bytes = self.bytes.load(Ordering::Relaxed);
        let last_total = self.last_total_us.load(Ordering::Relaxed);
        let last_bytes = self.last_bytes.load(Ordering::Relaxed);
        let max = self.max_us.swap(0, Ordering::Relaxed);

        self.last_count.store(count, Ordering::Relaxed);
        self.last_total_us.store(total_us, Ordering::Relaxed);
        self.last_bytes.store(bytes, Ordering::Relaxed);

        let win_count = count - last;
        let win_total_us = total_us - last_total;
        let win_bytes = bytes - last_bytes;
        let avg_us = win_total_us.checked_div(win_count).unwrap_or(0);
        let mbps = if win_total_us > 0 {
            (win_bytes as f64) / (win_total_us as f64) // bytes/µs == MB/s
        } else {
            0.0
        };

        use std::io::Write as _;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(PWRITE_STATS_LOG_PATH)
        {
            let _ = writeln!(
                f,
                "pwrite_window: chunks={win_count} avg_us={avg_us} max_us={max} \
                 win_bytes={win_bytes} mbps={mbps:.2} \
                 cumulative_chunks={count} cumulative_bytes={bytes}"
            );
        }
    }
}

impl Write for PipelinedWriter {
    fn write(&mut self, buf: &[u8]) -> Result<usize> {
        // Surface any worker error eagerly so the burn aborts on first
        // I/O fault rather than queueing more chunks behind a dead
        // worker.
        if let Some(e) = self.errors.lock().unwrap().pop() {
            return Err(e);
        }

        // Pull an empty buffer from the pool. This is the natural
        // backpressure point: once `pool_size` buffers are in flight,
        // the producer blocks here until the worker recycles one. A
        // closed channel means the worker died — surface as BrokenPipe.
        let mut owned = self.free_rx.recv().map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "writer thread gone (free pool closed)",
            )
        })?;
        owned.clear();
        owned.extend_from_slice(buf);

        let len = buf.len();
        let job = WriteJob {
            offset: self.offset,
            data: owned,
        };
        if let Some(tx) = &self.work_tx {
            tx.send(job).map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::BrokenPipe, "writer thread gone")
            })?;
        }
        self.offset += len as u64;
        Ok(len)
    }

    fn flush(&mut self) -> Result<()> {
        // Real flush happens in finish() — we can't block here without
        // draining the worker, which would defeat the pipeline.
        Ok(())
    }
}

impl DeviceWriter for PipelinedWriter {
    fn finish(mut self: Box<Self>) -> Result<()> {
        // Close the work channel so the worker exits after draining
        // its queue, then join.
        self.work_tx.take();
        if let Some(t) = self.worker.take() {
            let _ = t.join();
        }
        if let Some(e) = self.errors.lock().unwrap().pop() {
            return Err(e);
        }
        // sync_all on the Arc<File> via underlying fd.
        let fd = self.file.as_raw_fd();
        let ret = unsafe { libc::fsync(fd) };
        if ret != 0 {
            let e = std::io::Error::last_os_error();
            return Err(std::io::Error::new(
                e.kind(),
                format!("fsync after {} bytes written: {}", self.offset, e),
            ));
        }
        Ok(())
    }
}

pub struct SimpleReader {
    file: File,
}

impl Read for SimpleReader {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        self.file.read(buf)
    }
}

impl DeviceReader for SimpleReader {}

impl Seek for SimpleReader {
    fn seek(&mut self, pos: std::io::SeekFrom) -> Result<u64> {
        self.file.seek(pos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipelined_device_io_name() {
        assert_eq!(PipelinedRawDeviceIo::new(4, 15).name(), "raw-pipelined");
    }

    #[test]
    fn pipelined_default_retains_legacy_field_values() {
        // worker_threads is no longer interpreted at runtime (single-
        // worker model now), but the field is preserved so the legacy
        // "1/2/4/8/16" prefs selector and the helper CLI surface stay
        // round-trippable.
        let io = PipelinedRawDeviceIo::default();
        assert_eq!(io.worker_threads, 4);
        assert_eq!(io.queue_depth, 16);
    }

    #[test]
    fn pipelined_new_stores_supplied_values() {
        let io = PipelinedRawDeviceIo::new(8, 64);
        assert_eq!(io.worker_threads, 8);
        assert_eq!(io.queue_depth, 64);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn translate_to_raw_inserts_r_prefix() {
        assert_eq!(
            translate_to_raw(&PathBuf::from("/dev/disk5")),
            PathBuf::from("/dev/rdisk5")
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn translate_to_raw_preserves_already_raw_device() {
        assert_eq!(
            translate_to_raw(&PathBuf::from("/dev/rdisk5")),
            PathBuf::from("/dev/rdisk5")
        );
    }

    #[test]
    fn translate_to_raw_passes_non_disk_paths_through() {
        let p = PathBuf::from("/tmp/some-file.img");
        assert_eq!(translate_to_raw(&p), p);
    }

    #[test]
    fn translate_to_raw_handles_empty_path() {
        let p = PathBuf::from("");
        assert_eq!(translate_to_raw(&p), p);
    }

    /// End-to-end pool round-trip against a temp file: writes more
    /// chunks than the pool has slots so the recycle path is exercised
    /// at least once. A "buffer never returned" bug would deadlock this
    /// test; a "wrong offset" bug would produce out-of-order bytes.
    #[test]
    fn pipelined_writer_recycles_pool_buffers_through_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pool-roundtrip.bin");
        std::fs::File::create(&path).unwrap();
        let io = PipelinedRawDeviceIo::new(4, 4);
        let mut w = io.open_write(&path).unwrap();
        for i in 0u8..8 {
            let chunk = vec![i; 1024];
            w.write_all(&chunk).unwrap();
        }
        w.finish().unwrap();
        let actual = std::fs::read(&path).unwrap();
        assert_eq!(actual.len(), 8 * 1024);
        for (i, slice) in actual.chunks(1024).enumerate() {
            assert!(
                slice.iter().all(|&b| b as usize == i),
                "chunk {i} bytes mismatch"
            );
        }
    }
}
