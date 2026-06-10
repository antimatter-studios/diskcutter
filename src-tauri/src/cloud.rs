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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

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

/// Number of concurrent read streams used to fault a cloud file in. A single
/// sequential read leaves the link idle between requests — the File Provider
/// faults on demand with no read-ahead, so one request is in flight at a time
/// and throughput collapses to a fraction of the mount's bandwidth. Eight
/// concurrent ranges keep enough fetches in flight to saturate a fast mount
/// (matching Finder's "Download Now") without flooding the provider.
const MATERIALIZE_WORKERS: u64 = 8;

/// Force a dataless cloud file to fault in by reading every byte (the bytes are
/// discarded — the goal is purely to make the path resident so the burn's own
/// read is local). The file is split into [`MATERIALIZE_WORKERS`] contiguous
/// regions read in parallel, so multiple fetches overlap instead of trickling
/// one 8 MiB request at a time.
///
/// `on_progress(bytes_faulted)` is called from the calling thread as the shared
/// counter advances, so the caller's closure (which emits Tauri events) never
/// crosses a thread boundary. The read aborts with
/// [`std::io::ErrorKind::Interrupted`] as soon as `should_cancel` returns true;
/// a read error (e.g. the wedged-provider `EDEADLK`) is propagated verbatim.
pub fn materialize(
    path: &Path,
    mut on_progress: impl FnMut(u64),
    should_cancel: impl Fn() -> bool,
) -> std::io::Result<()> {
    let len = std::fs::metadata(path)?.len();
    if len == 0 {
        return Ok(());
    }
    if should_cancel() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Interrupted,
            "materialization cancelled",
        ));
    }

    let done = Arc::new(AtomicU64::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let first_err: Arc<Mutex<Option<std::io::Error>>> = Arc::new(Mutex::new(None));

    let region = len.div_ceil(MATERIALIZE_WORKERS);
    let mut handles = Vec::new();
    let mut start = 0u64;
    while start < len {
        let end = (start + region).min(len);
        let path = path.to_path_buf();
        let done = Arc::clone(&done);
        let stop = Arc::clone(&stop);
        let first_err = Arc::clone(&first_err);
        handles.push(std::thread::spawn(move || {
            if let Err(e) = fault_region(&path, start, end, &done, &stop) {
                stop.store(true, Ordering::Relaxed);
                let mut slot = first_err.lock().unwrap();
                if slot.is_none() {
                    *slot = Some(e);
                }
            }
        }));
        start = end;
    }

    // Monitor from the calling thread: drive progress, honour cancel, and bail
    // on the first worker error. Keeping the loop here means `on_progress` /
    // `should_cancel` stay on this thread and need no `Send` bound.
    loop {
        if should_cancel() {
            stop.store(true, Ordering::Relaxed);
            break;
        }
        let d = done.load(Ordering::Relaxed);
        on_progress(d);
        if d >= len || stop.load(Ordering::Relaxed) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    for h in handles {
        let _ = h.join();
    }

    if let Some(e) = first_err.lock().unwrap().take() {
        return Err(e);
    }
    if should_cancel() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Interrupted,
            "materialization cancelled",
        ));
    }
    on_progress(len);
    Ok(())
}

/// Sequentially read the half-open range `[start, end)` of `path`, discarding
/// bytes and advancing the shared `done` counter. Uses its own file handle so
/// the cursor is independent of sibling workers (`seek` + `read` is portable;
/// no `pread`/`FileExt` so it still compiles on Windows). Stops early if `stop`
/// is set (cancel, or another worker's error).
fn fault_region(
    path: &Path,
    start: u64,
    end: u64,
    done: &AtomicU64,
    stop: &AtomicBool,
) -> std::io::Result<()> {
    let mut file = std::fs::File::open(path)?;
    file.seek(SeekFrom::Start(start))?;
    // 8 MiB matches the burn's default chunk: large enough that per-read
    // overhead is negligible.
    let mut buf = vec![0u8; 8 * 1024 * 1024];
    let mut off = start;
    while off < end {
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        let want = ((end - off).min(buf.len() as u64)) as usize;
        let n = file.read(&mut buf[..want])?;
        if n == 0 {
            break;
        }
        off += n as u64;
        done.fetch_add(n as u64, Ordering::Relaxed);
    }
    Ok(())
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
        let err = materialize(f.path(), |_| {}, || true).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::Interrupted);
    }
}
