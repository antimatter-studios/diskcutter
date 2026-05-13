//! Disk → image backup pipeline.
//!
//! The inverse of `pipeline::burn`. Reads a source (device or file) byte
//! by byte, optionally compresses on the fly, hashes the uncompressed
//! stream as it goes, writes to a destination file. Emits progress events
//! at the same cadence as the burn pipeline so the UI can reuse the same
//! progress component.
//!
//! Compression is a small adapter trait so each backend (none/gzip/xz/
//! bzip2/zstd) stays isolated and unit-testable. The trait owns a boxed
//! `Write` that the writer delegates to; finishing the trait flushes the
//! compressor and returns the underlying writer for stat collection.

use std::fs::File;
use std::io::{BufWriter, Read, Result, Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

const DEFAULT_CHUNK: usize = 1024 * 1024;
const PROGRESS_INTERVAL_MS: u128 = 250;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Compression {
    None,
    Gzip,
    Xz,
    Bzip2,
    Zstd,
}

impl Compression {
    /// Recommended output-file suffix for the compression choice. Used
    /// when the caller asks for an "auto" output path.
    pub fn suffix(self) -> &'static str {
        match self {
            Compression::None => "",
            Compression::Gzip => ".gz",
            Compression::Xz => ".xz",
            Compression::Bzip2 => ".bz2",
            Compression::Zstd => ".zst",
        }
    }

    /// Parse a string spelling — case-insensitive, accepts a few common
    /// aliases. None for unknown values so callers can surface a clean
    /// error rather than silently defaulting.
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "none" | "raw" | "" => Some(Compression::None),
            "gz" | "gzip" => Some(Compression::Gzip),
            "xz" | "lzma" => Some(Compression::Xz),
            "bz2" | "bzip2" => Some(Compression::Bzip2),
            "zst" | "zstd" => Some(Compression::Zstd),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct BackupOptions {
    pub source_path: PathBuf,
    pub output_path: PathBuf,
    pub compression: Compression,
    pub chunk_size: usize,
    /// Total source byte count. For block devices this is queried via
    /// the OS; for files it's the file size. Used for progress totals
    /// and to decide when to stop reading (a raw device read returns
    /// EOF eventually but reading the whole disk twice is wasteful).
    pub source_bytes: u64,
    /// When true and compression == None, punch filesystem holes for
    /// runs of zero bytes — backups of sparse VM images (qcow2 /
    /// vhd-dynamic / etc.) shrink to roughly the allocated extent on
    /// disk instead of the full virtual size. Compressed outputs
    /// already compress zeros for free so the flag is a no-op there.
    pub sparse: bool,
}

impl BackupOptions {
    pub fn new(source: impl Into<PathBuf>, output: impl Into<PathBuf>) -> Self {
        Self {
            source_path: source.into(),
            output_path: output.into(),
            compression: Compression::None,
            chunk_size: DEFAULT_CHUNK,
            source_bytes: 0,
            sparse: false,
        }
    }
}

#[derive(Debug)]
#[allow(dead_code)]
pub enum BackupError {
    Io(std::io::Error),
    Cancelled,
    SourceTooLarge { limit: u64, got: u64 },
}

impl From<std::io::Error> for BackupError {
    fn from(e: std::io::Error) -> Self {
        BackupError::Io(e)
    }
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct BackupProgress {
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub bytes_per_sec: u64,
    pub elapsed: Duration,
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct BackupResult {
    pub bytes_read: u64,
    pub bytes_written: u64,
    pub source_sha256: String,
    pub elapsed: Duration,
    pub avg_bytes_per_sec: u64,
}

/// Build the configured output writer. Pure-ish (depends only on the
/// compression choice + the underlying writer) so each branch can be
/// unit-tested in isolation against an in-memory buffer.
pub fn make_encoder(
    compression: Compression,
    sink: Box<dyn Write + Send>,
) -> Box<dyn FinishableWrite> {
    match compression {
        Compression::None => Box::new(PlainEncoder { inner: sink }),
        Compression::Gzip => Box::new(GzipEncoder {
            inner: Some(flate2::write::GzEncoder::new(
                sink,
                flate2::Compression::default(),
            )),
        }),
        Compression::Xz => Box::new(XzEncoder {
            inner: Some(xz2::write::XzEncoder::new(sink, 6)),
        }),
        Compression::Bzip2 => Box::new(Bz2Encoder {
            inner: Some(bzip2::write::BzEncoder::new(
                sink,
                bzip2::Compression::default(),
            )),
        }),
        Compression::Zstd => Box::new(ZstdEncoder {
            inner: Some(zstd::stream::Encoder::new(sink, 3).expect("zstd encoder init")),
        }),
    }
}

/// `Write` + finalisation. `finish` flushes the encoder, drains the
/// underlying writer, returns the count of compressed bytes emitted.
pub trait FinishableWrite: Write + Send {
    fn finish(self: Box<Self>) -> Result<u64>;
}

struct PlainEncoder {
    inner: Box<dyn Write + Send>,
}
impl Write for PlainEncoder {
    fn write(&mut self, buf: &[u8]) -> Result<usize> {
        self.inner.write(buf)
    }
    fn flush(&mut self) -> Result<()> {
        self.inner.flush()
    }
}
impl FinishableWrite for PlainEncoder {
    fn finish(mut self: Box<Self>) -> Result<u64> {
        self.inner.flush()?;
        Ok(0) // plain pass-through: caller relies on bytes_read instead
    }
}

struct GzipEncoder {
    inner: Option<flate2::write::GzEncoder<Box<dyn Write + Send>>>,
}
impl Write for GzipEncoder {
    fn write(&mut self, buf: &[u8]) -> Result<usize> {
        self.inner.as_mut().unwrap().write(buf)
    }
    fn flush(&mut self) -> Result<()> {
        self.inner.as_mut().unwrap().flush()
    }
}
impl FinishableWrite for GzipEncoder {
    fn finish(mut self: Box<Self>) -> Result<u64> {
        let enc = self.inner.take().unwrap();
        let mut sink = enc.finish()?;
        sink.flush()?;
        Ok(0)
    }
}

struct XzEncoder {
    inner: Option<xz2::write::XzEncoder<Box<dyn Write + Send>>>,
}
impl Write for XzEncoder {
    fn write(&mut self, buf: &[u8]) -> Result<usize> {
        self.inner.as_mut().unwrap().write(buf)
    }
    fn flush(&mut self) -> Result<()> {
        self.inner.as_mut().unwrap().flush()
    }
}
impl FinishableWrite for XzEncoder {
    fn finish(mut self: Box<Self>) -> Result<u64> {
        let enc = self.inner.take().unwrap();
        let mut sink = enc.finish()?;
        sink.flush()?;
        Ok(0)
    }
}

struct Bz2Encoder {
    inner: Option<bzip2::write::BzEncoder<Box<dyn Write + Send>>>,
}
impl Write for Bz2Encoder {
    fn write(&mut self, buf: &[u8]) -> Result<usize> {
        self.inner.as_mut().unwrap().write(buf)
    }
    fn flush(&mut self) -> Result<()> {
        self.inner.as_mut().unwrap().flush()
    }
}
impl FinishableWrite for Bz2Encoder {
    fn finish(mut self: Box<Self>) -> Result<u64> {
        let enc = self.inner.take().unwrap();
        let mut sink = enc.finish()?;
        sink.flush()?;
        Ok(0)
    }
}

struct ZstdEncoder {
    inner: Option<zstd::stream::Encoder<'static, Box<dyn Write + Send>>>,
}
impl Write for ZstdEncoder {
    fn write(&mut self, buf: &[u8]) -> Result<usize> {
        self.inner.as_mut().unwrap().write(buf)
    }
    fn flush(&mut self) -> Result<()> {
        self.inner.as_mut().unwrap().flush()
    }
}
impl FinishableWrite for ZstdEncoder {
    fn finish(mut self: Box<Self>) -> Result<u64> {
        let enc = self.inner.take().unwrap();
        let mut sink = enc.finish()?;
        sink.flush()?;
        Ok(0)
    }
}

/// Run a backup against a generic source/sink. The Tauri-command layer
/// builds a File reader and a File writer; tests use Cursor + Vec.
///
/// The reader is consumed up to `source_bytes` bytes (matching block
/// device semantics: there's no EOF on a raw `/dev/diskN` read, you
/// stop at the device's size) — for file sources, pass the file size.
pub fn run<R: Read>(
    reader: &mut R,
    writer: Box<dyn FinishableWrite>,
    source_bytes: u64,
    chunk_size: usize,
    cancel: &AtomicBool,
    mut on_progress: impl FnMut(BackupProgress),
) -> std::result::Result<BackupResult, BackupError> {
    let mut writer = writer;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; chunk_size];
    let mut bytes_read: u64 = 0;
    let started = Instant::now();
    let mut last_emit = Instant::now();
    let mut window_start = Instant::now();
    let mut window_bytes: u64 = 0;

    while bytes_read < source_bytes {
        if cancel.load(Ordering::Relaxed) {
            return Err(BackupError::Cancelled);
        }
        let remaining = source_bytes - bytes_read;
        let cap = (buf.len() as u64).min(remaining) as usize;
        let n = reader.read(&mut buf[..cap])?;
        if n == 0 {
            break;
        }
        writer.write_all(&buf[..n])?;
        hasher.update(&buf[..n]);
        bytes_read += n as u64;
        window_bytes += n as u64;

        if last_emit.elapsed().as_millis() >= PROGRESS_INTERVAL_MS {
            let win = window_start.elapsed().as_secs_f64().max(0.001);
            let bps = (window_bytes as f64 / win) as u64;
            on_progress(BackupProgress {
                bytes_done: bytes_read,
                bytes_total: source_bytes,
                bytes_per_sec: bps,
                elapsed: started.elapsed(),
            });
            last_emit = Instant::now();
            window_start = Instant::now();
            window_bytes = 0;
        }
    }

    writer.finish()?;
    let elapsed = started.elapsed();
    let avg = (bytes_read as f64 / elapsed.as_secs_f64().max(0.001)) as u64;
    on_progress(BackupProgress {
        bytes_done: bytes_read,
        bytes_total: source_bytes.max(bytes_read),
        bytes_per_sec: avg,
        elapsed,
    });
    Ok(BackupResult {
        bytes_read,
        bytes_written: 0, // populated by caller via fs::metadata on output path
        source_sha256: hex(hasher.finalize()),
        elapsed,
        avg_bytes_per_sec: avg,
    })
}

/// Convenience top-level: opens source as a file (or as a block device
/// on Unix), opens output file, picks encoder, runs the pipeline,
/// patches `bytes_written` from the output file's final size.
pub fn run_to_file(
    options: &BackupOptions,
    cancel: &AtomicBool,
    on_progress: impl FnMut(BackupProgress),
) -> std::result::Result<BackupResult, BackupError> {
    let mut source = File::open(&options.source_path)?;
    let dest = File::create(&options.output_path)?;
    // Sparse only makes sense for uncompressed output to a regular
    // file — compressed encoders already squash zero runs, and block
    // devices can't have holes. The sparse writer wraps the dest file
    // before the encoder so the hole detection runs on the raw stream.
    let use_sparse = options.sparse && matches!(options.compression, Compression::None);
    let sink: Box<dyn Write + Send> = if use_sparse {
        Box::new(crate::sparse::SparseFileWriter::new(
            dest,
            options.chunk_size,
        ))
    } else {
        Box::new(BufWriter::new(dest))
    };
    let writer = make_encoder(options.compression, sink);
    let mut result = run(
        &mut source,
        writer,
        options.source_bytes,
        options.chunk_size,
        cancel,
        on_progress,
    )?;
    // Probe the output file size — for plain output this equals
    // bytes_read; for compressed output it's the compressed length;
    // for sparse output it's the file's *logical* length (holes are
    // counted toward len() but don't consume blocks).
    result.bytes_written = std::fs::metadata(&options.output_path)
        .map(|m| m.len())
        .unwrap_or(0);
    Ok(result)
}

/// File-size query that works for both regular files and Unix block
/// devices (where `metadata().len()` reports 0 on macOS for /dev/diskN
/// because the file isn't a regular file). Pure function over a Read +
/// Seek so the test suite can exercise it with a Cursor.
pub fn probe_source_size(path: &Path) -> std::io::Result<u64> {
    let meta = std::fs::metadata(path)?;
    if meta.len() > 0 {
        return Ok(meta.len());
    }
    // Block device on macOS / Linux: seek to end to discover size.
    let mut f = File::open(path)?;
    let end = f.seek(std::io::SeekFrom::End(0))?;
    Ok(end)
}

fn hex(bytes: impl AsRef<[u8]>) -> String {
    let mut s = String::with_capacity(bytes.as_ref().len() * 2);
    for b in bytes.as_ref() {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use tempfile::tempdir;

    fn sha256_hex(bytes: &[u8]) -> String {
        let mut h = Sha256::new();
        h.update(bytes);
        hex(h.finalize())
    }

    struct VecWriter {
        inner: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    }
    impl Write for VecWriter {
        fn write(&mut self, buf: &[u8]) -> Result<usize> {
            self.inner.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> Result<()> {
            Ok(())
        }
    }

    fn capture_writer() -> (
        Box<dyn Write + Send>,
        std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    ) {
        let buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let w = VecWriter {
            inner: std::sync::Arc::clone(&buf),
        };
        (Box::new(w), buf)
    }

    #[test]
    fn compression_suffix_maps_each_variant() {
        assert_eq!(Compression::None.suffix(), "");
        assert_eq!(Compression::Gzip.suffix(), ".gz");
        assert_eq!(Compression::Xz.suffix(), ".xz");
        assert_eq!(Compression::Bzip2.suffix(), ".bz2");
        assert_eq!(Compression::Zstd.suffix(), ".zst");
    }

    #[test]
    fn compression_parse_accepts_canonical_names() {
        assert_eq!(Compression::parse("none"), Some(Compression::None));
        assert_eq!(Compression::parse("gzip"), Some(Compression::Gzip));
        assert_eq!(Compression::parse("xz"), Some(Compression::Xz));
        assert_eq!(Compression::parse("bzip2"), Some(Compression::Bzip2));
        assert_eq!(Compression::parse("zstd"), Some(Compression::Zstd));
    }

    #[test]
    fn compression_parse_accepts_common_aliases_case_insensitive() {
        assert_eq!(Compression::parse("GZ"), Some(Compression::Gzip));
        assert_eq!(Compression::parse("lzma"), Some(Compression::Xz));
        assert_eq!(Compression::parse("BZ2"), Some(Compression::Bzip2));
        assert_eq!(Compression::parse("ZST"), Some(Compression::Zstd));
        assert_eq!(Compression::parse("RAW"), Some(Compression::None));
        assert_eq!(Compression::parse(""), Some(Compression::None));
    }

    #[test]
    fn compression_parse_rejects_unknown_name() {
        assert_eq!(Compression::parse("lz4"), None);
        assert_eq!(Compression::parse("brotli"), None);
    }

    #[test]
    fn run_plain_copies_source_bytes_and_hashes_correctly() {
        let payload: Vec<u8> = (0..8192u32).map(|i| (i % 256) as u8).collect();
        let mut src = Cursor::new(payload.clone());
        let (sink, captured) = capture_writer();
        let writer = make_encoder(Compression::None, sink);
        let cancel = AtomicBool::new(false);
        let result = run(
            &mut src,
            writer,
            payload.len() as u64,
            1024,
            &cancel,
            |_| {},
        )
        .unwrap();
        assert_eq!(result.bytes_read, payload.len() as u64);
        assert_eq!(result.source_sha256, sha256_hex(&payload));
        assert_eq!(*captured.lock().unwrap(), payload);
    }

    #[test]
    fn run_plain_respects_source_bytes_smaller_than_source() {
        // Block-device semantics: caller knows the size up front. Reader
        // has more bytes available, but we stop at source_bytes.
        let payload: Vec<u8> = vec![0xAB; 4096];
        let mut src = Cursor::new(payload.clone());
        let (sink, captured) = capture_writer();
        let writer = make_encoder(Compression::None, sink);
        let cancel = AtomicBool::new(false);
        let result = run(&mut src, writer, 1024, 256, &cancel, |_| {}).unwrap();
        assert_eq!(result.bytes_read, 1024);
        assert_eq!(captured.lock().unwrap().len(), 1024);
    }

    #[test]
    fn run_returns_cancelled_when_flag_set_before_start() {
        let mut src = Cursor::new(vec![0u8; 1024]);
        let (sink, _) = capture_writer();
        let writer = make_encoder(Compression::None, sink);
        let cancel = AtomicBool::new(true);
        match run(&mut src, writer, 1024, 256, &cancel, |_| {}) {
            Err(BackupError::Cancelled) => {}
            other => panic!("expected Cancelled, got {other:?}"),
        }
    }

    #[test]
    fn run_emits_final_progress_at_completion() {
        use std::cell::RefCell;
        let payload = vec![1u8; 64];
        let mut src = Cursor::new(payload);
        let (sink, _) = capture_writer();
        let writer = make_encoder(Compression::None, sink);
        let cancel = AtomicBool::new(false);
        let progress = RefCell::new(Vec::<BackupProgress>::new());
        run(&mut src, writer, 64, 1024, &cancel, |p| {
            progress.borrow_mut().push(p)
        })
        .unwrap();
        let last = progress.borrow().last().cloned().expect("progress");
        assert_eq!(last.bytes_done, 64);
        assert!(last.bytes_total >= 64);
    }

    #[test]
    fn round_trip_gzip_recovers_source_bytes() {
        use flate2::read::GzDecoder;
        let payload: Vec<u8> = (0..4096u32).map(|i| (i % 256) as u8).collect();
        let mut src = Cursor::new(payload.clone());
        let (sink, captured) = capture_writer();
        let writer = make_encoder(Compression::Gzip, sink);
        let cancel = AtomicBool::new(false);
        let result = run(
            &mut src,
            writer,
            payload.len() as u64,
            1024,
            &cancel,
            |_| {},
        )
        .unwrap();
        assert_eq!(result.bytes_read, payload.len() as u64);
        assert_eq!(result.source_sha256, sha256_hex(&payload));
        // Compressed output is smaller than source for our zero-biased
        // payload.
        let compressed = captured.lock().unwrap().clone();
        assert!(!compressed.is_empty() && compressed.len() < payload.len());
        // Decompresses back to the original.
        let mut decoder = GzDecoder::new(Cursor::new(compressed));
        let mut out = Vec::new();
        decoder.read_to_end(&mut out).unwrap();
        assert_eq!(out, payload);
    }

    #[test]
    fn round_trip_xz_recovers_source_bytes() {
        use xz2::read::XzDecoder;
        let payload: Vec<u8> = vec![0xCD; 8 * 1024];
        let mut src = Cursor::new(payload.clone());
        let (sink, captured) = capture_writer();
        let writer = make_encoder(Compression::Xz, sink);
        let cancel = AtomicBool::new(false);
        run(
            &mut src,
            writer,
            payload.len() as u64,
            1024,
            &cancel,
            |_| {},
        )
        .unwrap();
        let mut decoder = XzDecoder::new(Cursor::new(captured.lock().unwrap().clone()));
        let mut out = Vec::new();
        decoder.read_to_end(&mut out).unwrap();
        assert_eq!(out, payload);
    }

    #[test]
    fn round_trip_bzip2_recovers_source_bytes() {
        use bzip2::read::BzDecoder;
        let payload: Vec<u8> = vec![0xEF; 4096];
        let mut src = Cursor::new(payload.clone());
        let (sink, captured) = capture_writer();
        let writer = make_encoder(Compression::Bzip2, sink);
        let cancel = AtomicBool::new(false);
        run(
            &mut src,
            writer,
            payload.len() as u64,
            1024,
            &cancel,
            |_| {},
        )
        .unwrap();
        let mut decoder = BzDecoder::new(Cursor::new(captured.lock().unwrap().clone()));
        let mut out = Vec::new();
        decoder.read_to_end(&mut out).unwrap();
        assert_eq!(out, payload);
    }

    #[test]
    fn round_trip_zstd_recovers_source_bytes() {
        let payload: Vec<u8> = vec![0x42; 16 * 1024];
        let mut src = Cursor::new(payload.clone());
        let (sink, captured) = capture_writer();
        let writer = make_encoder(Compression::Zstd, sink);
        let cancel = AtomicBool::new(false);
        run(
            &mut src,
            writer,
            payload.len() as u64,
            1024,
            &cancel,
            |_| {},
        )
        .unwrap();
        let mut decoder =
            zstd::stream::Decoder::new(Cursor::new(captured.lock().unwrap().clone())).unwrap();
        let mut out = Vec::new();
        decoder.read_to_end(&mut out).unwrap();
        assert_eq!(out, payload);
    }

    #[test]
    fn run_to_file_writes_plain_image_with_correct_size() {
        let dir = tempdir().unwrap();
        let source_path = dir.path().join("src.bin");
        let output_path = dir.path().join("out.img");
        let payload: Vec<u8> = (0..2048u32).map(|i| (i % 256) as u8).collect();
        std::fs::write(&source_path, &payload).unwrap();
        let options = BackupOptions {
            source_path: source_path.clone(),
            output_path: output_path.clone(),
            compression: Compression::None,
            chunk_size: 256,
            source_bytes: payload.len() as u64,
            sparse: false,
        };
        let cancel = AtomicBool::new(false);
        let r = run_to_file(&options, &cancel, |_| {}).unwrap();
        assert_eq!(r.bytes_read, payload.len() as u64);
        assert_eq!(r.bytes_written, payload.len() as u64);
        assert_eq!(r.source_sha256, sha256_hex(&payload));
        assert_eq!(std::fs::read(&output_path).unwrap(), payload);
    }

    #[test]
    fn run_to_file_writes_compressed_image_smaller_than_source() {
        let dir = tempdir().unwrap();
        let source_path = dir.path().join("src.bin");
        let output_path = dir.path().join("out.img.gz");
        let payload = vec![0u8; 16 * 1024];
        std::fs::write(&source_path, &payload).unwrap();
        let options = BackupOptions {
            source_path: source_path.clone(),
            output_path: output_path.clone(),
            compression: Compression::Gzip,
            chunk_size: 256,
            source_bytes: payload.len() as u64,
            sparse: false,
        };
        let cancel = AtomicBool::new(false);
        let r = run_to_file(&options, &cancel, |_| {}).unwrap();
        assert_eq!(r.bytes_read, payload.len() as u64);
        // Highly compressible payload — output should be tiny.
        assert!(
            r.bytes_written > 0 && r.bytes_written < payload.len() as u64 / 4,
            "expected compressed output much smaller than source, got {} vs {}",
            r.bytes_written,
            payload.len(),
        );
    }

    #[test]
    fn run_to_file_sparse_output_logical_size_matches_source() {
        // A 1 MiB source with 1 KiB of real content + 1 MiB - 1 KiB of
        // trailing zeros should produce an output whose *logical* size
        // equals the source (1 MiB) but whose on-disk content is
        // dominated by a hole. We can't assert on-disk block count
        // portably, so we assert on the logical length and that the
        // bytes round-trip exactly.
        let dir = tempdir().unwrap();
        let source = dir.path().join("sparse_src.bin");
        let output = dir.path().join("sparse_out.bin");
        let mut payload = vec![0u8; 1024 * 1024];
        for (i, b) in payload[..1024].iter_mut().enumerate() {
            *b = (i % 256) as u8;
        }
        std::fs::write(&source, &payload).unwrap();
        let options = BackupOptions {
            source_path: source.clone(),
            output_path: output.clone(),
            compression: Compression::None,
            chunk_size: 4096,
            source_bytes: payload.len() as u64,
            sparse: true,
        };
        let cancel = AtomicBool::new(false);
        let r = run_to_file(&options, &cancel, |_| {}).unwrap();
        assert_eq!(r.bytes_read, payload.len() as u64);
        assert_eq!(r.bytes_written, payload.len() as u64);
        assert_eq!(std::fs::read(&output).unwrap(), payload);
    }

    #[test]
    fn run_to_file_sparse_disabled_writes_dense_output() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("dense_src.bin");
        let output = dir.path().join("dense_out.bin");
        let payload = vec![0u8; 8192];
        std::fs::write(&source, &payload).unwrap();
        let options = BackupOptions {
            source_path: source.clone(),
            output_path: output.clone(),
            compression: Compression::None,
            chunk_size: 4096,
            source_bytes: payload.len() as u64,
            sparse: false,
        };
        let cancel = AtomicBool::new(false);
        let r = run_to_file(&options, &cancel, |_| {}).unwrap();
        assert_eq!(r.bytes_read, payload.len() as u64);
        assert_eq!(r.bytes_written, payload.len() as u64);
        // Dense path still emits zeros to the file — round-trip equal.
        assert_eq!(std::fs::read(&output).unwrap(), payload);
    }

    #[test]
    fn probe_source_size_reports_file_size() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("a.bin");
        std::fs::write(&p, vec![0u8; 12345]).unwrap();
        assert_eq!(probe_source_size(&p).unwrap(), 12345);
    }

    #[test]
    fn probe_source_size_errors_for_missing_file() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("nope");
        assert!(probe_source_size(&p).is_err());
    }
}
