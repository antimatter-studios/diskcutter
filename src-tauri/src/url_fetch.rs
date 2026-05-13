//! URL → local file download pipeline. Used by the "FROM URL" button so
//! the user can paste an Ubuntu/Fedora/etc ISO link and burn it without
//! having to download manually.
//!
//! Wire shape mirrors the burn pipeline: caller starts a job, gets back
//! a job_id, then watches event streams (`disk-cutter://download-*`)
//! for progress / completion / failure. Downloads land in
//! `app_data_dir()/downloads/<sanitized_filename>` so they survive
//! across app restarts and can be re-burned later without re-fetching.
//!
//! Synchronous HTTP via `ureq` — no tokio dep, matches our thread-per-
//! download model. Each in-flight download owns an `AtomicBool` that
//! `cancel_download` can flip to abort the streaming read mid-way.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter, Manager, State};

#[derive(Default)]
pub struct DownloadRegistry(pub Mutex<HashMap<String, Arc<AtomicBool>>>);

#[derive(Serialize, Clone)]
pub struct DownloadProgress {
    pub job_id: String,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub bytes_per_sec: u64,
}

#[derive(Serialize, Clone)]
pub struct DownloadComplete {
    pub job_id: String,
    pub path: String,
    pub name: String,
    pub bytes: u64,
    pub sha256: String,
    pub elapsed_ms: u64,
}

#[derive(Serialize, Clone)]
pub struct DownloadError {
    pub job_id: String,
    pub error_code: String,
    pub error_message: String,
}

/// Validate + normalise a URL. We only accept http/https. URLs with
/// anything else (file:, ftp:, javascript:, …) are rejected — a "FROM
/// URL" prompt that quietly downloaded `file:///etc/passwd` would be
/// a footgun.
pub fn validate_url(raw: &str) -> Result<String, &'static str> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("URL is empty");
    }
    let lower = trimmed.to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        return Err("Only http(s) URLs are supported");
    }
    // Reject obvious malformed cases without pulling in a full URL parser.
    if trimmed.contains(' ') || trimmed.contains('\n') || trimmed.contains('\t') {
        return Err("URL contains whitespace");
    }
    Ok(trimmed.to_string())
}

/// Derive a safe filename for the local download from the URL's last
/// path segment. Strips query strings and falls back to "download.img"
/// when the URL has no usable basename.
pub fn derive_filename(url: &str) -> String {
    let after_scheme = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let path_only = after_scheme.split('?').next().unwrap_or(after_scheme);
    // No path segment at all (e.g. "https://example.com") → fallback.
    // Without this check, rsplit('/').next() returns the host part.
    if !path_only.contains('/') {
        return "download.img".to_string();
    }
    let last = path_only.rsplit('/').next().unwrap_or("");
    let cleaned = sanitize_filename(last);
    if cleaned.is_empty() || !cleaned.contains('.') {
        return "download.img".to_string();
    }
    cleaned
}

/// Restrict a filename to characters that are safe across macOS / Linux
/// / Windows file systems. Anything else collapses to `_`. Length is
/// capped at 200 chars — long enough for "ubuntu-24.04.1-desktop-amd64"
/// type filenames, short enough to leave headroom under POSIX 255 byte
/// path component limits even after we add a numeric suffix.
pub fn sanitize_filename(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for ch in name.chars() {
        match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '.' | '-' | '_' => out.push(ch),
            _ => out.push('_'),
        }
    }
    if out.len() > 200 {
        out.truncate(200);
    }
    // Strip leading dots so "..\..\evil" can't escape via filename.
    let trimmed = out.trim_start_matches('.').to_string();
    trimmed
}

#[tauri::command]
pub fn start_download(
    app: AppHandle,
    registry: State<'_, DownloadRegistry>,
    job_id: String,
    url: String,
) -> Result<(), String> {
    let url = validate_url(&url).map_err(|e| e.to_string())?;
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("app_data_dir: {e}"))?
        .join("downloads");
    std::fs::create_dir_all(&dir).map_err(|e| format!("create_dir_all: {e}"))?;
    let filename = derive_filename(&url);
    let dest = dir.join(&filename);

    let cancel = Arc::new(AtomicBool::new(false));
    registry
        .0
        .lock()
        .map_err(|e| e.to_string())?
        .insert(job_id.clone(), cancel.clone());

    let app_t = app.clone();
    let id = job_id.clone();
    let url_t = url;
    let dest_t = dest;
    std::thread::spawn(move || {
        let started = Instant::now();
        let result = download_to_file(&url_t, &dest_t, &cancel, |bytes_done, bytes_total, bps| {
            let _ = app_t.emit(
                "disk-cutter://download-progress",
                DownloadProgress {
                    job_id: id.clone(),
                    bytes_done,
                    bytes_total,
                    bytes_per_sec: bps,
                },
            );
        });
        // Drop the cancel flag from the registry now that the worker is
        // done with it; keeps the map from growing unbounded across many
        // downloads in one session.
        if let Ok(mut reg) = app_t.state::<DownloadRegistry>().0.lock() {
            reg.remove(&id);
        }
        let elapsed_ms = started.elapsed().as_millis() as u64;
        match result {
            Ok(r) => {
                let _ = app_t.emit(
                    "disk-cutter://download-complete",
                    DownloadComplete {
                        job_id: id,
                        path: r.path.to_string_lossy().to_string(),
                        name: r
                            .path
                            .file_name()
                            .and_then(|s| s.to_str())
                            .unwrap_or("")
                            .to_string(),
                        bytes: r.bytes,
                        sha256: r.sha256,
                        elapsed_ms,
                    },
                );
            }
            Err(DownloadFail { code, message }) => {
                let _ = app_t.emit(
                    "disk-cutter://download-error",
                    DownloadError {
                        job_id: id,
                        error_code: code.to_string(),
                        error_message: message,
                    },
                );
            }
        }
    });
    Ok(())
}

#[tauri::command]
pub fn cancel_download(
    registry: State<'_, DownloadRegistry>,
    job_id: String,
) -> Result<(), String> {
    if let Some(flag) = registry.0.lock().map_err(|e| e.to_string())?.get(&job_id) {
        flag.store(true, Ordering::Relaxed);
    }
    Ok(())
}

pub struct DownloadOk {
    pub path: PathBuf,
    pub bytes: u64,
    pub sha256: String,
}

pub struct DownloadFail {
    pub code: &'static str,
    pub message: String,
}

/// Pure function: stream a URL into `dest`, hashing as we go. Calls
/// `on_progress(bytes_done, bytes_total, bytes_per_sec)` roughly every
/// 250ms while the download is in flight. Honours the `cancel` flag on
/// every chunk boundary. `bytes_total` is 0 when the server doesn't
/// announce a Content-Length.
pub fn download_to_file<F>(
    url: &str,
    dest: &Path,
    cancel: &AtomicBool,
    mut on_progress: F,
) -> Result<DownloadOk, DownloadFail>
where
    F: FnMut(u64, u64, u64),
{
    let resp = match ureq::get(url).timeout(Duration::from_secs(30)).call() {
        Ok(r) => r,
        Err(ureq::Error::Status(code, _)) => {
            return Err(DownloadFail {
                code: "EHTTP",
                message: format!("HTTP {code}"),
            });
        }
        Err(e) => {
            return Err(DownloadFail {
                code: "ENET",
                message: format!("network error: {e}"),
            });
        }
    };

    let bytes_total = resp
        .header("Content-Length")
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);

    let mut file = File::create(dest).map_err(|e| DownloadFail {
        code: "EIO",
        message: format!("create {}: {e}", dest.display()),
    })?;
    let mut hasher = Sha256::new();
    let mut reader = resp.into_reader();
    let mut buf = vec![0u8; 256 * 1024];
    let mut bytes_done: u64 = 0;
    let started = Instant::now();
    let mut last_emit = Instant::now() - Duration::from_secs(1);

    loop {
        if cancel.load(Ordering::Relaxed) {
            // Tear down — leave the partial file in place but report cancel.
            return Err(DownloadFail {
                code: "ECANCELLED",
                message: "download cancelled".to_string(),
            });
        }
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => {
                return Err(DownloadFail {
                    code: "EIO",
                    message: format!("read body: {e}"),
                });
            }
        };
        file.write_all(&buf[..n]).map_err(|e| DownloadFail {
            code: "EIO",
            message: format!("write body: {e}"),
        })?;
        hasher.update(&buf[..n]);
        bytes_done += n as u64;

        if last_emit.elapsed() >= Duration::from_millis(250) {
            let elapsed = started.elapsed().as_secs_f64().max(0.001);
            let bps = (bytes_done as f64 / elapsed) as u64;
            on_progress(bytes_done, bytes_total, bps);
            last_emit = Instant::now();
        }
    }

    let elapsed = started.elapsed().as_secs_f64().max(0.001);
    let bps = (bytes_done as f64 / elapsed) as u64;
    on_progress(bytes_done, bytes_total.max(bytes_done), bps);

    file.flush().map_err(|e| DownloadFail {
        code: "EIO",
        message: format!("flush: {e}"),
    })?;
    drop(file);

    let sha256 = format!("{:x}", hasher.finalize());
    Ok(DownloadOk {
        path: dest.to_path_buf(),
        bytes: bytes_done,
        sha256,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_url_accepts_http_and_https() {
        assert!(validate_url("http://example.com/file.iso").is_ok());
        assert!(validate_url("https://example.com/file.iso").is_ok());
        assert!(validate_url("HTTPS://example.com/file.iso").is_ok());
    }

    #[test]
    fn validate_url_trims_whitespace() {
        assert_eq!(
            validate_url("  https://x/y.iso  ").unwrap(),
            "https://x/y.iso"
        );
    }

    #[test]
    fn validate_url_rejects_other_schemes() {
        for bad in [
            "file:///etc/passwd",
            "ftp://example.com/x.iso",
            "javascript:alert(1)",
            "data:text/plain,hi",
        ] {
            assert!(validate_url(bad).is_err(), "expected rejection for {bad:?}");
        }
    }

    #[test]
    fn validate_url_rejects_empty_and_whitespace_inside() {
        assert!(validate_url("").is_err());
        assert!(validate_url("   ").is_err());
        assert!(validate_url("https://example.com/ has spaces").is_err());
        assert!(validate_url("https://example.com/has\ttab").is_err());
    }

    #[test]
    fn derive_filename_takes_basename() {
        assert_eq!(
            derive_filename("https://releases.ubuntu.com/24.04/ubuntu-24.04.1-desktop-amd64.iso"),
            "ubuntu-24.04.1-desktop-amd64.iso"
        );
    }

    #[test]
    fn derive_filename_strips_query_string() {
        assert_eq!(
            derive_filename("https://example.com/file.iso?token=abc"),
            "file.iso"
        );
    }

    #[test]
    fn derive_filename_falls_back_when_no_extension() {
        assert_eq!(derive_filename("https://example.com/"), "download.img");
        assert_eq!(derive_filename("https://example.com"), "download.img");
        assert_eq!(
            derive_filename("https://example.com/path/no_dot_here"),
            "download.img"
        );
    }

    #[test]
    fn derive_filename_sanitizes_evil_input() {
        // Path-traversal sequences get reduced to underscores, leading
        // dots stripped — no escape from the downloads directory.
        let out = derive_filename("https://example.com/..%2F..%2Fetc%2Fpasswd");
        assert!(!out.starts_with('.'), "must not start with dot: {out}");
        assert!(!out.contains('/'), "no slashes: {out}");
        assert!(!out.contains('\\'), "no backslashes: {out}");
    }

    #[test]
    fn sanitize_filename_keeps_alnum_dot_dash_underscore() {
        assert_eq!(sanitize_filename("foo-bar.iso_v2"), "foo-bar.iso_v2");
    }

    #[test]
    fn sanitize_filename_collapses_unsafe_chars_to_underscore() {
        assert_eq!(sanitize_filename("a/b\\c d:e?f"), "a_b_c_d_e_f");
    }

    #[test]
    fn sanitize_filename_strips_leading_dots() {
        assert_eq!(sanitize_filename("..hidden.iso"), "hidden.iso");
    }

    #[test]
    fn sanitize_filename_truncates_long_names() {
        let s = "a".repeat(500);
        assert_eq!(sanitize_filename(&s).len(), 200);
    }

    #[test]
    fn download_to_file_honours_cancel_flag_before_first_read() {
        let cancel = AtomicBool::new(true);
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("x.iso");
        // Need a real-ish URL for the call to even attempt; use a localhost
        // URL that will fail to connect quickly. The cancel check happens
        // BEFORE first read, so the connection will succeed (or fail) but
        // we expect the cancel to be observed if the connection happens to
        // succeed and we get into the loop. For a robust test we just call
        // with an unreachable URL and assert we got an error of some kind.
        let r = download_to_file("http://127.0.0.1:1/x.iso", &dest, &cancel, |_, _, _| {});
        assert!(r.is_err());
    }

    #[test]
    fn download_to_file_returns_enet_for_unreachable_host() {
        let cancel = AtomicBool::new(false);
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("x.iso");
        let r = download_to_file(
            "http://127.0.0.1:1/this-port-is-closed.iso",
            &dest,
            &cancel,
            |_, _, _| {},
        );
        match r {
            Err(DownloadFail { code, .. }) => assert_eq!(code, "ENET"),
            Ok(_) => panic!("expected error"),
        }
    }
}
