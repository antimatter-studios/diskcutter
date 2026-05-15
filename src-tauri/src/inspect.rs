//! Pre-burn image inspection: partition table + per-partition filesystem
//! probe.
//!
//! Lets the UI show "this image holds: GPT, 3 partitions — EFI System
//! (FAT32, 100 MiB) · Linux filesystem (ext4, 12 GB) · swap (1 GB)"
//! before the user commits to a burn. All probes are read-only and
//! capped at the first kilobyte of each partition, so inspect of a
//! multi-GB image is still a sub-second operation.
//!
//! For raw / ISO sources the image file is opened directly as a
//! `fs_core::FileDevice`. Container formats (qcow2 / vhd / vhdx / vmdk)
//! present their virtual disk through their `BlockRead` impl, so the
//! same partition probe code path works against them. Compressed
//! sources (.gz / .xz / .bz2 / .zst) can't be probed without
//! decompressing — the caller gets `None` and the UI surfaces "extract
//! the archive first".
//!
//! Everything that doesn't need filesystem I/O is a pure function with
//! unit tests: GUID/byte → label maps, `FsKind` → display string,
//! bytes → human size.

use std::path::Path;

use fs_core::{BlockRead, FileDevice};
use partitions::{
    probe::{probe, Partition, PartitionKind, TableKind},
    sniff::{classify, sniff, ExtVersion, FsKind},
};
use serde::Serialize;

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct PartitionSummary {
    pub table_kind: String,
    pub partitions: Vec<PartInfo>,
    /// True when any partition in the table is marked bootable — the UI
    /// uses this to surface a whole-image "BOOTABLE" pill in the
    /// partition header.
    pub any_bootable: bool,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct PartInfo {
    pub index: u32,
    pub start_bytes: u64,
    pub length_bytes: u64,
    pub size_human: String,
    pub kind_label: String,
    pub type_id: String,
    pub label: Option<String>,
    pub uuid: Option<String>,
    pub filesystem: Option<String>,
    /// True when the on-disk table marks this partition bootable:
    /// MBR active flag set, GPT legacy-BIOS-bootable attribute set, or
    /// GPT type is the EFI System Partition GUID. Comes from
    /// `Partition::is_bootable` upstream.
    pub bootable: bool,
}

/// Inspect a raw / ISO image path. Returns `None` for unrecognised or
/// table-less sources (single-filesystem images, completely empty
/// devices, anything that doesn't probe to a recognised partition
/// table).
pub fn inspect_raw_image(path: &Path) -> Option<PartitionSummary> {
    let dev = FileDevice::open(path).ok()?;
    summarise(&dev).ok()
}

/// Inspect any supported image format. For raw / iso the file is
/// opened directly; for container formats (qcow2 / vhd / vhdx / vmdk)
/// the upstream reader's `BlockRead` view of the virtual disk is
/// passed to the partition probe. Compressed formats can't be probed
/// without decompressing — caller gets `None`.
pub fn inspect_any(path: &Path) -> Option<PartitionSummary> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase());
    match ext.as_deref() {
        Some("qcow2") | Some("qcow") => {
            let r = qcow2::Qcow2Reader::open(path).ok()?;
            inspect_block_read(&r)
        }
        Some("vhd") => {
            let r = vhd::VhdReader::open(path).ok()?;
            inspect_block_read(&r)
        }
        Some("vhdx") => {
            let r = vhdx::VhdxReader::open(path).ok()?;
            inspect_block_read(&r)
        }
        Some("vmdk") => {
            let r = vmdk::VmdkReader::open(path).ok()?;
            inspect_block_read(&r)
        }
        Some("gz") | Some("xz") | Some("bz2") | Some("bzip2") | Some("zst") | Some("zstd") => {
            // Compressed sources need decompression before partition
            // probe — outside the cheap-inspect scope.
            None
        }
        _ => inspect_raw_image(path),
    }
}

/// Inspect any source that already implements `BlockRead`. Use this
/// for container-format virtual disks (qcow2 / vhd / vhdx / vmdk) —
/// the caller passes the already-opened reader so we don't re-parse
/// the header.
pub fn inspect_block_read(dev: &dyn BlockRead) -> Option<PartitionSummary> {
    summarise(dev).ok()
}

fn summarise(dev: &dyn BlockRead) -> partitions::Result<PartitionSummary> {
    let (table, parts) = probe(dev)?;
    let mut out = Vec::with_capacity(parts.len());
    for (i, p) in parts.iter().enumerate() {
        let fs = sniff(dev, p).ok();
        out.push(make_part_info((i + 1) as u32, p, fs));
    }
    let any_bootable = out.iter().any(|p| p.bootable);
    Ok(PartitionSummary {
        table_kind: table_kind_label(table).to_string(),
        partitions: out,
        any_bootable,
    })
}

pub fn table_kind_label(t: TableKind) -> &'static str {
    match t {
        TableKind::Gpt => "GPT",
        TableKind::Mbr => "MBR",
    }
}

/// Build a `PartInfo` from the lower-level `Partition`. Pure — useful
/// in tests that synthesize partitions without filesystem I/O.
pub fn make_part_info(index: u32, p: &Partition, fs: Option<FsKind>) -> PartInfo {
    let (kind_label, type_id) = match p.kind {
        PartitionKind::Gpt { type_guid, .. } => (
            gpt_type_label(&type_guid).to_string(),
            format_guid(&type_guid),
        ),
        PartitionKind::Mbr { type_byte, .. } => (
            mbr_type_label(type_byte).to_string(),
            format!("0x{type_byte:02X}"),
        ),
        PartitionKind::Whole => ("Whole device".to_string(), "—".to_string()),
    };
    PartInfo {
        index,
        start_bytes: p.start,
        length_bytes: p.length,
        size_human: format_size(p.length),
        kind_label,
        type_id,
        label: p.label.clone(),
        uuid: p.uuid.map(|b| format_guid(&b)),
        filesystem: fs.map(format_fs_kind),
        bootable: p.is_bootable(),
    }
}

/// Classify a raw byte buffer as a filesystem and return a display
/// string. Re-exposed so external callers (CLI, future progress
/// reporters) can run the same classification without going through
/// the partition probe.
pub fn classify_buffer(buf: &[u8]) -> Option<String> {
    let kind = classify(buf);
    if matches!(kind, FsKind::Unknown) {
        None
    } else {
        Some(format_fs_kind(kind))
    }
}

pub fn format_fs_kind(k: FsKind) -> String {
    match k {
        FsKind::Ext { version } => match version {
            ExtVersion::Ext2OrAny => "ext2/3".into(),
            ExtVersion::Ext3 => "ext3".into(),
            ExtVersion::Ext4 => "ext4".into(),
        },
        FsKind::Ntfs => "NTFS".into(),
        FsKind::ExFat => "exFAT".into(),
        FsKind::Fat32 => "FAT32".into(),
        FsKind::Fat16 => "FAT16".into(),
        FsKind::HfsPlus => "HFS+".into(),
        FsKind::Apfs => "APFS".into(),
        FsKind::LinuxSwap => "Linux swap".into(),
        FsKind::Iso9660 => "ISO 9660".into(),
        FsKind::Squashfs => "SquashFS".into(),
        FsKind::Unknown => "unknown".into(),
    }
}

/// Human-friendly partition size: KiB / MiB / GiB / TiB power-of-two
/// units (filesystems think in those, not SI).
pub fn format_size(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;
    const GIB: u64 = MIB * 1024;
    const TIB: u64 = GIB * 1024;
    if bytes >= TIB {
        format!("{:.2} TiB", bytes as f64 / TIB as f64)
    } else if bytes >= GIB {
        format!("{:.2} GiB", bytes as f64 / GIB as f64)
    } else if bytes >= MIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.1} KiB", bytes as f64 / KIB as f64)
    } else {
        format!("{bytes} B")
    }
}

fn format_guid(b: &[u8; 16]) -> String {
    format!(
        "{:02X}{:02X}{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
        b[3], b[2], b[1], b[0],
        b[5], b[4],
        b[7], b[6],
        b[8], b[9],
        b[10], b[11], b[12], b[13], b[14], b[15]
    )
}

/// Map a GPT partition-type GUID to a human-readable label. The list
/// covers the GUIDs the user is most likely to encounter when burning
/// distro media — EFI system, Linux filesystem, Linux swap, BIOS boot,
/// Windows basic data, plus the well-known Microsoft reserved /
/// recovery GUIDs that explain "why is my USB stick 17 partitions".
/// Returns "Unknown" rather than the raw GUID; the GUID is also
/// surfaced in `PartInfo::type_id` for diagnostic purposes.
pub fn gpt_type_label(guid: &[u8; 16]) -> &'static str {
    let s = format_guid(guid).to_ascii_uppercase();
    match s.as_str() {
        "C12A7328-F81F-11D2-BA4B-00A0C93EC93B" => "EFI System",
        "21686148-6449-6E6F-744E-656564454649" => "BIOS boot",
        "0FC63DAF-8483-4772-8E79-3D69D8477DE4" => "Linux filesystem",
        "0657FD6D-A4AB-43C4-84E5-0933C84B4F4F" => "Linux swap",
        "44479540-F297-41B2-9AF7-D131D5F0458A" => "Linux x86-64 root",
        "933AC7E1-2EB4-4F13-B844-0E14E2AEF915" => "Linux /home",
        "EBD0A0A2-B9E5-4433-87C0-68B6B72699C7" => "Microsoft basic data",
        "E3C9E316-0B5C-4DB8-817D-F92DF00215AE" => "Microsoft reserved",
        "DE94BBA4-06D1-4D40-A16A-BFD50179D6AC" => "Windows recovery",
        "48465300-0000-11AA-AA11-00306543ECAC" => "Apple HFS+",
        "7C3457EF-0000-11AA-AA11-00306543ECAC" => "APFS container",
        _ => "Unknown",
    }
}

/// Map an MBR type byte to a human-readable label. Same idea as GPT:
/// only the partitions a USB-burn user normally meets.
pub fn mbr_type_label(byte: u8) -> &'static str {
    match byte {
        0x00 => "Empty",
        0x01 => "FAT12",
        0x04 | 0x06 | 0x0E => "FAT16",
        0x05 | 0x0F => "Extended",
        0x07 => "NTFS / exFAT",
        0x0B | 0x0C => "FAT32",
        0x11 => "Hidden FAT12",
        0x82 => "Linux swap",
        0x83 => "Linux filesystem",
        0xA5 | 0xA6 | 0xA9 => "BSD",
        0xAB => "Mac OS X boot",
        0xAF => "HFS+",
        0xEE => "GPT protective",
        0xEF => "EFI System",
        0xFD => "Linux RAID",
        _ => "Unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_size_picks_power_of_two_unit() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(512), "512 B");
        assert_eq!(format_size(1500), "1.5 KiB");
        assert_eq!(format_size(2 * 1024 * 1024), "2.0 MiB");
        assert_eq!(format_size(3 * 1024 * 1024 * 1024), "3.00 GiB");
        assert_eq!(format_size(5 * 1024_u64.pow(4)), "5.00 TiB");
    }

    #[test]
    fn format_guid_renders_microsoft_basic_data() {
        // EBD0A0A2-B9E5-4433-87C0-68B6B72699C7 stored mixed-endian per
        // GPT spec: first 3 fields little-endian, last 2 big-endian.
        let guid: [u8; 16] = [
            0xA2, 0xA0, 0xD0, 0xEB, 0xE5, 0xB9, 0x33, 0x44, 0x87, 0xC0, 0x68, 0xB6, 0xB7, 0x26,
            0x99, 0xC7,
        ];
        assert_eq!(format_guid(&guid), "EBD0A0A2-B9E5-4433-87C0-68B6B72699C7");
    }

    #[test]
    fn gpt_type_label_maps_common_partitions() {
        let efi: [u8; 16] = [
            0x28, 0x73, 0x2A, 0xC1, 0x1F, 0xF8, 0xD2, 0x11, 0xBA, 0x4B, 0x00, 0xA0, 0xC9, 0x3E,
            0xC9, 0x3B,
        ];
        assert_eq!(gpt_type_label(&efi), "EFI System");

        let linux: [u8; 16] = [
            0xAF, 0x3D, 0xC6, 0x0F, 0x83, 0x84, 0x72, 0x47, 0x8E, 0x79, 0x3D, 0x69, 0xD8, 0x47,
            0x7D, 0xE4,
        ];
        assert_eq!(gpt_type_label(&linux), "Linux filesystem");
    }

    #[test]
    fn gpt_type_label_returns_unknown_for_unrecognised_guid() {
        let unknown: [u8; 16] = [0u8; 16];
        assert_eq!(gpt_type_label(&unknown), "Unknown");
    }

    #[test]
    fn mbr_type_label_maps_common_codes() {
        assert_eq!(mbr_type_label(0x00), "Empty");
        assert_eq!(mbr_type_label(0x07), "NTFS / exFAT");
        assert_eq!(mbr_type_label(0x83), "Linux filesystem");
        assert_eq!(mbr_type_label(0xEE), "GPT protective");
        assert_eq!(mbr_type_label(0xEF), "EFI System");
        assert_eq!(mbr_type_label(0x42), "Unknown");
    }

    #[test]
    fn format_fs_kind_renders_each_variant() {
        assert_eq!(
            format_fs_kind(FsKind::Ext {
                version: ExtVersion::Ext4
            }),
            "ext4"
        );
        assert_eq!(format_fs_kind(FsKind::Ntfs), "NTFS");
        assert_eq!(format_fs_kind(FsKind::ExFat), "exFAT");
        assert_eq!(format_fs_kind(FsKind::Fat32), "FAT32");
        assert_eq!(format_fs_kind(FsKind::Fat16), "FAT16");
        assert_eq!(format_fs_kind(FsKind::HfsPlus), "HFS+");
        assert_eq!(format_fs_kind(FsKind::Apfs), "APFS");
        assert_eq!(format_fs_kind(FsKind::LinuxSwap), "Linux swap");
        assert_eq!(format_fs_kind(FsKind::Iso9660), "ISO 9660");
        assert_eq!(format_fs_kind(FsKind::Squashfs), "SquashFS");
        assert_eq!(format_fs_kind(FsKind::Unknown), "unknown");
    }

    #[test]
    fn classify_buffer_returns_none_for_garbage_and_some_for_known() {
        // Garbage = Unknown → None
        assert!(classify_buffer(&[0u8; 64]).is_none());
        // SquashFS magic at offset 0
        let sqsh = b"hsqs\x00\x00\x00\x00";
        assert_eq!(classify_buffer(sqsh).as_deref(), Some("SquashFS"));
    }

    #[test]
    fn make_part_info_populates_gpt_partition() {
        let guid: [u8; 16] = [
            0x28, 0x73, 0x2A, 0xC1, 0x1F, 0xF8, 0xD2, 0x11, 0xBA, 0x4B, 0x00, 0xA0, 0xC9, 0x3E,
            0xC9, 0x3B,
        ];
        let p = Partition {
            start: 1_048_576,
            length: 100 * 1024 * 1024,
            kind: PartitionKind::Gpt {
                type_guid: guid,
                attributes: 0,
            },
            label: Some("EFI".into()),
            uuid: Some(guid),
        };
        let info = make_part_info(1, &p, Some(FsKind::Fat32));
        assert_eq!(info.index, 1);
        assert_eq!(info.start_bytes, 1_048_576);
        assert_eq!(info.length_bytes, 100 * 1024 * 1024);
        assert_eq!(info.size_human, "100.0 MiB");
        assert_eq!(info.kind_label, "EFI System");
        assert_eq!(info.type_id, "C12A7328-F81F-11D2-BA4B-00A0C93EC93B");
        assert_eq!(info.label.as_deref(), Some("EFI"));
        assert_eq!(info.filesystem.as_deref(), Some("FAT32"));
        // ESP-typed GPT entry is bootable even with zero attributes.
        assert!(info.bootable);
    }

    #[test]
    fn make_part_info_populates_mbr_partition() {
        let p = Partition {
            start: 1024 * 512,
            length: 10 * 1024 * 1024,
            kind: PartitionKind::Mbr {
                type_byte: 0x83,
                active: false,
            },
            label: None,
            uuid: None,
        };
        let info = make_part_info(2, &p, None);
        assert_eq!(info.kind_label, "Linux filesystem");
        assert_eq!(info.type_id, "0x83");
        assert!(info.filesystem.is_none());
        assert!(info.label.is_none());
        // Non-active MBR partition is not bootable.
        assert!(!info.bootable);
    }

    #[test]
    fn make_part_info_renders_whole_device_kind() {
        let p = Partition {
            start: 0,
            length: 1024,
            kind: PartitionKind::Whole,
            label: None,
            uuid: None,
        };
        let info = make_part_info(0, &p, None);
        assert_eq!(info.kind_label, "Whole device");
        assert_eq!(info.type_id, "—");
        assert!(!info.bootable);
    }

    #[test]
    fn make_part_info_marks_active_mbr_bootable() {
        let p = Partition {
            start: 1024 * 512,
            length: 1024 * 512,
            kind: PartitionKind::Mbr {
                type_byte: 0x83,
                active: true,
            },
            label: None,
            uuid: None,
        };
        let info = make_part_info(1, &p, None);
        assert!(info.bootable);
    }

    #[test]
    fn make_part_info_marks_legacy_bios_bootable_gpt() {
        // Linux filesystem type GUID + the legacy-BIOS-bootable bit
        // (1<<2) → bootable.
        let guid: [u8; 16] = [
            0xAF, 0x3D, 0xC6, 0x0F, 0x83, 0x84, 0x72, 0x47, 0x8E, 0x79, 0x3D, 0x69, 0xD8, 0x47,
            0x7D, 0xE4,
        ];
        let p = Partition {
            start: 1 << 20,
            length: 100 << 20,
            kind: PartitionKind::Gpt {
                type_guid: guid,
                attributes: 1 << 2,
            },
            label: None,
            uuid: Some(guid),
        };
        let info = make_part_info(1, &p, None);
        assert!(info.bootable);
    }

    #[test]
    fn inspect_raw_image_returns_none_for_image_without_partition_table() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("empty.img");
        std::fs::write(&p, vec![0u8; 4096]).unwrap();
        assert!(inspect_raw_image(&p).is_none());
    }

    #[test]
    fn inspect_raw_image_handles_missing_file_gracefully() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("ghost.img");
        assert!(inspect_raw_image(&p).is_none());
    }

    /// Construct a minimal valid MBR with one Linux filesystem partition
    /// and inspect it end-to-end. Exercises the file-backed
    /// `BlockRead` path without needing external tools.
    #[test]
    fn inspect_any_routes_to_raw_for_iso_extension() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("e.iso");
        std::fs::write(&p, vec![0u8; 4096]).unwrap();
        // No partition table → None.
        assert!(inspect_any(&p).is_none());
    }

    #[test]
    fn inspect_any_returns_none_for_compressed_extensions() {
        let dir = tempfile::tempdir().unwrap();
        for ext in &["gz", "xz", "bz2", "zst"] {
            let p = dir.path().join(format!("img.iso.{ext}"));
            std::fs::write(&p, b"would need decompression").unwrap();
            assert!(inspect_any(&p).is_none(), "ext {ext} should be skipped");
        }
    }

    #[test]
    fn inspect_raw_image_reads_synthetic_mbr() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("mbr.img");
        // Build a 4 MiB image: zeros + MBR at end of LBA0.
        let mut image = vec![0u8; 4 * 1024 * 1024];
        // Partition 1: type 0x83 (Linux), start LBA = 2048, length = 4096.
        let entry_offset = 0x1BE;
        image[entry_offset] = 0x00; // bootable flag
        image[entry_offset + 1..entry_offset + 4].copy_from_slice(&[0, 0, 0]); // CHS start
        image[entry_offset + 4] = 0x83; // type
        image[entry_offset + 5..entry_offset + 8].copy_from_slice(&[0, 0, 0]); // CHS end
        image[entry_offset + 8..entry_offset + 12].copy_from_slice(&2048u32.to_le_bytes());
        image[entry_offset + 12..entry_offset + 16].copy_from_slice(&4096u32.to_le_bytes());
        image[510] = 0x55;
        image[511] = 0xAA;
        std::fs::write(&p, &image).unwrap();
        let summary = inspect_raw_image(&p).expect("probe should succeed");
        assert_eq!(summary.table_kind, "MBR");
        assert_eq!(summary.partitions.len(), 1);
        let part = &summary.partitions[0];
        assert_eq!(part.kind_label, "Linux filesystem");
        assert_eq!(part.start_bytes, 2048 * 512);
        assert_eq!(part.length_bytes, 4096 * 512);
        assert_eq!(part.type_id, "0x83");
    }
}
