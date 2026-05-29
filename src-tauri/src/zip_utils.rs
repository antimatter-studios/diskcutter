//! Shared helpers for ZIP-backed disk images.
//!
//! ZIP requires `Read + Seek` to parse the central directory, so it cannot
//! go through the streaming `DiskReader` decoder chain. These helpers find
//! the relevant image entry and expose a channel-based streaming reader that
//! bridges the seek requirement to the write pipeline's `Read`-only interface.

use std::io::{self, BufReader, Read, Seek};
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;

/// Prefer entries with these extensions; fall back to first non-directory entry.
const IMAGE_EXTENSIONS: &[&str] = &["img", "iso", "bin", "raw"];

/// Find the index of the best image entry in a ZIP archive.
/// Prefers known image extensions; falls back to the first non-directory entry.
pub fn find_image_entry_index<R: Read + Seek>(archive: &mut zip::ZipArchive<R>) -> Option<usize> {
    // First pass: image extension
    for i in 0..archive.len() {
        if let Ok(entry) = archive.by_index(i) {
            if !entry.is_dir() {
                let name = entry.name().to_lowercase();
                if IMAGE_EXTENSIONS
                    .iter()
                    .any(|e| name.ends_with(&format!(".{e}")))
                {
                    return Some(i);
                }
            }
        }
    }
    // Fallback: first non-directory
    for i in 0..archive.len() {
        if let Ok(entry) = archive.by_index(i) {
            if !entry.is_dir() {
                return Some(i);
            }
        }
    }
    None
}

/// Streaming reader for a single ZIP entry. Uses a background thread +
/// channel to avoid lifetime issues with ZipFile borrowing ZipArchive.
pub struct ZipChannelReader {
    rx: mpsc::Receiver<io::Result<Vec<u8>>>,
    current: Vec<u8>,
    pos: usize,
}

impl ZipChannelReader {
    pub fn new(path: PathBuf, entry_idx: usize) -> io::Result<Self> {
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let result: io::Result<()> = (|| {
                let file = std::fs::File::open(&path)?;
                let mut archive =
                    zip::ZipArchive::new(BufReader::new(file)).map_err(io::Error::other)?;
                let mut entry = archive.by_index(entry_idx).map_err(io::Error::other)?;
                let mut buf = vec![0u8; 256 * 1024];
                loop {
                    let n = entry.read(&mut buf)?;
                    if n == 0 {
                        break;
                    }
                    if tx.send(Ok(buf[..n].to_vec())).is_err() {
                        break;
                    }
                }
                Ok(())
            })();
            if let Err(e) = result {
                tx.send(Err(e)).ok();
            }
        });
        Ok(Self {
            rx,
            current: Vec::new(),
            pos: 0,
        })
    }
}

impl Read for ZipChannelReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        while self.pos >= self.current.len() {
            match self.rx.recv() {
                Ok(Ok(chunk)) => {
                    self.current = chunk;
                    self.pos = 0;
                }
                Ok(Err(e)) => return Err(e),
                Err(_) => return Ok(0),
            }
        }
        let available = &self.current[self.pos..];
        let n = buf.len().min(available.len());
        buf[..n].copy_from_slice(&available[..n]);
        self.pos += n;
        Ok(n)
    }
}
