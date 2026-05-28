use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::hash::{self, HashAlgo};

pub const DEFAULT_CHUNK: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Compression {
    None,
    Gz,
    Xz,
    Zstd,
}

impl Compression {
    pub fn parse(s: &str) -> Self {
        match s {
            "gz" | "gzip" => Compression::Gz,
            "xz" => Compression::Xz,
            "zstd" | "zst" => Compression::Zstd,
            _ => Compression::None,
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Compression::None => "img",
            Compression::Gz => "img.gz",
            Compression::Xz => "img.xz",
            Compression::Zstd => "img.zst",
        }
    }
}

#[derive(Debug)]
pub enum CaptureError {
    Io(std::io::Error),
    Cancelled,
}

impl From<std::io::Error> for CaptureError {
    fn from(e: std::io::Error) -> Self {
        CaptureError::Io(e)
    }
}

impl std::fmt::Display for CaptureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CaptureError::Cancelled => write!(f, "cancelled"),
            CaptureError::Io(e) => write!(f, "{e}"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct CaptureProgress {
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub bytes_per_sec: u64,
    pub elapsed: Duration,
}

#[derive(Debug)]
pub struct CaptureResult {
    pub bytes_read: u64,
    pub source_hash: String,
    pub elapsed: Duration,
    pub avg_bytes_per_sec: u64,
}

/// Read `total_bytes` from `source` and write to `output_path`, optionally
/// compressing on the fly. Hashes the raw (pre-compression) bytes read.
pub fn capture(
    source: &mut dyn Read,
    total_bytes: u64,
    output_path: &Path,
    compression: Compression,
    cancel: &AtomicBool,
    on_progress: impl FnMut(CaptureProgress),
) -> Result<CaptureResult, CaptureError> {
    let file = std::fs::File::create(output_path)?;
    match compression {
        Compression::None => {
            let mut writer = std::io::BufWriter::new(file);
            capture_inner(source, total_bytes, &mut writer, cancel, on_progress)
        }
        Compression::Gz => {
            let mut encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
            let result = capture_inner(source, total_bytes, &mut encoder, cancel, on_progress)?;
            encoder.finish()?;
            Ok(result)
        }
        Compression::Xz => {
            let mut encoder = xz2::write::XzEncoder::new(file, 6);
            let result = capture_inner(source, total_bytes, &mut encoder, cancel, on_progress)?;
            encoder.finish()?;
            Ok(result)
        }
        Compression::Zstd => {
            let mut encoder = zstd::Encoder::new(file, 3)?;
            let result = capture_inner(source, total_bytes, &mut encoder, cancel, on_progress)?;
            encoder.finish()?;
            Ok(result)
        }
    }
}

fn capture_inner(
    source: &mut dyn Read,
    total_bytes: u64,
    writer: &mut dyn Write,
    cancel: &AtomicBool,
    mut on_progress: impl FnMut(CaptureProgress),
) -> Result<CaptureResult, CaptureError> {
    let mut hasher = hash::new(HashAlgo::Xxh3);
    let mut buf = vec![0u8; DEFAULT_CHUNK];
    let mut done: u64 = 0;
    let started = Instant::now();
    let mut last_emit = Instant::now();
    let mut window_start = Instant::now();
    let mut window_bytes: u64 = 0;

    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(CaptureError::Cancelled);
        }

        // Limit read to what's left so we don't read past the device boundary.
        let remaining = if total_bytes > 0 {
            (total_bytes - done).min(buf.len() as u64) as usize
        } else {
            buf.len()
        };
        if remaining == 0 {
            break;
        }

        let n = match source.read(&mut buf[..remaining]) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => return Err(CaptureError::Io(e)),
        };

        writer.write_all(&buf[..n])?;
        hasher.update(&buf[..n]);
        done += n as u64;
        window_bytes += n as u64;

        if last_emit.elapsed() >= Duration::from_millis(250) {
            let win = window_start.elapsed().as_secs_f64().max(0.001);
            let bps = (window_bytes as f64 / win) as u64;
            on_progress(CaptureProgress {
                bytes_done: done,
                bytes_total: total_bytes,
                bytes_per_sec: bps,
                elapsed: started.elapsed(),
            });
            last_emit = Instant::now();
            window_start = Instant::now();
            window_bytes = 0;
        }
    }

    writer.flush()?;
    let elapsed = started.elapsed();
    let avg = (done as f64 / elapsed.as_secs_f64().max(0.001)) as u64;
    on_progress(CaptureProgress {
        bytes_done: done,
        bytes_total: total_bytes.max(done),
        bytes_per_sec: avg,
        elapsed,
    });
    Ok(CaptureResult {
        bytes_read: done,
        source_hash: hasher.finalize_hex(),
        elapsed,
        avg_bytes_per_sec: avg,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::sync::atomic::AtomicBool;

    #[test]
    fn capture_none_round_trips() {
        let data: Vec<u8> = (0..4096u32).map(|i| (i % 256) as u8).collect();
        let mut src = Cursor::new(data.clone());
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out.img");
        let cancel = AtomicBool::new(false);

        let result = capture(
            &mut src,
            data.len() as u64,
            &out,
            Compression::None,
            &cancel,
            |_| {},
        )
        .unwrap();

        assert_eq!(result.bytes_read, data.len() as u64);
        let written = std::fs::read(&out).unwrap();
        assert_eq!(written, data);
    }

    #[test]
    fn capture_returns_cancelled_when_flag_set() {
        let data = vec![0u8; 1024];
        let mut src = Cursor::new(data);
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out.img");
        let cancel = AtomicBool::new(true);

        match capture(&mut src, 1024, &out, Compression::None, &cancel, |_| {}) {
            Err(CaptureError::Cancelled) => {}
            other => panic!("expected Cancelled, got {other:?}"),
        }
    }

    #[test]
    fn compression_extension() {
        assert_eq!(Compression::None.extension(), "img");
        assert_eq!(Compression::Gz.extension(), "img.gz");
        assert_eq!(Compression::Xz.extension(), "img.xz");
        assert_eq!(Compression::Zstd.extension(), "img.zst");
    }

    #[test]
    fn compression_parse_roundtrips() {
        assert_eq!(Compression::parse("gz"), Compression::Gz);
        assert_eq!(Compression::parse("gzip"), Compression::Gz);
        assert_eq!(Compression::parse("xz"), Compression::Xz);
        assert_eq!(Compression::parse("zstd"), Compression::Zstd);
        assert_eq!(Compression::parse("zst"), Compression::Zstd);
        assert_eq!(Compression::parse("none"), Compression::None);
        assert_eq!(Compression::parse("bogus"), Compression::None);
    }
}
