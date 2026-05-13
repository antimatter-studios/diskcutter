//! Command-line surface for Disk Cutter.
//!
//! Lives alongside the Tauri GUI: when the binary is invoked with a
//! subcommand argv (e.g. `disk-cutter inspect foo.qcow2`), main.rs
//! routes here instead of launching the GUI. This lets us run Disk
//! Cutter scriptably from a shell, a CI job, or a make target,
//! reusing the exact same readers / backup engine / partition probe
//! the GUI uses.
//!
//! Subcommands:
//!   - `inspect <path>`        — partition table + filesystem probe
//!   - `formats`               — list supported reader formats
//!   - `backup <src> <out>`    — disk-to-image (with compression + sparse)
//!   - `snapshot <dev> <out>`  — capture target's first 4 MiB to recovery file
//!   - `restore <rec> <dev>`   — write recovery file back to device
//!   - `version`               — print package version
//!   - `help`                  — print usage
//!
//! Subcommand parsing is a pure function — `parse(args) -> Command`
//! produces a typed AST that the runner consumes. Unit tests cover
//! the parsing without touching the filesystem.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use crate::backup::{self, BackupOptions, Compression};
use crate::inspect;
use crate::snapshot::{self, DEFAULT_SNAPSHOT_BYTES};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Help,
    Version,
    Formats,
    Inspect {
        path: PathBuf,
    },
    Backup {
        source: PathBuf,
        output: PathBuf,
        compression: Compression,
        sparse: bool,
    },
    Snapshot {
        device: PathBuf,
        output: PathBuf,
        bytes: u64,
    },
    Restore {
        recovery: PathBuf,
        device: PathBuf,
    },
    Invalid(String),
}

/// Parse argv (minus the program name) into a `Command`. Pure
/// function, no filesystem or environment access. Unknown options
/// or missing positional arguments are folded into `Invalid` so the
/// caller can print a usage message and exit non-zero.
pub fn parse(args: &[String]) -> Command {
    if args.is_empty() {
        return Command::Help;
    }
    let head = args[0].as_str();
    match head {
        "help" | "-h" | "--help" => Command::Help,
        "version" | "-v" | "--version" => Command::Version,
        "formats" => Command::Formats,
        "inspect" => {
            if args.len() < 2 {
                return Command::Invalid("inspect needs a path".into());
            }
            Command::Inspect {
                path: PathBuf::from(&args[1]),
            }
        }
        "backup" => parse_backup(&args[1..]),
        "snapshot" => parse_snapshot(&args[1..]),
        "restore" => {
            if args.len() < 3 {
                return Command::Invalid("restore needs <recovery-file> <device>".into());
            }
            Command::Restore {
                recovery: PathBuf::from(&args[1]),
                device: PathBuf::from(&args[2]),
            }
        }
        other => Command::Invalid(format!("unknown subcommand: {other}")),
    }
}

fn parse_backup(rest: &[String]) -> Command {
    if rest.len() < 2 {
        return Command::Invalid("backup needs <source> <output> [options]".into());
    }
    let source = PathBuf::from(&rest[0]);
    let output = PathBuf::from(&rest[1]);
    let mut compression = Compression::None;
    let mut sparse = false;
    for flag in &rest[2..] {
        if let Some(v) = flag.strip_prefix("--compression=") {
            match Compression::parse(v) {
                Some(c) => compression = c,
                None => return Command::Invalid(format!("unknown compression: {v}")),
            }
        } else if flag == "--sparse" {
            sparse = true;
        } else {
            return Command::Invalid(format!("unknown backup flag: {flag}"));
        }
    }
    Command::Backup {
        source,
        output,
        compression,
        sparse,
    }
}

fn parse_snapshot(rest: &[String]) -> Command {
    if rest.len() < 2 {
        return Command::Invalid("snapshot needs <device> <output> [--bytes=N]".into());
    }
    let device = PathBuf::from(&rest[0]);
    let output = PathBuf::from(&rest[1]);
    let mut bytes = DEFAULT_SNAPSHOT_BYTES;
    for flag in &rest[2..] {
        if let Some(v) = flag.strip_prefix("--bytes=") {
            match v.parse::<u64>() {
                Ok(n) => bytes = n,
                Err(_) => return Command::Invalid(format!("--bytes= needs a u64: {v}")),
            }
        } else {
            return Command::Invalid(format!("unknown snapshot flag: {flag}"));
        }
    }
    Command::Snapshot {
        device,
        output,
        bytes,
    }
}

const USAGE: &str = r#"Disk Cutter — brutalist disk-image writer

usage: disk-cutter <command> [args]

commands:
  inspect  <path>                     partition + filesystem probe of an image
  formats                             list supported image / compression formats
  backup   <src> <out> [opts]         disk-to-image; --compression=none|gz|xz|bz2|zst, --sparse
  snapshot <dev> <out> [--bytes=N]    capture first N bytes of device (default 4 MiB)
  restore  <rec> <dev>                write a recovery file back to the device's first bytes
  version                             print version
  help                                this message

with no command, launches the GUI.
"#;

const FORMATS: &str = r#"Image formats:
  raw      .iso .img .bin .raw
  gzip     .gz   (sniffed by magic; .iso.gz / .img.gz)
  xz       .xz   (sniffed by magic)
  bzip2    .bz2  .bzip2
  zstd     .zst  .zstd
  qcow2    .qcow2 .qcow
  vhd      .vhd
  vhdx     .vhdx
  vmdk     .vmdk

Magic-byte sniffing on probe — a file renamed (e.g. ubuntu.iso.gz → ubuntu.iso)
is still routed to the correct reader.
"#;

/// Top-level CLI runner. Parses argv, dispatches, returns a process
/// exit code. The runner side does I/O; the parsing side is pure.
pub fn run_cli(args: &[String]) -> i32 {
    match parse(args) {
        Command::Help => {
            println!("{USAGE}");
            0
        }
        Command::Version => {
            println!("{}", env!("CARGO_PKG_VERSION"));
            0
        }
        Command::Formats => {
            println!("{FORMATS}");
            0
        }
        Command::Inspect { path } => run_inspect(&path),
        Command::Backup {
            source,
            output,
            compression,
            sparse,
        } => run_backup(&source, &output, compression, sparse),
        Command::Snapshot {
            device,
            output,
            bytes,
        } => run_snapshot(&device, &output, bytes),
        Command::Restore { recovery, device } => run_restore(&recovery, &device),
        Command::Invalid(msg) => {
            eprintln!("disk-cutter: {msg}");
            eprintln!();
            eprintln!("{USAGE}");
            2
        }
    }
}

fn run_inspect(path: &std::path::Path) -> i32 {
    match inspect::inspect_any(path) {
        Some(summary) => {
            println!("Partition table: {}", summary.table_kind);
            for p in &summary.partitions {
                let fs = p.filesystem.as_deref().unwrap_or("—");
                let label = p.label.as_deref().unwrap_or("");
                println!(
                    "  {:>2}  {:>10}  {:<22}  fs={:<10}  label={}",
                    p.index, p.size_human, p.kind_label, fs, label
                );
            }
            0
        }
        None => {
            eprintln!(
                "disk-cutter: no partition table found in {}",
                path.display()
            );
            1
        }
    }
}

fn run_backup(
    source: &std::path::Path,
    output: &std::path::Path,
    compression: Compression,
    sparse: bool,
) -> i32 {
    let source_bytes = match backup::probe_source_size(source) {
        Ok(n) => n,
        Err(e) => {
            eprintln!("disk-cutter: probe source: {e}");
            return 1;
        }
    };
    let options = BackupOptions {
        source_path: source.to_path_buf(),
        output_path: output.to_path_buf(),
        compression,
        chunk_size: 1024 * 1024,
        source_bytes,
        sparse,
    };
    let cancel = AtomicBool::new(false);
    let mut last_pct: u32 = 0;
    let on_progress = |p: backup::BackupProgress| {
        if p.bytes_total > 0 {
            let pct = ((p.bytes_done as f64 / p.bytes_total as f64) * 100.0) as u32;
            if pct >= last_pct + 5 || pct == 100 {
                eprintln!(
                    "  {pct:>3}%  {} MiB / {} MiB",
                    p.bytes_done / (1024 * 1024),
                    p.bytes_total / (1024 * 1024)
                );
                last_pct = pct;
            }
        }
    };
    match backup::run_to_file(&options, &cancel, on_progress) {
        Ok(r) => {
            println!("backup complete");
            println!("  bytes_read:      {}", r.bytes_read);
            println!("  bytes_written:   {}", r.bytes_written);
            println!("  source_sha256:   {}", r.source_sha256);
            println!("  elapsed_ms:      {}", r.elapsed.as_millis());
            println!("  avg_bps:         {}", r.avg_bytes_per_sec);
            0
        }
        Err(e) => {
            eprintln!("disk-cutter: backup: {e:?}");
            1
        }
    }
}

fn run_snapshot(device: &std::path::Path, output: &std::path::Path, bytes: u64) -> i32 {
    match snapshot::snapshot_target(device, output, bytes) {
        Ok(r) => {
            println!("snapshot complete");
            println!("  recovery_path:   {}", r.recovery_path.display());
            println!("  captured_bytes:  {}", r.header.snapshot_bytes);
            println!("  source_size:     {}", r.header.source_size_bytes);
            println!("  sha256:          {}", r.header.sha256);
            0
        }
        Err(e) => {
            eprintln!("disk-cutter: snapshot: {e:?}");
            1
        }
    }
}

fn run_restore(recovery: &std::path::Path, device: &std::path::Path) -> i32 {
    match snapshot::restore_target(recovery, device) {
        Ok(r) => {
            println!("restore complete");
            println!("  device:          {}", r.device_path.display());
            println!("  bytes_written:   {}", r.bytes_written);
            println!("  sha256_verified: {}", r.verified_sha256);
            0
        }
        Err(e) => {
            eprintln!("disk-cutter: restore: {e:?}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parse_empty_args_returns_help() {
        assert_eq!(parse(&[]), Command::Help);
    }

    #[test]
    fn parse_help_variants() {
        for v in &["help", "-h", "--help"] {
            assert_eq!(parse(&args(&[v])), Command::Help);
        }
    }

    #[test]
    fn parse_version_variants() {
        for v in &["version", "-v", "--version"] {
            assert_eq!(parse(&args(&[v])), Command::Version);
        }
    }

    #[test]
    fn parse_formats_subcommand() {
        assert_eq!(parse(&args(&["formats"])), Command::Formats);
    }

    #[test]
    fn parse_inspect_with_path() {
        match parse(&args(&["inspect", "/tmp/x.iso"])) {
            Command::Inspect { path } => assert_eq!(path, PathBuf::from("/tmp/x.iso")),
            other => panic!("expected Inspect, got {other:?}"),
        }
    }

    #[test]
    fn parse_inspect_without_path_is_invalid() {
        match parse(&args(&["inspect"])) {
            Command::Invalid(_) => {}
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn parse_backup_basic() {
        match parse(&args(&["backup", "/dev/disk5", "out.img"])) {
            Command::Backup {
                source,
                output,
                compression,
                sparse,
            } => {
                assert_eq!(source, PathBuf::from("/dev/disk5"));
                assert_eq!(output, PathBuf::from("out.img"));
                assert_eq!(compression, Compression::None);
                assert!(!sparse);
            }
            other => panic!("expected Backup, got {other:?}"),
        }
    }

    #[test]
    fn parse_backup_with_compression_and_sparse() {
        match parse(&args(&[
            "backup",
            "src",
            "out.img.xz",
            "--compression=xz",
            "--sparse",
        ])) {
            Command::Backup {
                compression,
                sparse,
                ..
            } => {
                assert_eq!(compression, Compression::Xz);
                assert!(sparse);
            }
            other => panic!("expected Backup, got {other:?}"),
        }
    }

    #[test]
    fn parse_backup_rejects_unknown_compression() {
        match parse(&args(&["backup", "src", "out", "--compression=brotli"])) {
            Command::Invalid(m) => assert!(m.contains("brotli")),
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn parse_backup_rejects_unknown_flag() {
        match parse(&args(&["backup", "src", "out", "--lz4"])) {
            Command::Invalid(m) => assert!(m.contains("--lz4")),
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn parse_backup_without_args_is_invalid() {
        match parse(&args(&["backup", "only-source"])) {
            Command::Invalid(_) => {}
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn parse_snapshot_default_bytes() {
        match parse(&args(&["snapshot", "/dev/disk5", "recov.bin"])) {
            Command::Snapshot { bytes, .. } => assert_eq!(bytes, DEFAULT_SNAPSHOT_BYTES),
            other => panic!("expected Snapshot, got {other:?}"),
        }
    }

    #[test]
    fn parse_snapshot_with_custom_bytes() {
        match parse(&args(&[
            "snapshot",
            "/dev/disk5",
            "recov.bin",
            "--bytes=1048576",
        ])) {
            Command::Snapshot { bytes, .. } => assert_eq!(bytes, 1024 * 1024),
            other => panic!("expected Snapshot, got {other:?}"),
        }
    }

    #[test]
    fn parse_snapshot_rejects_non_numeric_bytes() {
        match parse(&args(&["snapshot", "dev", "out", "--bytes=many"])) {
            Command::Invalid(_) => {}
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn parse_restore_pairs_args() {
        match parse(&args(&["restore", "recov.bin", "/dev/disk5"])) {
            Command::Restore { recovery, device } => {
                assert_eq!(recovery, PathBuf::from("recov.bin"));
                assert_eq!(device, PathBuf::from("/dev/disk5"));
            }
            other => panic!("expected Restore, got {other:?}"),
        }
    }

    #[test]
    fn parse_restore_without_device_is_invalid() {
        match parse(&args(&["restore", "only-recovery"])) {
            Command::Invalid(_) => {}
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn parse_unknown_subcommand_is_invalid() {
        match parse(&args(&["frobnicate"])) {
            Command::Invalid(m) => assert!(m.contains("frobnicate")),
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn run_cli_help_exits_zero() {
        assert_eq!(run_cli(&args(&["help"])), 0);
    }

    #[test]
    fn run_cli_version_exits_zero() {
        assert_eq!(run_cli(&args(&["version"])), 0);
    }

    #[test]
    fn run_cli_formats_exits_zero() {
        assert_eq!(run_cli(&args(&["formats"])), 0);
    }

    #[test]
    fn run_cli_invalid_subcommand_exits_two() {
        assert_eq!(run_cli(&args(&["frobnicate"])), 2);
    }

    #[test]
    fn run_cli_inspect_missing_file_exits_one() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("ghost.iso");
        let s = p.to_string_lossy().to_string();
        assert_eq!(run_cli(&args(&["inspect", &s])), 1);
    }

    #[test]
    fn run_cli_backup_end_to_end_against_real_files() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.bin");
        let out = dir.path().join("out.bin");
        std::fs::write(&src, vec![0xABu8; 4096]).unwrap();
        let src_s = src.to_string_lossy().to_string();
        let out_s = out.to_string_lossy().to_string();
        let rc = run_cli(&args(&["backup", &src_s, &out_s]));
        assert_eq!(rc, 0);
        assert_eq!(std::fs::read(&out).unwrap(), vec![0xABu8; 4096]);
    }

    #[test]
    fn run_cli_snapshot_then_restore_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("dev.img");
        let recov = dir.path().join("dev.recov");
        let dest = dir.path().join("dest.img");
        std::fs::write(&src, vec![0x42u8; 1024]).unwrap();
        std::fs::write(&dest, vec![0u8; 1024]).unwrap();
        let src_s = src.to_string_lossy().to_string();
        let recov_s = recov.to_string_lossy().to_string();
        let dest_s = dest.to_string_lossy().to_string();
        assert_eq!(
            run_cli(&args(&["snapshot", &src_s, &recov_s, "--bytes=1024"])),
            0
        );
        assert_eq!(run_cli(&args(&["restore", &recov_s, &dest_s])), 0);
        assert_eq!(std::fs::read(&dest).unwrap(), vec![0x42u8; 1024]);
    }
}
