use std::fs::{File, OpenOptions};
use std::io::{Read, Result, Write};
use std::path::Path;

use super::{DeviceIo, DeviceReader, DeviceWriter};

pub struct PlainFileDeviceIo;

impl DeviceIo for PlainFileDeviceIo {
    fn name(&self) -> &'static str {
        "plain-file"
    }

    fn open_write(&self, device: &Path) -> Result<Box<dyn DeviceWriter>> {
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(device)?;
        Ok(Box::new(PlainWriter { file }))
    }

    fn open_read(&self, device: &Path) -> Result<Box<dyn DeviceReader>> {
        let file = File::open(device)?;
        Ok(Box::new(PlainReader { file }))
    }
}

pub struct PlainWriter {
    file: File,
}

impl Write for PlainWriter {
    fn write(&mut self, buf: &[u8]) -> Result<usize> {
        self.file.write(buf)
    }
    fn flush(&mut self) -> Result<()> {
        self.file.flush()
    }
}

impl DeviceWriter for PlainWriter {
    fn finish(mut self: Box<Self>) -> Result<()> {
        self.file.flush()?;
        self.file.sync_all()
    }
}

pub struct PlainReader {
    file: File,
}

impl Read for PlainReader {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        self.file.read(buf)
    }
}

impl DeviceReader for PlainReader {}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn name_is_plain_file() {
        assert_eq!(PlainFileDeviceIo.name(), "plain-file");
    }

    #[test]
    fn round_trip_write_then_read() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("buf.img");
        let io = PlainFileDeviceIo;

        let mut writer = io.open_write(&p).unwrap();
        writer.write_all(b"hello world").unwrap();
        writer.finish().unwrap();

        let mut reader = io.open_read(&p).unwrap();
        let mut out = Vec::new();
        Read::read_to_end(&mut reader, &mut out).unwrap();
        assert_eq!(out, b"hello world");
    }

    #[test]
    fn open_write_truncates_existing_file() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("buf.img");
        std::fs::write(&p, b"old long content").unwrap();
        let io = PlainFileDeviceIo;

        let mut writer = io.open_write(&p).unwrap();
        writer.write_all(b"new").unwrap();
        writer.finish().unwrap();

        let bytes = std::fs::read(&p).unwrap();
        assert_eq!(bytes, b"new");
    }

    #[test]
    fn open_read_errors_on_missing_file() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("ghost.img");
        let io = PlainFileDeviceIo;
        assert!(io.open_read(&p).is_err());
    }

    #[test]
    fn open_write_creates_missing_file() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("fresh.img");
        assert!(!p.exists());
        let io = PlainFileDeviceIo;

        let mut writer = io.open_write(&p).unwrap();
        writer.write_all(b"x").unwrap();
        writer.finish().unwrap();

        assert!(p.exists());
        assert_eq!(std::fs::read(&p).unwrap(), b"x");
    }

    #[test]
    fn round_trip_empty_payload() {
        // Truncate to zero, finish without writing — readback must be empty.
        let dir = tempdir().unwrap();
        let p = dir.path().join("empty.img");
        std::fs::write(&p, b"prev").unwrap();
        let io = PlainFileDeviceIo;

        let writer = io.open_write(&p).unwrap();
        writer.finish().unwrap();

        let mut reader = io.open_read(&p).unwrap();
        let mut out = Vec::new();
        Read::read_to_end(&mut reader, &mut out).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn multiple_writes_concat_in_order() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("multi.img");
        let io = PlainFileDeviceIo;

        let mut writer = io.open_write(&p).unwrap();
        writer.write_all(b"foo").unwrap();
        writer.write_all(b"bar").unwrap();
        writer.write_all(b"baz").unwrap();
        writer.finish().unwrap();

        assert_eq!(std::fs::read(&p).unwrap(), b"foobarbaz");
    }

    #[test]
    fn open_write_errors_on_unwritable_directory() {
        // Path under a nonexistent directory — open should fail.
        let dir = tempdir().unwrap();
        let p = dir.path().join("no/such/dir/file.img");
        let io = PlainFileDeviceIo;
        assert!(io.open_write(&p).is_err());
    }
}
