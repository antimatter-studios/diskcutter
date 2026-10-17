// Parallel-pwrite raw-device writer. Mechanically identical to
// `writers/pipelined.rs` (buffer pool, F_NOCACHE on rdisk, F_NOCACHE
// QoS hoist, etc.) but with **N** worker threads pulling jobs from a
// lock-free MPMC channel (`crossbeam_channel`) so the kernel can have
// up to N pwrite syscalls in flight against the same FD.
//
// Why this is a separate writer impl rather than a tweak to the
// pipelined writer:
//
// On most USB-MSC reader+card combos, the device's reported per-pwrite
// max (DKIOCGETMAXBYTECOUNTWRITE — often 4 MiB) caps single-worker
// throughput to whatever a 4 MiB pwrite takes in wall-clock. Whether
// concurrent pwrites against the same FD lift the ceiling depends on
// the reader supporting overlapping SCSI WRITE commands (tagged
// command queueing). It's reader-firmware-specific and not portable.
// Keeping it as a `writer.impl = parallel` opt-in lets us A/B against
// the known-good `pipelined` baseline without risking a regression
// for users whose hardware doesn't benefit.
//
// Architecture differs from `pipelined.rs` only in two places:
//   1. The work channel is `crossbeam_channel::bounded` (MPMC),
//      cloned into each worker so multiple recv()s proceed in
//      parallel without a mutex.
//   2. N worker threads spawned, each running the same loop. All
//      share one `Arc<File>`; each `file.write_all_at(buf, offset)`
//      is independent of the others (pwrite carries its own offset,
//      so concurrent writes to disjoint offsets don't race).

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

use crossbeam_channel::{bounded, Receiver as XRecv, Sender as XSender};

use super::{DeviceIo, DeviceReader, DeviceWriter};

pub struct ParallelRawDeviceIo {
    pub worker_threads: usize,
    pub queue_depth: usize,
}

impl ParallelRawDeviceIo {
    pub fn new(worker_threads: usize, queue_depth: usize) -> Self {
        Self {
            // Sanity-clamp: zero workers would hang the producer
            // immediately. One worker degenerates into the same shape
            // as the single-writer pipelined impl — allow it, that's
            // useful for A/B benchmarking.
            worker_threads: worker_threads.max(1),
            queue_depth,
        }
    }
}

impl DeviceIo for ParallelRawDeviceIo {
    fn name(&self) -> &'static str {
        "raw-parallel"
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

        // F_NOCACHE: redundant on rdisk (already raw) but harmless and
        // matches Etcher's flag set. See `writers/pipelined.rs` for
        // the long-form discussion.
        #[cfg(target_os = "macos")]
        unsafe {
            libc::fcntl(file.as_raw_fd(), libc::F_NOCACHE, 1);
        }

        // The work channel needs to be MPMC: N workers consume from
        // it. crossbeam's `bounded` is lock-free for the typical
        // contention pattern (much faster than `Arc<Mutex<Receiver>>`).
        // Capacity = queue_depth + workers gives the producer enough
        // headroom that it almost never blocks on send: it can keep
        // `queue_depth` buffers queued plus `workers` actively being
        // pwritten by the workers.
        let pool_size = self.queue_depth.max(self.worker_threads).max(1);
        let work_capacity = pool_size + self.worker_threads;
        let (work_tx, work_rx): (XSender<WriteJob>, XRecv<WriteJob>) = bounded(work_capacity);

        // Free pool is single-consumer (the producer pulls empties)
        // but multi-producer (each worker returns its used buffer).
        // std mpsc::sync_channel handles this exactly: SyncSender is
        // Clone, Receiver is single.
        let (free_tx, free_rx) = mpsc::sync_channel::<Vec<u8>>(pool_size);
        for _ in 0..pool_size {
            free_tx.send(Vec::new()).expect("free pool prefill");
        }

        let file = Arc::new(file);
        let errors = Arc::new(std::sync::Mutex::new(Vec::<std::io::Error>::new()));
        let stats = Arc::new(PwriteStats::default());

        let mut workers = Vec::with_capacity(self.worker_threads);
        for _ in 0..self.worker_threads {
            let file = file.clone();
            let errors = errors.clone();
            let free_tx_worker = free_tx.clone();
            let stats = stats.clone();
            let work_rx = work_rx.clone();
            workers.push(std::thread::spawn(move || {
                // Same QoS / IO-policy hoist as pipelined's worker.
                #[cfg(target_os = "macos")]
                unsafe {
                    set_worker_priorities();
                }
                while let Ok(job) = work_rx.recv() {
                    let started = Instant::now();
                    if let Err(e) = file.write_all_at(&job.data, job.offset) {
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
                        // First failing worker drops out of the loop.
                        // Surviving workers continue draining the
                        // queue so the producer doesn't block forever
                        // on a full work channel; the producer will
                        // pick up the queued error on its next write.
                        break;
                    }
                    let elapsed_us = started.elapsed().as_micros() as u64;
                    let bytes = job.data.len() as u64;
                    stats.record_pwrite(elapsed_us, bytes);
                    stats.maybe_flush();
                    let _ = free_tx_worker.send(job.data);
                }
            }));
        }

        // Drop the producer-side copies of the work_rx clone so that
        // when the producer drops work_tx in `finish()`, workers see
        // disconnection promptly.
        drop(work_rx);

        Ok(Box::new(ParallelWriter {
            work_tx: Some(work_tx),
            free_rx,
            file,
            offset: 0,
            workers,
            errors,
            #[allow(dead_code)]
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

#[cfg(target_os = "macos")]
unsafe fn set_worker_priorities() {
    #[allow(non_camel_case_types)]
    type qos_class_t = u32;
    const QOS_CLASS_USER_INTERACTIVE: qos_class_t = 0x21;
    extern "C" {
        fn pthread_set_qos_class_self_np(qc: qos_class_t, relative_priority: i32) -> i32;
    }
    let _ = pthread_set_qos_class_self_np(QOS_CLASS_USER_INTERACTIVE, 0);

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
    data: Vec<u8>,
}

pub struct ParallelWriter {
    work_tx: Option<XSender<WriteJob>>,
    free_rx: mpsc::Receiver<Vec<u8>>,
    file: Arc<File>,
    offset: u64,
    workers: Vec<JoinHandle<()>>,
    errors: Arc<std::sync::Mutex<Vec<std::io::Error>>>,
    /// Retained on the writer struct so its `Arc` lifetime is at least
    /// as long as the workers that mutate the same counters. Read-only
    /// from the producer's perspective.
    #[allow(dead_code)]
    stats: Arc<PwriteStats>,
}

/// Per-worker diagnostic counters with the same on-disk format as
/// `writers::pipelined::PwriteStats`. We deliberately log to a
/// distinct file (`/tmp/disk-cutter-parallel-pwrite-stats.log`) so an
/// A/B test against the single-worker pipelined impl produces two
/// separate streams to compare side-by-side. Identical semantics
/// otherwise — duplicated rather than abstracted to keep both writer
/// files readable in isolation.
#[derive(Default)]
struct PwriteStats {
    count: AtomicU64,
    total_us: AtomicU64,
    max_us: AtomicU64,
    bytes: AtomicU64,
    last_count: AtomicU64,
    last_total_us: AtomicU64,
    last_bytes: AtomicU64,
}

const STATS_FLUSH_EVERY: u64 = 32;
const PARALLEL_PWRITE_STATS_LOG_PATH: &str = "/tmp/disk-cutter-parallel-pwrite-stats.log";

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
        // `mbps` here is the device-bound implied throughput
        // *summed across workers* — i.e. if the workers can all
        // overlap their pwrites perfectly, this is the device's
        // sustained MB/s ceiling. If overlap is partial the value
        // will exceed observed wall-clock throughput.
        let mbps = if win_total_us > 0 {
            (win_bytes as f64) / (win_total_us as f64)
        } else {
            0.0
        };

        use std::io::Write as _;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(PARALLEL_PWRITE_STATS_LOG_PATH)
        {
            let _ = writeln!(
                f,
                "parallel_pwrite_window: chunks={win_count} avg_us={avg_us} max_us={max} \
                 win_bytes={win_bytes} mbps_summed={mbps:.2} \
                 cumulative_chunks={count} cumulative_bytes={bytes}"
            );
        }
    }
}

impl Write for ParallelWriter {
    fn write(&mut self, buf: &[u8]) -> Result<usize> {
        if let Some(e) = self.errors.lock().unwrap().pop() {
            return Err(e);
        }
        let mut owned = self.free_rx.recv().map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "writer threads gone (free pool closed)",
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
                std::io::Error::new(std::io::ErrorKind::BrokenPipe, "writer threads gone")
            })?;
        }
        self.offset += len as u64;
        Ok(len)
    }

    fn flush(&mut self) -> Result<()> {
        Ok(())
    }
}

impl DeviceWriter for ParallelWriter {
    fn finish(mut self: Box<Self>) -> Result<()> {
        // Close the work channel so workers see disconnection after
        // draining whatever is still queued, then join all of them.
        self.work_tx.take();
        for t in self.workers.drain(..) {
            let _ = t.join();
        }
        if let Some(e) = self.errors.lock().unwrap().pop() {
            return Err(e);
        }
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
    fn parallel_device_io_name() {
        assert_eq!(ParallelRawDeviceIo::new(4, 16).name(), "raw-parallel");
    }

    #[test]
    fn parallel_clamps_zero_workers_to_one() {
        let io = ParallelRawDeviceIo::new(0, 16);
        assert_eq!(io.worker_threads, 1);
    }

    /// End-to-end round-trip with N=4 workers writing to a temp file.
    /// The producer hands 12 chunks of distinct payload through; with
    /// 4 workers and a pool of 8, recycling MUST happen for the write
    /// to complete. Any worker hang or pool starvation would deadlock
    /// the test.
    #[test]
    fn parallel_writer_round_trips_through_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("parallel-roundtrip.bin");
        std::fs::File::create(&path).unwrap();
        let io = ParallelRawDeviceIo::new(4, 8);
        let mut w = io.open_write(&path).unwrap();
        for i in 0u8..12 {
            let chunk = vec![i; 1024];
            w.write_all(&chunk).unwrap();
        }
        w.finish().unwrap();
        let actual = std::fs::read(&path).unwrap();
        assert_eq!(actual.len(), 12 * 1024);
        for (i, slice) in actual.chunks(1024).enumerate() {
            assert!(
                slice.iter().all(|&b| b as usize == i),
                "chunk {i} bytes mismatch"
            );
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn translate_to_raw_inserts_r_prefix() {
        assert_eq!(
            translate_to_raw(&PathBuf::from("/dev/disk5")),
            PathBuf::from("/dev/rdisk5")
        );
    }
}
