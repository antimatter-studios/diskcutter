use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

use crate::readers::ImageReader;
use crate::writers::{DeviceReader, DeviceWriter};

// 1 MiB matches typical USB-MSC max transfer length on macOS, avoiding
// kernel-side splitting of bigger writes. Etcher uses the same.
pub const DEFAULT_CHUNK: usize = 1024 * 1024;
const MAX_MISMATCHES: usize = 256;
const SECTOR_BYTES: u64 = 512;

#[derive(Debug)]
#[allow(dead_code)]
pub enum BurnError {
    Io(std::io::Error),
    Cancelled,
    SizeMismatch { expected: u64, actual: u64 },
}

impl From<std::io::Error> for BurnError {
    fn from(e: std::io::Error) -> Self {
        BurnError::Io(e)
    }
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct BurnProgress {
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub bytes_per_sec: u64,
    pub elapsed: Duration,
}

pub struct BurnResult {
    pub bytes_written: u64,
    pub source_sha256: String,
    pub elapsed: Duration,
    pub avg_bytes_per_sec: u64,
}

pub fn burn(
    reader: &mut dyn ImageReader,
    writer: Box<dyn DeviceWriter>,
    chunk_size: usize,
    cancel: &AtomicBool,
    mut on_progress: impl FnMut(BurnProgress),
) -> Result<BurnResult, BurnError> {
    let total = reader.info().uncompressed_bytes;
    let mut writer = writer;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; chunk_size];
    let mut done: u64 = 0;
    let started = Instant::now();
    let mut last_emit = Instant::now();
    let mut window_start = Instant::now();
    let mut window_bytes: u64 = 0;

    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(BurnError::Cancelled);
        }
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        writer.write_all(&buf[..n])?;
        hasher.update(&buf[..n]);
        done += n as u64;
        window_bytes += n as u64;

        if last_emit.elapsed() >= Duration::from_millis(250) {
            let win = window_start.elapsed().as_secs_f64().max(0.001);
            let bps = (window_bytes as f64 / win) as u64;
            on_progress(BurnProgress {
                bytes_done: done,
                bytes_total: total,
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
    let avg = (done as f64 / elapsed.as_secs_f64().max(0.001)) as u64;
    on_progress(BurnProgress {
        bytes_done: done,
        bytes_total: total.max(done),
        bytes_per_sec: avg,
        elapsed,
    });
    Ok(BurnResult {
        bytes_written: done,
        source_sha256: hex(hasher.finalize()),
        elapsed,
        avg_bytes_per_sec: avg,
    })
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct VerifyMismatch {
    pub lba: String,
    pub byte_offset: String,
    pub expected: String,
    pub actual: String,
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct VerifyProgress {
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub bytes_per_sec: u64,
    pub elapsed: Duration,
}

#[allow(dead_code)]
pub struct VerifyResult {
    pub source_sha256: String,
    pub readback_sha256: String,
    pub match_: bool,
    pub bytes_checked: u64,
    pub bytes_total: u64,
    pub mismatches: Vec<VerifyMismatch>,
    pub elapsed: Duration,
    pub avg_bytes_per_sec: u64,
}

#[allow(dead_code)]
pub struct HashOnlyResult {
    pub readback_sha256: String,
    pub bytes_checked: u64,
    pub bytes_total: u64,
    pub elapsed: Duration,
    pub avg_bytes_per_sec: u64,
}

pub fn verify_hash_only(
    device: &mut dyn DeviceReader,
    expected_bytes: u64,
    chunk_size: usize,
    cancel: &AtomicBool,
    mut on_progress: impl FnMut(VerifyProgress),
) -> Result<HashOnlyResult, BurnError> {
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; chunk_size];
    let mut done: u64 = 0;
    let started = Instant::now();
    let mut last_emit = Instant::now();
    let mut window_start = Instant::now();
    let mut window_bytes: u64 = 0;

    while done < expected_bytes {
        if cancel.load(Ordering::Relaxed) {
            return Err(BurnError::Cancelled);
        }
        let remaining = expected_bytes - done;
        let cap = (buf.len() as u64).min(remaining) as usize;
        let n = device.read(&mut buf[..cap])?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        done += n as u64;
        window_bytes += n as u64;

        if last_emit.elapsed() >= Duration::from_millis(250) {
            let win = window_start.elapsed().as_secs_f64().max(0.001);
            let bps = (window_bytes as f64 / win) as u64;
            on_progress(VerifyProgress {
                bytes_done: done,
                bytes_total: expected_bytes,
                bytes_per_sec: bps,
                elapsed: started.elapsed(),
            });
            last_emit = Instant::now();
            window_start = Instant::now();
            window_bytes = 0;
        }
    }

    let elapsed = started.elapsed();
    let avg = (done as f64 / elapsed.as_secs_f64().max(0.001)) as u64;
    on_progress(VerifyProgress {
        bytes_done: done,
        bytes_total: expected_bytes.max(done),
        bytes_per_sec: avg,
        elapsed,
    });
    Ok(HashOnlyResult {
        readback_sha256: hex(hasher.finalize()),
        bytes_checked: done,
        bytes_total: expected_bytes.max(done),
        elapsed,
        avg_bytes_per_sec: avg,
    })
}

pub fn verify(
    source: &mut dyn ImageReader,
    device: &mut dyn DeviceReader,
    chunk_size: usize,
    cancel: &AtomicBool,
    mut on_progress: impl FnMut(VerifyProgress),
) -> Result<VerifyResult, BurnError> {
    let total = source.info().uncompressed_bytes;
    let mut src_hasher = Sha256::new();
    let mut dev_hasher = Sha256::new();
    let mut src_buf = vec![0u8; chunk_size];
    let mut dev_buf = vec![0u8; chunk_size];
    let mut mismatches: Vec<VerifyMismatch> = Vec::new();
    let mut done: u64 = 0;
    let started = Instant::now();
    let mut last_emit = Instant::now();
    let mut window_start = Instant::now();
    let mut window_bytes: u64 = 0;

    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(BurnError::Cancelled);
        }
        let n_src = read_full(source, &mut src_buf)?;
        if n_src == 0 {
            break;
        }
        let n_dev = read_full(device, &mut dev_buf[..n_src])?;
        if n_dev < n_src {
            for i in n_dev..n_src {
                push_mismatch(&mut mismatches, done + i as u64, &src_buf[i..i + 1], b"");
            }
        }
        src_hasher.update(&src_buf[..n_src]);
        dev_hasher.update(&dev_buf[..n_dev]);

        if mismatches.len() < MAX_MISMATCHES {
            scan_mismatches(
                &src_buf[..n_src],
                &dev_buf[..n_dev.min(n_src)],
                done,
                &mut mismatches,
            );
        }

        done += n_src as u64;
        window_bytes += n_src as u64;

        if last_emit.elapsed() >= Duration::from_millis(250) {
            let win = window_start.elapsed().as_secs_f64().max(0.001);
            let bps = (window_bytes as f64 / win) as u64;
            on_progress(VerifyProgress {
                bytes_done: done,
                bytes_total: total,
                bytes_per_sec: bps,
                elapsed: started.elapsed(),
            });
            last_emit = Instant::now();
            window_start = Instant::now();
            window_bytes = 0;
        }
    }

    let elapsed = started.elapsed();
    let avg = (done as f64 / elapsed.as_secs_f64().max(0.001)) as u64;
    let src_hash = hex(src_hasher.finalize());
    let dev_hash = hex(dev_hasher.finalize());
    Ok(VerifyResult {
        match_: src_hash == dev_hash && mismatches.is_empty(),
        source_sha256: src_hash,
        readback_sha256: dev_hash,
        bytes_checked: done,
        bytes_total: total.max(done),
        mismatches,
        elapsed,
        avg_bytes_per_sec: avg,
    })
}

fn read_full<R: Read + ?Sized>(r: &mut R, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match r.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) => return Err(e),
        }
    }
    Ok(filled)
}

fn scan_mismatches(src: &[u8], dev: &[u8], base: u64, out: &mut Vec<VerifyMismatch>) {
    let len = src.len().min(dev.len());
    let mut i = 0;
    while i < len && out.len() < MAX_MISMATCHES {
        if src[i] != dev[i] {
            let end = (i + 16).min(len);
            push_mismatch(out, base + i as u64, &src[i..end], &dev[i..end]);
            i = end;
        } else {
            i += 1;
        }
    }
}

fn push_mismatch(out: &mut Vec<VerifyMismatch>, byte_pos: u64, expected: &[u8], actual: &[u8]) {
    if out.len() >= MAX_MISMATCHES {
        return;
    }
    let lba = byte_pos / SECTOR_BYTES;
    let off = byte_pos % SECTOR_BYTES;
    out.push(VerifyMismatch {
        lba: format!("0x{:08X}", lba),
        byte_offset: format!("+0x{:04X}", off),
        expected: hex_bytes(expected),
        actual: hex_bytes(actual),
    });
}

fn hex(bytes: impl AsRef<[u8]>) -> String {
    let mut s = String::with_capacity(bytes.as_ref().len() * 2);
    for b in bytes.as_ref() {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

fn hex_bytes(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return "—".to_string();
    }
    bytes
        .iter()
        .map(|b| format!("{:02X}", b))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::readers::{ImageInfo, ImageReader};
    use crate::writers::{DeviceReader, DeviceWriter};
    use std::cell::RefCell;
    use std::io::{Cursor, Read as IoRead, Write as IoWrite};
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    struct MockImageReader {
        info: ImageInfo,
        inner: Cursor<Vec<u8>>,
    }

    impl MockImageReader {
        fn new(data: Vec<u8>) -> Self {
            let len = data.len() as u64;
            Self {
                info: ImageInfo {
                    path: PathBuf::from("/mock.img"),
                    format_label: "MOCK".into(),
                    source_bytes: len,
                    uncompressed_bytes: len,
                },
                inner: Cursor::new(data),
            }
        }
    }

    impl IoRead for MockImageReader {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.inner.read(buf)
        }
    }

    impl ImageReader for MockImageReader {
        fn info(&self) -> &ImageInfo {
            &self.info
        }
    }

    struct CollectingWriter {
        sink: Arc<Mutex<Vec<u8>>>,
        finished: Arc<AtomicBool>,
    }

    impl IoWrite for CollectingWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.sink.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl DeviceWriter for CollectingWriter {
        fn finish(self: Box<Self>) -> std::io::Result<()> {
            self.finished.store(true, Ordering::Relaxed);
            Ok(())
        }
    }

    struct CursorDeviceReader {
        inner: Cursor<Vec<u8>>,
    }

    impl IoRead for CursorDeviceReader {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.inner.read(buf)
        }
    }

    impl DeviceReader for CursorDeviceReader {}

    fn sha256_of(bytes: &[u8]) -> String {
        let mut h = Sha256::new();
        h.update(bytes);
        hex(h.finalize())
    }

    #[allow(clippy::type_complexity)]
    fn make_writer() -> (Box<dyn DeviceWriter>, Arc<Mutex<Vec<u8>>>, Arc<AtomicBool>) {
        let sink = Arc::new(Mutex::new(Vec::new()));
        let finished = Arc::new(AtomicBool::new(false));
        let w = Box::new(CollectingWriter {
            sink: sink.clone(),
            finished: finished.clone(),
        });
        (w, sink, finished)
    }

    #[test]
    fn burn_writes_all_source_bytes() {
        let data: Vec<u8> = (0..10_000u32).map(|i| (i % 256) as u8).collect();
        let mut reader = MockImageReader::new(data.clone());
        let (writer, sink, finished) = make_writer();
        let cancel = AtomicBool::new(false);

        let result = burn(&mut reader, writer, DEFAULT_CHUNK, &cancel, |_| {}).expect("burn ok");

        assert_eq!(result.bytes_written, data.len() as u64);
        assert_eq!(result.source_sha256, sha256_of(&data));
        assert_eq!(*sink.lock().unwrap(), data);
        assert!(finished.load(Ordering::Relaxed));
    }

    #[test]
    fn burn_handles_data_larger_than_one_chunk() {
        let data: Vec<u8> = vec![0xAB; DEFAULT_CHUNK + 1_000];
        let mut reader = MockImageReader::new(data.clone());
        let (writer, sink, _) = make_writer();
        let cancel = AtomicBool::new(false);

        let result = burn(&mut reader, writer, DEFAULT_CHUNK, &cancel, |_| {}).unwrap();

        assert_eq!(result.bytes_written, data.len() as u64);
        assert_eq!(*sink.lock().unwrap(), data);
    }

    #[test]
    fn burn_returns_cancelled_when_flag_set_before_start() {
        let mut reader = MockImageReader::new(vec![0u8; 1024]);
        let (writer, _, _) = make_writer();
        let cancel = AtomicBool::new(true);

        match burn(&mut reader, writer, DEFAULT_CHUNK, &cancel, |_| {}) {
            Err(BurnError::Cancelled) => {}
            Err(e) => panic!("expected Cancelled, got {e:?}"),
            Ok(_) => panic!("expected Err, got Ok"),
        }
    }

    #[test]
    fn burn_emits_final_progress_at_completion() {
        let mut reader = MockImageReader::new(vec![1u8; 64]);
        let (writer, _, _) = make_writer();
        let cancel = AtomicBool::new(false);
        let progress = RefCell::new(Vec::<BurnProgress>::new());

        burn(&mut reader, writer, DEFAULT_CHUNK, &cancel, |p| {
            progress.borrow_mut().push(p)
        })
        .unwrap();

        let last = progress.borrow().last().cloned().expect("progress");
        assert_eq!(last.bytes_done, 64);
        assert!(last.bytes_total >= 64);
    }

    #[test]
    fn verify_matches_identical_streams() {
        let data: Vec<u8> = (0..2048u32).map(|i| (i % 256) as u8).collect();
        let mut src = MockImageReader::new(data.clone());
        let mut dev = CursorDeviceReader {
            inner: Cursor::new(data.clone()),
        };
        let cancel = AtomicBool::new(false);

        let result = verify(&mut src, &mut dev, DEFAULT_CHUNK, &cancel, |_| {}).unwrap();

        assert!(result.match_);
        assert!(result.mismatches.is_empty());
        assert_eq!(result.bytes_checked, data.len() as u64);
        assert_eq!(result.source_sha256, result.readback_sha256);
    }

    #[test]
    fn verify_detects_mismatched_bytes() {
        let src_bytes = vec![0u8; 1024];
        let mut dev_bytes = src_bytes.clone();
        dev_bytes[100] = 0xFF;
        dev_bytes[200] = 0xAA;
        let mut src = MockImageReader::new(src_bytes);
        let mut dev = CursorDeviceReader {
            inner: Cursor::new(dev_bytes),
        };
        let cancel = AtomicBool::new(false);

        let result = verify(&mut src, &mut dev, DEFAULT_CHUNK, &cancel, |_| {}).unwrap();

        assert!(!result.match_);
        assert!(!result.mismatches.is_empty());
        assert_ne!(result.source_sha256, result.readback_sha256);
    }

    #[test]
    fn verify_reports_mismatches_when_device_truncated() {
        let src_bytes = vec![1u8; 1024];
        let dev_bytes = vec![1u8; 512];
        let mut src = MockImageReader::new(src_bytes);
        let mut dev = CursorDeviceReader {
            inner: Cursor::new(dev_bytes),
        };
        let cancel = AtomicBool::new(false);

        let result = verify(&mut src, &mut dev, DEFAULT_CHUNK, &cancel, |_| {}).unwrap();

        assert!(!result.match_);
        assert!(!result.mismatches.is_empty());
    }

    #[test]
    fn verify_caps_at_max_mismatches() {
        let src_bytes = vec![0u8; 16 * 1024];
        let dev_bytes = vec![0xFFu8; 16 * 1024];
        let mut src = MockImageReader::new(src_bytes);
        let mut dev = CursorDeviceReader {
            inner: Cursor::new(dev_bytes),
        };
        let cancel = AtomicBool::new(false);

        let result = verify(&mut src, &mut dev, DEFAULT_CHUNK, &cancel, |_| {}).unwrap();

        assert!(result.mismatches.len() <= MAX_MISMATCHES);
    }

    #[test]
    fn verify_returns_cancelled_when_flag_set_first() {
        let mut src = MockImageReader::new(vec![0u8; 1024]);
        let mut dev = CursorDeviceReader {
            inner: Cursor::new(vec![0u8; 1024]),
        };
        let cancel = AtomicBool::new(true);

        match verify(&mut src, &mut dev, DEFAULT_CHUNK, &cancel, |_| {}) {
            Err(BurnError::Cancelled) => {}
            Err(e) => panic!("expected Cancelled, got {e:?}"),
            Ok(_) => panic!("expected Err, got Ok"),
        }
    }

    #[test]
    fn read_full_fills_buffer() {
        let data = b"hello world";
        let mut cur = Cursor::new(&data[..]);
        let mut out = [0u8; 11];
        let n = read_full(&mut cur, &mut out).unwrap();
        assert_eq!(n, 11);
        assert_eq!(&out, b"hello world");
    }

    #[test]
    fn read_full_returns_partial_at_eof() {
        let data = b"hi";
        let mut cur = Cursor::new(&data[..]);
        let mut out = [0u8; 8];
        let n = read_full(&mut cur, &mut out).unwrap();
        assert_eq!(n, 2);
        assert_eq!(&out[..n], b"hi");
    }

    #[test]
    fn scan_mismatches_finds_byte_differences() {
        let src = b"AAAAAAAA";
        let dev = b"AABAAAAA";
        let mut out = Vec::new();
        scan_mismatches(src, dev, 0, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].byte_offset, "+0x0002");
    }

    #[test]
    fn scan_mismatches_advances_past_a_run_in_16_byte_chunks() {
        let src = vec![0u8; 100];
        let dev = vec![0xFFu8; 100];
        let mut out = Vec::new();
        scan_mismatches(&src, &dev, 0, &mut out);
        // Each mismatch entry covers 16 bytes; ceil(100 / 16) = 7 entries.
        assert_eq!(out.len(), 7);
    }

    #[test]
    fn push_mismatch_formats_lba_and_offset() {
        let mut out = Vec::new();
        push_mismatch(&mut out, 1024 + 5, b"\x01", b"\x02");
        let m = &out[0];
        assert_eq!(m.lba, "0x00000002");
        assert_eq!(m.byte_offset, "+0x0005");
        assert_eq!(m.expected, "01");
        assert_eq!(m.actual, "02");
    }

    #[test]
    fn push_mismatch_respects_cap() {
        let mut out: Vec<VerifyMismatch> = (0..MAX_MISMATCHES)
            .map(|i| VerifyMismatch {
                lba: format!("{i}"),
                byte_offset: "".into(),
                expected: "".into(),
                actual: "".into(),
            })
            .collect();
        push_mismatch(&mut out, 0, b"\x01", b"\x02");
        assert_eq!(out.len(), MAX_MISMATCHES);
    }

    #[test]
    fn hex_is_lowercase_and_zero_padded() {
        assert_eq!(hex([0xAB, 0xCD, 0x00]), "abcd00");
        assert_eq!(hex([] as [u8; 0]), "");
    }

    #[test]
    fn hex_bytes_renders_empty_dash_and_uppercase_with_separators() {
        assert_eq!(hex_bytes(&[]), "—");
        assert_eq!(hex_bytes(&[0x0A, 0xFF]), "0A FF");
    }

    #[test]
    fn hex_zero_byte_round_trips_as_00() {
        assert_eq!(hex([0x00u8]), "00");
    }

    #[test]
    fn hex_full_byte_round_trips_as_ff() {
        assert_eq!(hex([0xFFu8]), "ff");
    }

    #[test]
    fn hex_multi_byte_preserves_ordering() {
        assert_eq!(
            hex([0x01, 0x02, 0x03, 0xDE, 0xAD, 0xBE, 0xEF]),
            "010203deadbeef"
        );
    }

    #[test]
    fn hex_bytes_single_byte_has_no_separator() {
        assert_eq!(hex_bytes(&[0x7F]), "7F");
    }

    #[test]
    fn hex_bytes_zero_and_ff_render_uppercase_padded() {
        assert_eq!(hex_bytes(&[0x00]), "00");
        assert_eq!(hex_bytes(&[0xFF]), "FF");
    }

    #[test]
    fn scan_mismatches_finds_nothing_for_identical_buffers() {
        let buf = vec![0xAB; 4096];
        let mut out = Vec::new();
        scan_mismatches(&buf, &buf, 0, &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn scan_mismatches_records_base_offset() {
        // One differing byte at index 3 inside a chunk whose base offset is 1024
        // should produce an LBA derived from byte_pos = 1024 + 3 = 1027.
        let src = vec![0u8; 16];
        let mut dev = src.clone();
        dev[3] = 0x99;
        let mut out = Vec::new();
        scan_mismatches(&src, &dev, 1024, &mut out);
        assert_eq!(out.len(), 1);
        // 1027 / 512 = 2; 1027 % 512 = 3
        assert_eq!(out[0].lba, "0x00000002");
        assert_eq!(out[0].byte_offset, "+0x0003");
    }

    #[test]
    fn scan_mismatches_respects_max_cap() {
        // Pre-fill out near the cap; verify the function stops adding past MAX_MISMATCHES.
        let src = vec![0u8; 16 * 1024];
        let dev = vec![0xFFu8; 16 * 1024];
        let mut out: Vec<VerifyMismatch> = (0..MAX_MISMATCHES - 1)
            .map(|i| VerifyMismatch {
                lba: format!("{i}"),
                byte_offset: "".into(),
                expected: "".into(),
                actual: "".into(),
            })
            .collect();
        scan_mismatches(&src, &dev, 0, &mut out);
        assert_eq!(out.len(), MAX_MISMATCHES);
    }

    #[test]
    fn scan_mismatches_takes_shorter_of_two_buffers() {
        // src longer than dev — only `dev.len()` bytes get compared.
        let src = vec![0u8; 100];
        let dev = vec![0xFFu8; 16];
        let mut out = Vec::new();
        scan_mismatches(&src, &dev, 0, &mut out);
        // 16 bytes compared, all differ, formed into one 16-byte chunk.
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn push_mismatch_at_zero_byte_pos_renders_zero_lba_and_offset() {
        let mut out = Vec::new();
        push_mismatch(&mut out, 0, b"\xAA", b"\xBB");
        assert_eq!(out[0].lba, "0x00000000");
        assert_eq!(out[0].byte_offset, "+0x0000");
        assert_eq!(out[0].expected, "AA");
        assert_eq!(out[0].actual, "BB");
    }

    #[test]
    fn push_mismatch_empty_actual_renders_dash() {
        let mut out = Vec::new();
        push_mismatch(&mut out, 0, b"\x01", b"");
        assert_eq!(out[0].actual, "—");
    }

    #[test]
    fn push_mismatch_does_nothing_when_already_at_cap() {
        let mut out: Vec<VerifyMismatch> = (0..MAX_MISMATCHES)
            .map(|_| VerifyMismatch {
                lba: "x".into(),
                byte_offset: "y".into(),
                expected: "".into(),
                actual: "".into(),
            })
            .collect();
        let snapshot_first = out[0].clone();
        push_mismatch(&mut out, 0, b"\x01", b"\x02");
        assert_eq!(out.len(), MAX_MISMATCHES);
        // First entry untouched (we don't mutate existing entries).
        assert_eq!(out[0].lba, snapshot_first.lba);
    }

    #[test]
    fn read_full_returns_zero_at_eof() {
        let data: &[u8] = b"";
        let mut cur = Cursor::new(data);
        let mut out = [0u8; 8];
        let n = read_full(&mut cur, &mut out).unwrap();
        assert_eq!(n, 0);
    }

    struct ShortReader {
        chunks: Vec<Vec<u8>>,
    }

    impl IoRead for ShortReader {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.chunks.is_empty() {
                return Ok(0);
            }
            let next = self.chunks.remove(0);
            let n = next.len().min(buf.len());
            buf[..n].copy_from_slice(&next[..n]);
            Ok(n)
        }
    }

    #[test]
    fn read_full_loops_over_short_reads_until_filled() {
        // Reader emits 3+3+2 bytes for an 8-byte buffer.
        let mut r = ShortReader {
            chunks: vec![vec![1, 2, 3], vec![4, 5, 6], vec![7, 8]],
        };
        let mut out = [0u8; 8];
        let n = read_full(&mut r, &mut out).unwrap();
        assert_eq!(n, 8);
        assert_eq!(&out, &[1, 2, 3, 4, 5, 6, 7, 8]);
    }

    struct FailingReader;

    impl IoRead for FailingReader {
        fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("boom"))
        }
    }

    #[test]
    fn read_full_propagates_io_error() {
        let mut r = FailingReader;
        let mut out = [0u8; 4];
        let err = read_full(&mut r, &mut out).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::Other);
    }
}
