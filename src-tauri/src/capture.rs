use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::hash::{self, HashAlgo, StreamingHasher};

pub const DEFAULT_CHUNK: usize = 1024 * 1024;

/// Bytes compared just below the resume point — read from both the (re-attached)
/// source and the partial image — to confirm they still match before trusting
/// the bytes already on disk. Guards against resuming onto a different card.
const RESUME_VERIFY_WINDOW: u64 = 256 * 1024;

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
///
/// When `resume` is set and the output is an uncompressed image already holding
/// a partial capture, this continues from the last whole chunk instead of
/// starting over — provided the re-attached source still matches the partial at
/// the boundary (otherwise it falls back to a clean restart). Resume is not
/// possible for compressed output (you can't seek into a compressed stream), so
/// `resume` is ignored there.
pub fn capture<R: Read + Seek + ?Sized>(
    source: &mut R,
    total_bytes: u64,
    output_path: &Path,
    compression: Compression,
    resume: bool,
    cancel: &AtomicBool,
    on_progress: impl FnMut(CaptureProgress),
) -> Result<CaptureResult, CaptureError> {
    match compression {
        Compression::None => {
            let (mut writer, hasher, start) = open_uncompressed(source, output_path, resume)?;
            capture_inner(
                source,
                total_bytes,
                &mut writer,
                start,
                hasher,
                cancel,
                on_progress,
            )
        }
        Compression::Gz => {
            let file = std::fs::File::create(output_path)?;
            let mut encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
            let hasher = hash::new(HashAlgo::Xxh3);
            let result = capture_inner(
                source,
                total_bytes,
                &mut encoder,
                0,
                hasher,
                cancel,
                on_progress,
            )?;
            encoder.finish()?;
            Ok(result)
        }
        Compression::Xz => {
            let file = std::fs::File::create(output_path)?;
            let mut encoder = xz2::write::XzEncoder::new(file, 6);
            let hasher = hash::new(HashAlgo::Xxh3);
            let result = capture_inner(
                source,
                total_bytes,
                &mut encoder,
                0,
                hasher,
                cancel,
                on_progress,
            )?;
            encoder.finish()?;
            Ok(result)
        }
        Compression::Zstd => {
            let file = std::fs::File::create(output_path)?;
            let mut encoder = zstd::Encoder::new(file, 3)?;
            let hasher = hash::new(HashAlgo::Xxh3);
            let result = capture_inner(
                source,
                total_bytes,
                &mut encoder,
                0,
                hasher,
                cancel,
                on_progress,
            )?;
            encoder.finish()?;
            Ok(result)
        }
    }
}

/// The buffered writer positioned at the write offset, a hasher already covering
/// any bytes kept from a prior attempt, and the starting byte offset.
type OpenOutput = (
    std::io::BufWriter<std::fs::File>,
    Box<dyn StreamingHasher>,
    u64,
);

/// Open an uncompressed output for writing, handling resume.
fn open_uncompressed<R: Read + Seek + ?Sized>(
    source: &mut R,
    output_path: &Path,
    resume: bool,
) -> Result<OpenOutput, CaptureError> {
    let resume_from = if resume {
        chunk_aligned_len(output_path)
    } else {
        0
    };

    // Fresh capture (or the partial was sub-chunk) — truncate and start at top.
    if resume_from == 0 {
        let file = std::fs::File::create(output_path)?;
        return Ok((std::io::BufWriter::new(file), hash::new(HashAlgo::Xxh3), 0));
    }

    // Only trust the existing bytes if the re-attached source still matches them
    // at the boundary; otherwise it's likely a different card — restart clean.
    if !source_matches_partial(source, output_path, resume_from)? {
        let file = std::fs::File::create(output_path)?;
        source.seek(SeekFrom::Start(0))?;
        return Ok((std::io::BufWriter::new(file), hash::new(HashAlgo::Xxh3), 0));
    }

    // Seed the hash with the bytes already on disk so the final digest still
    // covers the whole image, then position both ends at the resume point.
    let hasher = hash_prefix(output_path, resume_from)?;
    let file = std::fs::OpenOptions::new().write(true).open(output_path)?;
    let mut writer = std::io::BufWriter::new(file);
    writer.seek(SeekFrom::Start(resume_from))?;
    source.seek(SeekFrom::Start(resume_from))?;
    Ok((writer, hasher, resume_from))
}

/// Existing output length floored to a chunk boundary — a torn final block from
/// the failed attempt is re-read rather than trusted.
fn chunk_aligned_len(output_path: &Path) -> u64 {
    let len = std::fs::metadata(output_path).map(|m| m.len()).unwrap_or(0);
    (len / DEFAULT_CHUNK as u64) * DEFAULT_CHUNK as u64
}

/// Compare the window just below `resume_from` between the source and the
/// partial image — confirms the re-attached source holds the same content
/// before the resumed capture trusts the bytes already written.
fn source_matches_partial<R: Read + Seek + ?Sized>(
    source: &mut R,
    output_path: &Path,
    resume_from: u64,
) -> Result<bool, CaptureError> {
    let window = RESUME_VERIFY_WINDOW.min(resume_from) as usize;
    if window == 0 {
        return Ok(true);
    }
    let at = resume_from - window as u64;

    source.seek(SeekFrom::Start(at))?;
    let mut from_source = vec![0u8; window];
    fill(source, &mut from_source)?;

    let mut img = std::fs::File::open(output_path)?;
    img.seek(SeekFrom::Start(at))?;
    let mut from_img = vec![0u8; window];
    fill(&mut img, &mut from_img)?;

    Ok(from_source == from_img)
}

/// Hash `[0, resume_from)` of the partial image so a resumed capture's final
/// digest covers the whole image, not just the bytes read this run.
fn hash_prefix(
    output_path: &Path,
    resume_from: u64,
) -> Result<Box<dyn StreamingHasher>, CaptureError> {
    let mut hasher = hash::new(HashAlgo::Xxh3);
    let mut img = std::fs::File::open(output_path)?;
    let mut buf = vec![0u8; DEFAULT_CHUNK];
    let mut remaining = resume_from;
    while remaining > 0 {
        let want = remaining.min(buf.len() as u64) as usize;
        let n = img.read(&mut buf[..want])?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        remaining -= n as u64;
    }
    Ok(hasher)
}

/// Read until `buf` is full or EOF (a short read only at true end-of-file).
fn fill<T: Read + ?Sized>(r: &mut T, buf: &mut [u8]) -> std::io::Result<()> {
    let mut filled = 0;
    while filled < buf.len() {
        let n = r.read(&mut buf[filled..])?;
        if n == 0 {
            break;
        }
        filled += n;
    }
    Ok(())
}

fn capture_inner<R: Read + ?Sized>(
    source: &mut R,
    total_bytes: u64,
    writer: &mut dyn Write,
    mut done: u64,
    mut hasher: Box<dyn StreamingHasher>,
    cancel: &AtomicBool,
    mut on_progress: impl FnMut(CaptureProgress),
) -> Result<CaptureResult, CaptureError> {
    let mut buf = vec![0u8; DEFAULT_CHUNK];
    let start_done = done;
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
    let avg = ((done - start_done) as f64 / elapsed.as_secs_f64().max(0.001)) as u64;
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
            false,
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

        match capture(
            &mut src,
            1024,
            &out,
            Compression::None,
            false,
            &cancel,
            |_| {},
        ) {
            Err(CaptureError::Cancelled) => {}
            other => panic!("expected Cancelled, got {other:?}"),
        }
    }

    // 2 MiB of distinctive data; the first 1 MiB is a "partial" left by a failed
    // capture, the resume continues from the 1 MiB chunk boundary.
    fn ramp(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i % 251) as u8).collect()
    }

    #[test]
    fn capture_resume_continues_from_partial_and_hashes_whole_image() {
        let full = ramp(2 * DEFAULT_CHUNK);
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out.img");
        // Leave a partial image: exactly the first chunk already on disk.
        std::fs::write(&out, &full[..DEFAULT_CHUNK]).unwrap();
        let cancel = AtomicBool::new(false);

        // Fresh capture of the whole thing — reference hash.
        let ref_out = dir.path().join("ref.img");
        let fresh = capture(
            &mut Cursor::new(full.clone()),
            full.len() as u64,
            &ref_out,
            Compression::None,
            false,
            &cancel,
            |_| {},
        )
        .unwrap();

        // Resume from the partial.
        let mut src = Cursor::new(full.clone());
        let resumed = capture(
            &mut src,
            full.len() as u64,
            &out,
            Compression::None,
            true,
            &cancel,
            |_| {},
        )
        .unwrap();

        assert_eq!(std::fs::read(&out).unwrap(), full, "image is complete");
        assert_eq!(resumed.bytes_read, full.len() as u64);
        assert_eq!(
            resumed.source_hash, fresh.source_hash,
            "resumed digest must cover the whole image"
        );
    }

    #[test]
    fn capture_resume_restarts_when_source_differs() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out.img");
        // Partial written from one card...
        std::fs::write(&out, ramp(DEFAULT_CHUNK)).unwrap();
        // ...but the re-attached source has different content at the boundary.
        let other: Vec<u8> = (0..2 * DEFAULT_CHUNK).map(|i| (i % 97 + 1) as u8).collect();
        let cancel = AtomicBool::new(false);

        let mut src = Cursor::new(other.clone());
        let r = capture(
            &mut src,
            other.len() as u64,
            &out,
            Compression::None,
            true,
            &cancel,
            |_| {},
        )
        .unwrap();

        // Safeguard tripped → clean restart, so the image equals the new source.
        assert_eq!(std::fs::read(&out).unwrap(), other);
        assert_eq!(r.bytes_read, other.len() as u64);
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
