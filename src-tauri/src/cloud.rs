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

use std::io::Read;
use std::path::Path;

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

/// Read `path` start-to-end to force a dataless cloud file to fault in. The
/// bytes are discarded — the goal is purely to warm the local cache so the
/// burn's own read is local. `on_progress(bytes_read)` is called after every
/// chunk so the caller can drive a download progress bar; the read aborts
/// early with [`std::io::ErrorKind::Interrupted`] as soon as `should_cancel`
/// returns true. A read error (e.g. the wedged-provider `EDEADLK`) is
/// propagated verbatim for the caller to classify.
pub fn materialize(
    path: &Path,
    mut on_progress: impl FnMut(u64),
    should_cancel: impl Fn() -> bool,
) -> std::io::Result<()> {
    let mut file = std::fs::File::open(path)?;
    // 8 MiB matches the burn's default chunk: large enough that per-read
    // overhead is negligible against a multi-hundred-MB cloud download.
    let mut buf = vec![0u8; 8 * 1024 * 1024];
    let mut done: u64 = 0;
    loop {
        if should_cancel() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "materialization cancelled",
            ));
        }
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        done += n as u64;
        on_progress(done);
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
        assert_eq!(last, data.len() as u64);
        assert!(calls >= 3, "expected multiple chunk callbacks, got {calls}");
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
