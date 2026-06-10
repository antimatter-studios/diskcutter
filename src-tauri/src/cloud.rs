//! Cloud-storage source materialization.
//!
//! Files under macOS "File Provider" mounts (iCloud Drive, Google Drive,
//! Dropbox, OneDrive, …) can be *dataless* placeholders: the metadata is
//! resident locally but the bytes live in the cloud and are faulted in on
//! first read. Two burns reading the SAME dataless file concurrently can
//! drive the provider to return `EDEADLK` ("Resource deadlock avoided",
//! errno 11) and wedge that file so every subsequent read fails the same
//! way — which surfaces in the burn pipeline as a generic
//! "READ OR WRITE FAILED" I/O error long after the user has forgotten the
//! source was a cloud placeholder.
//!
//! We dodge the whole class by materializing the source once, in the parent
//! (user) process, BEFORE handing the path to the burn helper: a single
//! sequential read-through triggers an orderly fault-in, so by the time the
//! helper opens the file the bytes are local and warm. Doing it in the
//! parent also keeps the download as the invoking user (the File Provider is
//! per-session) rather than as the elevated root helper.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// macOS `st_flags` bit set on a File Provider placeholder whose content is
/// not resident locally. Mirrors `<sys/stat.h>`'s `SF_DATALESS`; `chflags`
/// renders it as the string `dataless`.
#[cfg(target_os = "macos")]
const SF_DATALESS: u32 = 0x4000_0000;

/// True if `path` is a cloud placeholder whose bytes are not resident
/// locally (macOS `SF_DATALESS`). Always false off macOS, and false when the
/// path can't be stat'd (a missing file is the burn's problem to report, not
/// ours).
pub fn is_dataless(path: &Path) -> bool {
    #[cfg(target_os = "macos")]
    {
        use std::os::macos::fs::MetadataExt;
        std::fs::metadata(path)
            .map(|m| m.st_flags() & SF_DATALESS != 0)
            .unwrap_or(false)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = path;
        false
    }
}

/// Number of concurrent read streams used to fault a cloud file in. They pull
/// blocks off one shared cursor that advances start→end, so the in-flight reads
/// form a sliding window at the download frontier rather than scattered offsets
/// across the file. That ordering matters: a File Provider materializes its
/// backing file roughly linearly, so a read far ahead of the frontier stalls
/// until the provider's own download reaches it — which is what made the
/// fixed-region approach hit `ETIMEDOUT`. Ten keeps the pipe full (an ~80 MiB
/// read-ahead window) without reading ahead of where the provider has data.
/// Overridable per burn via the `cloud.materialize_workers` config key so the
/// sweet spot can be tuned per provider/link without a rebuild.
pub const MATERIALIZE_WORKERS: usize = 10;

/// Bytes each worker claims and faults in per turn.
const MATERIALIZE_BLOCK: u64 = 8 * 1024 * 1024;

/// Attempts to fault a single block before giving up. File Provider reads
/// transiently time out under load on a long download (`os error 60`); a single
/// blip must not abort a multi-GB materialization, so retry the block with
/// backoff before surfacing the error.
const MATERIALIZE_BLOCK_ATTEMPTS: u32 = 10;

/// Cap on the per-retry backoff so a genuinely-stuck block doesn't sleep for
/// a minute on the later attempts (the doubling would otherwise reach ~64s).
const MATERIALIZE_BACKOFF_CAP: Duration = Duration::from_secs(5);

/// Force a dataless cloud file to fault in by reading every byte (the bytes are
/// discarded — the goal is purely to make the path resident so the burn's own
/// read is local).
///
/// Workers pull fixed-size blocks off a shared cursor rather than owning fixed
/// regions, so once the already-local prefix is consumed every worker converges
/// on the part that still needs the network — no stragglers carrying the slow
/// tail alone. Each block read is retried with backoff so a transient provider
/// timeout doesn't kill the whole download.
///
/// `on_progress(bytes_faulted)` is called from the calling thread as the shared
/// counter advances, so the caller's closure (which emits Tauri events) never
/// crosses a thread boundary. Returns [`std::io::ErrorKind::Interrupted`] when
/// `should_cancel` flips true; a block that fails every attempt propagates its
/// error verbatim (the wedged-provider `EDEADLK`, or `ETIMEDOUT`).
pub fn materialize(
    path: &Path,
    workers: usize,
    mut on_progress: impl FnMut(u64),
    should_cancel: impl Fn() -> bool,
) -> std::io::Result<()> {
    // Clamp to a sane range — 0 would never make progress, and absurd counts
    // just thrash the provider. The default lives in `MATERIALIZE_WORKERS`.
    let workers = workers.clamp(1, 64);

    let len = std::fs::metadata(path)?.len();
    if len == 0 {
        return Ok(());
    }
    if should_cancel() {
        return Err(cancelled());
    }

    let cursor = Arc::new(AtomicU64::new(0));
    let done = Arc::new(AtomicU64::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let live = Arc::new(AtomicUsize::new(workers));
    let first_err: Arc<Mutex<Option<std::io::Error>>> = Arc::new(Mutex::new(None));

    let mut handles = Vec::with_capacity(workers);
    for _ in 0..workers {
        let path = path.to_path_buf();
        let cursor = Arc::clone(&cursor);
        let done = Arc::clone(&done);
        let stop = Arc::clone(&stop);
        let live = Arc::clone(&live);
        let first_err = Arc::clone(&first_err);
        handles.push(std::thread::spawn(move || {
            if let Err(e) = fault_worker(&path, len, &cursor, &done, &stop) {
                stop.store(true, Ordering::Relaxed);
                let mut slot = first_err.lock().unwrap();
                if slot.is_none() {
                    *slot = Some(e);
                }
            }
            live.fetch_sub(1, Ordering::Relaxed);
        }));
    }

    // Monitor from the calling thread: drive progress and honour cancel.
    // Keeping the loop here means `on_progress` / `should_cancel` stay on this
    // thread and need no `Send` bound. Stop when every worker has exited (all
    // blocks faulted, or an error set `stop`).
    loop {
        if should_cancel() {
            stop.store(true, Ordering::Relaxed);
            break;
        }
        on_progress(done.load(Ordering::Relaxed));
        if live.load(Ordering::Relaxed) == 0 {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    for h in handles {
        let _ = h.join();
    }

    if let Some(e) = first_err.lock().unwrap().take() {
        return Err(e);
    }
    if should_cancel() {
        return Err(cancelled());
    }
    on_progress(len);
    Ok(())
}

fn cancelled() -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::Interrupted, "materialization cancelled")
}

/// Pull blocks off the shared `cursor` and fault each one in until the file is
/// fully claimed (or `stop` is set). Its own file handle keeps the cursor
/// independent of sibling workers (`seek` + `read` is portable — no
/// `pread`/`FileExt` — so it still compiles on Windows).
fn fault_worker(
    path: &Path,
    len: u64,
    cursor: &AtomicU64,
    done: &AtomicU64,
    stop: &AtomicBool,
) -> std::io::Result<()> {
    let mut file = std::fs::File::open(path)?;
    let mut buf = vec![0u8; MATERIALIZE_BLOCK as usize];
    loop {
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        let off = cursor.fetch_add(MATERIALIZE_BLOCK, Ordering::Relaxed);
        if off >= len {
            return Ok(());
        }
        let want = ((len - off).min(MATERIALIZE_BLOCK)) as usize;
        let n = fault_block(&mut file, off, &mut buf[..want], stop)?;
        done.fetch_add(n as u64, Ordering::Relaxed);
    }
}

/// Read `[off, off + buf.len())` into `buf`, retrying transient failures with
/// exponential backoff. Returns the bytes read (`== buf.len()` unless the file
/// ended early). Gives up — propagating the error — after
/// [`MATERIALIZE_BLOCK_ATTEMPTS`] failures or once `stop` is set.
fn fault_block(
    file: &mut std::fs::File,
    off: u64,
    buf: &mut [u8],
    stop: &AtomicBool,
) -> std::io::Result<usize> {
    let mut attempt: u32 = 0;
    loop {
        match read_block_at(file, off, buf) {
            Ok(n) => return Ok(n),
            Err(e) => {
                attempt += 1;
                if attempt >= MATERIALIZE_BLOCK_ATTEMPTS || stop.load(Ordering::Relaxed) {
                    return Err(e);
                }
                // 250ms, 500ms, 1s, 2s, … (capped) — give the provider room to
                // recover before re-requesting the same range.
                let backoff = Duration::from_millis(250u64 << (attempt - 1));
                std::thread::sleep(backoff.min(MATERIALIZE_BACKOFF_CAP));
            }
        }
    }
}

/// Seek to `off` and fill `buf` (a short read only at true end-of-file),
/// discarding the bytes. Re-reading an already-faulted range on retry is cheap
/// — those bytes are now local.
fn read_block_at(file: &mut std::fs::File, off: u64, buf: &mut [u8]) -> std::io::Result<usize> {
    file.seek(SeekFrom::Start(off))?;
    let mut filled = 0;
    while filled < buf.len() {
        let n = file.read(&mut buf[filled..])?;
        if n == 0 {
            break;
        }
        filled += n;
    }
    Ok(filled)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn regular_file_is_not_dataless() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(b"hello").unwrap();
        assert!(!is_dataless(f.path()));
    }

    #[test]
    fn missing_file_is_not_dataless() {
        assert!(!is_dataless(Path::new("/nonexistent/path/xyz.img")));
    }

    #[test]
    fn materialize_reads_whole_file_and_reports_progress() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        let data = vec![7u8; 20 * 1024 * 1024 + 123]; // > 2 chunks + remainder
        f.write_all(&data).unwrap();
        f.flush().unwrap();
        let mut last = 0u64;
        let mut calls = 0u32;
        materialize(
            f.path(),
            4,
            |done| {
                assert!(done >= last, "progress must be monotonic");
                last = done;
                calls += 1;
            },
            || false,
        )
        .unwrap();
        // Progress is now polled from the monitor thread (not per read chunk),
        // so a fast local file may report only a couple of times — what matters
        // is that the whole file faulted in and progress was monotonic.
        assert_eq!(last, data.len() as u64, "must fault in the whole file");
        assert!(calls >= 1, "progress must be reported at least once");
    }

    #[test]
    fn materialize_aborts_when_cancelled() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(&vec![0u8; 1024]).unwrap();
        f.flush().unwrap();
        let err = materialize(f.path(), 4, |_| {}, || true).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::Interrupted);
    }
}
