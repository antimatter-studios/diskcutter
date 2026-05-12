// Pipelined raw-device writer. Mirrors Etcher's design: ~16 in-flight 1 MiB
// writes via a worker-thread pool, with macOS `F_NOCACHE` to skip the unified
// buffer cache. Produces dramatically higher sustained USB throughput than a
// single-threaded `write_all` loop because the kernel/USB driver queue stays
// full instead of round-tripping per chunk.
//
// Architecture:
//   producer (Write::write)  →  sync_channel(QUEUE_DEPTH)  →  N writer threads
//                                                              ↓
//                                                       Arc<File> (pwrite at offset)
//
// The producer hands ownership of each buffer to the channel; writer threads
// pull and `pwrite` at the right offset. pwrite lets multiple threads share
// one FD without locking the file position. fsync happens once in `finish()`.

#![cfg(unix)]

use std::fs::{File, OpenOptions};
use std::io::{Read, Result, Write};
use std::os::unix::fs::{FileExt, OpenOptionsExt};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread::JoinHandle;

use super::{DeviceIo, DeviceReader, DeviceWriter};

pub struct PipelinedRawDeviceIo {
    pub worker_threads: usize,
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
        Self::new(4, 15)
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
        let file = opts.open(&target)?;

        // Disable the unified buffer cache so writes go straight to the device
        // (macOS-specific; F_NOCACHE has no Linux equivalent and isn't needed).
        #[cfg(target_os = "macos")]
        unsafe {
            libc::fcntl(file.as_raw_fd(), libc::F_NOCACHE, 1);
        }

        let file = Arc::new(file);
        let (tx, rx) = mpsc::sync_channel::<WriteJob>(self.queue_depth);
        let rx = Arc::new(std::sync::Mutex::new(rx));
        let errors = Arc::new(std::sync::Mutex::new(Vec::<std::io::Error>::new()));

        let mut threads = Vec::with_capacity(self.worker_threads);
        for _ in 0..self.worker_threads {
            let file = file.clone();
            let rx = rx.clone();
            let errors = errors.clone();
            threads.push(std::thread::spawn(move || loop {
                let job = match rx.lock().unwrap().recv() {
                    Ok(j) => j,
                    Err(_) => break, // channel closed
                };
                if let Err(e) = file.write_all_at(&job.data, job.offset) {
                    errors.lock().unwrap().push(e);
                    break;
                }
            }));
        }

        Ok(Box::new(PipelinedWriter {
            tx: Some(tx),
            file,
            offset: 0,
            threads,
            errors,
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

pub struct PipelinedWriter {
    tx: Option<mpsc::SyncSender<WriteJob>>,
    file: Arc<File>,
    offset: u64,
    threads: Vec<JoinHandle<()>>,
    errors: Arc<std::sync::Mutex<Vec<std::io::Error>>>,
}

impl Write for PipelinedWriter {
    fn write(&mut self, buf: &[u8]) -> Result<usize> {
        // Surface any worker error eagerly so the burn aborts on first I/O fault.
        if let Some(e) = self.errors.lock().unwrap().pop() {
            return Err(e);
        }
        let len = buf.len();
        let job = WriteJob {
            offset: self.offset,
            data: buf.to_vec(),
        };
        if let Some(tx) = &self.tx {
            tx.send(job).map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::BrokenPipe, "writer thread gone")
            })?;
        }
        self.offset += len as u64;
        Ok(len)
    }

    fn flush(&mut self) -> Result<()> {
        // Real flush happens in finish() — we can't block here without
        // draining all threads, which would defeat the pipeline.
        Ok(())
    }
}

impl DeviceWriter for PipelinedWriter {
    fn finish(mut self: Box<Self>) -> Result<()> {
        // Close the channel so workers exit after draining.
        self.tx.take();
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
        if let Some(e) = self.errors.lock().unwrap().pop() {
            return Err(e);
        }
        // sync_all on the Arc<File> via underlying fd.
        let fd = self.file.as_raw_fd();
        let ret = unsafe { libc::fsync(fd) };
        if ret != 0 {
            return Err(std::io::Error::last_os_error());
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
