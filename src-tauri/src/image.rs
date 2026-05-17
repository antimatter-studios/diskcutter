//! `DiskImage` — the single object that knows about disk image *files*.
//!
//! Centralises every "what kind of file is this and how do I read it"
//! question that used to live duplicated in `inspect.rs`, `validate.rs`,
//! and a handful of one-off Tauri command bodies. Callers ask questions
//! by name (`partitions()`, `validate()`, `is_bootable()`,
//! `boot_sources()`) and don't care which container format the file
//! happens to be.
//!
//! Internally there are two backends:
//!
//! - **Block** — file directly mountable as a `BlockRead`. Covers raw
//!   / ISO / qcow2 / vhd / vhdx / vmdk. All probe-level questions
//!   work: partitions, bootability, validation.
//!
//! - **Compressed** — `.gz` / `.xz` / `.bz2` / `.zst`. We can't seek
//!   without decompression, so we only keep a decompressed prefix big
//!   enough for filesystem-magic + ISO 9660 signatures. Partition and
//!   boot probes degrade to "unknown" rather than re-decompressing the
//!   whole file inline.
//!
//! Each per-question result is cached behind a `OnceCell` so a single
//! `DiskImage` answers multiple commands (validate, then partitions,
//! then bootability) without re-running the underlying probes.

use std::cell::OnceCell;
use std::path::{Path, PathBuf};

use fs_core::{BlockRead, FileDevice};
use partitions::probe::{probe, PartitionKind};
use serde::Serialize;

use crate::decoder_chain::DiskReader;
use crate::inspect::{make_part_info, table_kind_label, PartitionSummary};
use crate::validate::{ValidationReport, SNIFF_WINDOW_BYTES};

/// The container-format family of an image file. Used internally to
/// pick the right reader and to surface a stable string for the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFormat {
    Raw,
    Qcow2,
    Vhd,
    Vhdx,
    Vmdk,
    CompressedGz,
    CompressedXz,
    CompressedBz2,
    CompressedZst,
}

impl ImageFormat {
    pub fn label(self) -> &'static str {
        match self {
            ImageFormat::Raw => "raw",
            ImageFormat::Qcow2 => "qcow2",
            ImageFormat::Vhd => "vhd",
            ImageFormat::Vhdx => "vhdx",
            ImageFormat::Vmdk => "vmdk",
            ImageFormat::CompressedGz => "gzip",
            ImageFormat::CompressedXz => "xz",
            ImageFormat::CompressedBz2 => "bzip2",
            ImageFormat::CompressedZst => "zstd",
        }
    }
    pub fn is_compressed(self) -> bool {
        matches!(
            self,
            ImageFormat::CompressedGz
                | ImageFormat::CompressedXz
                | ImageFormat::CompressedBz2
                | ImageFormat::CompressedZst
        )
    }
}

/// One specific reason this image is bootable. A real-world hybrid
/// distro ISO will commonly report several of these at once (e.g.
/// `[ElTorito, MbrActive(1), GptEsp(2)]`); a non-bootable data image
/// reports an empty list.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BootSource {
    /// MBR entry with the 0x80 status-byte active flag set. The legacy
    /// BIOS firmware boots from this partition.
    MbrActive { index: u32 },
    /// GPT entry typed as the EFI System Partition.
    GptEsp { index: u32 },
    /// GPT entry with the `LEGACY_BIOS_BOOTABLE` attribute (bit 2) set.
    GptLegacyBios { index: u32 },
    /// LBA 0 has the 0x55AA signature *and* non-trivial bootloader code
    /// in bytes 0..446. Many `dd`-ready raw images live here — they
    /// boot via the MBR even when no partition entry carries the active
    /// flag.
    MbrBootloader,
    /// ISO 9660 boot record volume descriptor at sector 17 (offset
    /// 0x8800). Standard mechanism for bootable optical media + USB
    /// images mastered from `mkisofs`/`genisoimage`/`xorriso`.
    ElTorito,
}

/// Errors `DiskImage::open` can surface. Keep them coarse — the UI
/// either gets a `DiskImage` or it falls back to "invalid image" with
/// the error's `to_string()`.
#[derive(Debug)]
#[allow(dead_code)]
pub enum ImageError {
    Io(std::io::Error),
    UnsupportedFormat(String),
}

impl std::fmt::Display for ImageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ImageError::Io(e) => write!(f, "i/o error: {e}"),
            ImageError::UnsupportedFormat(ext) => write!(f, "unsupported image format: .{ext}"),
        }
    }
}

impl std::error::Error for ImageError {}

impl From<std::io::Error> for ImageError {
    fn from(e: std::io::Error) -> Self {
        ImageError::Io(e)
    }
}

enum Backend {
    /// Block-level reader. Probes work directly.
    Block(Box<dyn BlockRead>),
    /// Compressed source — we hold the decompressed prefix only.
    /// Sized just enough for filesystem-magic + ISO 9660 signatures so
    /// `validate()` can still classify the contents without re-running
    /// the full decompression pipeline.
    Compressed { prefix: Vec<u8> },
}

pub struct DiskImage {
    path: PathBuf,
    format: ImageFormat,
    backend: Backend,
    partitions: OnceCell<Option<PartitionSummary>>,
    boot_sources: OnceCell<Vec<BootSource>>,
}

impl DiskImage {
    /// Open an image file. Format is sniffed from the extension —
    /// matches what the file-picker already restricts the user to. The
    /// reader is opened (or, for compressed sources, the prefix is
    /// decompressed) inside this call so subsequent question-asking is
    /// constant-time.
    ///
    /// No per-item logging — see `open_with_log` for the scan path.
    pub fn open(path: &Path) -> Result<Self, ImageError> {
        Self::open_with_log(path, &crate::joblog::NullLogger)
    }

    /// Same as `open`, but emits debug entries into the per-item log via
    /// `log`. Use this from scan-time commands (validate / inspect
    /// partitions / inspect bootable) so the row's log captures format
    /// identification, container metadata, and (for compressed inputs)
    /// the decoder chain layers.
    pub fn open_with_log(
        path: &Path,
        log: &dyn crate::joblog::JobLogger,
    ) -> Result<Self, ImageError> {
        log.debug(&format!("scan: opening image {}", path.display()));
        if let Ok(meta) = std::fs::metadata(path) {
            log.debug(&format!("scan: file size = {} bytes", meta.len()));
        }
        let ext = path
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s.to_ascii_lowercase());
        log.debug(&format!(
            "scan: extension = {}",
            ext.as_deref().unwrap_or("(none)")
        ));
        let (format, backend) = match ext.as_deref() {
            Some("qcow2") | Some("qcow") => {
                let r = qcow2::Qcow2Reader::open(path).map_err(std::io::Error::other)?;
                log.debug(&format!(
                    "scan: qcow2 v{} cluster={} bytes virtual_size={} bytes",
                    r.version(),
                    r.cluster_size(),
                    r.virtual_size()
                ));
                (ImageFormat::Qcow2, Backend::Block(Box::new(r)))
            }
            Some("vhd") => {
                let r = vhd::VhdReader::open(path).map_err(std::io::Error::other)?;
                log.debug(&format!(
                    "scan: vhd virtual_size={} bytes",
                    r.virtual_size()
                ));
                (ImageFormat::Vhd, Backend::Block(Box::new(r)))
            }
            Some("vhdx") => {
                let r = vhdx::VhdxReader::open(path).map_err(std::io::Error::other)?;
                log.debug(&format!(
                    "scan: vhdx virtual_size={} bytes",
                    r.virtual_size()
                ));
                (ImageFormat::Vhdx, Backend::Block(Box::new(r)))
            }
            Some("vmdk") => {
                let r = vmdk::VmdkReader::open(path).map_err(std::io::Error::other)?;
                log.debug(&format!(
                    "scan: vmdk virtual_size={} bytes",
                    r.virtual_size()
                ));
                (ImageFormat::Vmdk, Backend::Block(Box::new(r)))
            }
            Some("gz") => (ImageFormat::CompressedGz, decompressed_prefix(path, log)?),
            Some("xz") => (ImageFormat::CompressedXz, decompressed_prefix(path, log)?),
            Some("bz2") | Some("bzip2") => {
                (ImageFormat::CompressedBz2, decompressed_prefix(path, log)?)
            }
            Some("zst") | Some("zstd") => {
                (ImageFormat::CompressedZst, decompressed_prefix(path, log)?)
            }
            _ => {
                let r = FileDevice::open(path).map_err(std::io::Error::other)?;
                log.debug(&format!(
                    "scan: raw image size_bytes={} ({} sectors @ 512B)",
                    r.size_bytes(),
                    r.size_bytes() / 512
                ));
                (ImageFormat::Raw, Backend::Block(Box::new(r)))
            }
        };
        log.info(&format!("scan: format identified as {:?}", format));
        Ok(Self {
            path: path.to_path_buf(),
            format,
            backend,
            partitions: OnceCell::new(),
            boot_sources: OnceCell::new(),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn format(&self) -> ImageFormat {
        self.format
    }
    pub fn size_bytes(&self) -> u64 {
        match &self.backend {
            Backend::Block(dev) => dev.size_bytes(),
            // For a compressed image we only know how much of the
            // prefix decoded; the *full* decompressed size is not known
            // without finishing the decode. Callers that need the true
            // size should look at the image_inspect command's
            // `uncompressed_bytes`, which the readers report.
            Backend::Compressed { prefix } => prefix.len() as u64,
        }
    }

    /// Partition + filesystem summary. `None` for images with no recognised
    /// table. For compressed sources the probe runs over the slurped prefix
    /// via `PrefixBlockView` — partitions whose start LBA is inside the
    /// prefix get full filesystem detail; entries that point past the
    /// prefix still appear in the table, just with `filesystem: None`.
    pub fn partitions(&self) -> Option<&PartitionSummary> {
        self.partitions
            .get_or_init(|| match &self.backend {
                Backend::Block(dev) => summarise(dev.as_ref()).ok(),
                Backend::Compressed { prefix } => {
                    let view =
                        crate::decoder_chain::block_view::PrefixBlockView::new(prefix.clone());
                    summarise(&view).ok()
                }
            })
            .as_ref()
    }

    /// Every boot signal this image presents. Empty = not bootable by
    /// any of the mechanisms we recognise. See [`BootSource`] for the
    /// individual cases. For compressed sources the probe runs over the
    /// slurped prefix — most boot signals (MBR boot code, GPT EFI System
    /// Partition, El Torito at 0x8800) sit inside the default prefix size.
    pub fn boot_sources(&self) -> &[BootSource] {
        self.boot_sources.get_or_init(|| match &self.backend {
            Backend::Block(dev) => compute_boot_sources(dev.as_ref(), self.partitions()),
            Backend::Compressed { prefix } => {
                let view = crate::decoder_chain::block_view::PrefixBlockView::new(prefix.clone());
                compute_boot_sources(&view, self.partitions())
            }
        })
    }

    pub fn is_bootable(&self) -> bool {
        !self.boot_sources().is_empty()
    }

    /// "Does this file actually contain a burnable disk?" — same gate
    /// `validate_image_contents` used to compute inline. Folds the
    /// partition probe and filesystem sniff into a single verdict; see
    /// `crate::validate` for the exact rules.
    pub fn validate(&self) -> ValidationReport {
        match &self.backend {
            Backend::Block(dev) => crate::validate::validate_block(dev.as_ref()),
            Backend::Compressed { prefix } => {
                // Run the same partition-table-first / filesystem-sniff
                // logic as the Block path, just over the decompressed
                // prefix. Falls back to the softer "not a recognised
                // disk image" message when nothing matches so the UI
                // doesn't surface the partition-probe's internal hints
                // (which the user can't action on a compressed source).
                let view = crate::decoder_chain::block_view::PrefixBlockView::new(prefix.clone());
                match crate::validate::validate_block(&view) {
                    v @ ValidationReport::Valid { .. } => v,
                    ValidationReport::Invalid { .. } => ValidationReport::Invalid {
                        reason: "compressed contents are not a recognised disk image".into(),
                    },
                }
            }
        }
    }
}

/// Read enough of a compressed source to classify the decompressed
/// prefix. Mirrors what `validate::validate_compressed` used to do —
/// the byte count is `SNIFF_WINDOW_BYTES_INSPECT` (0x8200) so all
/// FAT/NTFS/exFAT/ext/HFS+/ISO 9660 signatures are reachable.
fn decompressed_prefix(
    path: &Path,
    log: &dyn crate::joblog::JobLogger,
) -> Result<Backend, ImageError> {
    let mut reader = DiskReader::open_with_log(path, log)
        .map_err(|e| ImageError::Io(std::io::Error::other(e)))?;
    log.debug(&format!(
        "scan: decompressing first {} bytes of prefix for fs sniff",
        SNIFF_WINDOW_BYTES
    ));
    let mut buf = vec![0u8; SNIFF_WINDOW_BYTES];
    let mut filled = 0usize;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) => return Err(ImageError::Io(e)),
        }
    }
    buf.truncate(filled);
    log.debug(&format!("scan: decompressed prefix = {} bytes", buf.len()));
    Ok(Backend::Compressed { prefix: buf })
}

#[allow(dead_code)]
fn extension_string(path: &Path) -> String {
    path.extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_else(|| "(none)".into())
}

fn summarise(dev: &dyn BlockRead) -> partitions::Result<PartitionSummary> {
    let (table, parts) = probe(dev)?;
    let mut out = Vec::with_capacity(parts.len());
    for (i, p) in parts.iter().enumerate() {
        let fs = partitions::sniff::sniff(dev, p).ok();
        let mut info = make_part_info((i + 1) as u32, p, fs);
        info.fs_label = fs.and_then(|kind| read_fs_label(dev, p, kind));
        out.push(info);
    }
    let any_bootable = out.iter().any(|p| p.bootable);
    Ok(PartitionSummary {
        table_kind: table_kind_label(table).to_string(),
        partitions: out,
        any_bootable,
    })
}

/// Read up to 64 KB from the partition start and let the inspect
/// parser pull out a volume label. Errors / short reads silently
/// produce `None` — labels are decorative.
fn read_fs_label(
    dev: &dyn BlockRead,
    p: &partitions::probe::Partition,
    kind: partitions::sniff::FsKind,
) -> Option<String> {
    let take = (64 * 1024u64).min(p.length) as usize;
    if take == 0 {
        return None;
    }
    let mut buf = vec![0u8; take];
    dev.read_at(p.start, &mut buf).ok()?;
    crate::inspect::fs_label_from_sample(kind, &buf)
}

/// Walk every boot signal the image carries.
///
/// Order matters only for surfacing — `is_bootable()` collapses to the
/// boolean. We list partition-level signals first (more specific) and
/// then fall back to disk-level (less specific). El Torito is checked
/// separately because it lives outside the partition-table world.
pub(crate) fn compute_boot_sources(
    dev: &dyn BlockRead,
    parts: Option<&PartitionSummary>,
) -> Vec<BootSource> {
    let mut out = Vec::new();

    // Per-partition signals. The active/attribute/type-GUID values
    // already round-tripped through `am-partitions` and we just
    // re-derive the labelled BootSource enum here so the UI doesn't
    // have to.
    if let Ok((_table, raw_parts)) = probe(dev) {
        for (i, p) in raw_parts.iter().enumerate() {
            let idx = (i + 1) as u32;
            match p.kind {
                PartitionKind::Mbr { active, .. } if active => {
                    out.push(BootSource::MbrActive { index: idx });
                }
                PartitionKind::Gpt {
                    type_guid,
                    attributes,
                } => {
                    if type_guid == partitions::gpt::type_guids::EFI_SYSTEM {
                        out.push(BootSource::GptEsp { index: idx });
                    }
                    if attributes & partitions::gpt::attr::LEGACY_BIOS_BOOTABLE != 0 {
                        out.push(BootSource::GptLegacyBios { index: idx });
                    }
                }
                _ => {}
            }
        }
    }
    // Silence "unused" warning when `parts` is None; we still want the
    // arg for future bootability heuristics that read filesystem data.
    let _ = parts;

    if has_mbr_bootloader(dev) {
        out.push(BootSource::MbrBootloader);
    }
    if has_el_torito(dev) {
        out.push(BootSource::ElTorito);
    }
    out
}

/// True when LBA 0 has the 0x55AA signature *and* non-trivial code
/// (some byte other than 0x00 / 0xFF) in the first 446 bytes. The all-
/// zeros / all-FFs check is what stops a clean protective-MBR (GPT
/// disks with no bootloader code) from spuriously firing this signal —
/// only hybrid layouts that actually carry bootloader code report it.
fn has_mbr_bootloader(dev: &dyn BlockRead) -> bool {
    let mut lba0 = [0u8; 512];
    if dev.read_at(0, &mut lba0).is_err() {
        return false;
    }
    if !(lba0[510] == 0x55 && lba0[511] == 0xAA) {
        return false;
    }
    let code = &lba0[..446];
    let all_zero = code.iter().all(|&b| b == 0x00);
    let all_ff = code.iter().all(|&b| b == 0xFF);
    !(all_zero || all_ff)
}

/// ISO 9660 boot record volume descriptor at sector 17 (offset
/// 0x8800). Spec layout:
///
/// ```text
///   0       0x00            (boot record indicator)
///   1..6    "CD001"         (standard identifier)
///   6       0x01            (volume descriptor version)
///   7..39   "EL TORITO SPECIFICATION" + space padding
///   ...
/// ```
pub const EL_TORITO_OFFSET: u64 = 0x8800;
fn has_el_torito(dev: &dyn BlockRead) -> bool {
    if dev.size_bytes() < EL_TORITO_OFFSET + 39 {
        return false;
    }
    let mut buf = [0u8; 39];
    if dev.read_at(EL_TORITO_OFFSET, &mut buf).is_err() {
        return false;
    }
    if buf[0] != 0x00 {
        return false;
    }
    if &buf[1..6] != b"CD001" {
        return false;
    }
    if buf[6] != 0x01 {
        return false;
    }
    &buf[7..7 + 23] == b"EL TORITO SPECIFICATION"
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write_mbr_active(path: &Path) {
        let mut image = vec![0u8; 4 * 1024 * 1024];
        let entry = 0x1BE;
        image[entry] = 0x80; // active flag
        image[entry + 4] = 0x83; // Linux type
        image[entry + 8..entry + 12].copy_from_slice(&2048u32.to_le_bytes());
        image[entry + 12..entry + 16].copy_from_slice(&4096u32.to_le_bytes());
        image[510] = 0x55;
        image[511] = 0xAA;
        std::fs::write(path, &image).unwrap();
    }

    fn write_mbr_inactive_with_bootloader(path: &Path) {
        let mut image = vec![0u8; 4 * 1024 * 1024];
        // Plant some non-zero bootloader code in the first 446 bytes.
        for (i, b) in image[..446].iter_mut().enumerate() {
            *b = ((i as u32 & 0xFF) | 0x10) as u8;
        }
        let entry = 0x1BE;
        // partition entry — type set, no active flag
        image[entry] = 0x00;
        image[entry + 4] = 0x83;
        image[entry + 8..entry + 12].copy_from_slice(&2048u32.to_le_bytes());
        image[entry + 12..entry + 16].copy_from_slice(&4096u32.to_le_bytes());
        image[510] = 0x55;
        image[511] = 0xAA;
        std::fs::write(path, &image).unwrap();
    }

    fn write_iso_with_el_torito(path: &Path) {
        // Just past EL_TORITO_OFFSET + 39 bytes is enough for the
        // boot-record detector. Pad to that size and stamp the
        // signature; everything else can stay zero.
        let mut image = vec![0u8; (EL_TORITO_OFFSET as usize) + 64];
        let off = EL_TORITO_OFFSET as usize;
        image[off] = 0x00;
        image[off + 1..off + 6].copy_from_slice(b"CD001");
        image[off + 6] = 0x01;
        image[off + 7..off + 7 + 23].copy_from_slice(b"EL TORITO SPECIFICATION");
        std::fs::write(path, &image).unwrap();
    }

    #[test]
    fn open_raw_image_reports_raw_format() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("plain.img");
        std::fs::write(&p, vec![0u8; 4096]).unwrap();
        let img = DiskImage::open(&p).unwrap();
        assert_eq!(img.format(), ImageFormat::Raw);
        assert!(!img.is_bootable());
        assert!(img.boot_sources().is_empty());
    }

    #[test]
    fn active_mbr_partition_reports_mbr_active_boot_source() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("bootable.img");
        write_mbr_active(&p);
        let img = DiskImage::open(&p).unwrap();
        assert!(img.is_bootable());
        let sources = img.boot_sources();
        assert!(
            sources
                .iter()
                .any(|s| matches!(s, BootSource::MbrActive { index: 1 })),
            "got {sources:?}"
        );
    }

    #[test]
    fn mbr_with_bootloader_code_but_no_active_partition_is_still_bootable() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("bootloader.img");
        write_mbr_inactive_with_bootloader(&p);
        let img = DiskImage::open(&p).unwrap();
        assert!(img.is_bootable());
        assert!(img.boot_sources().contains(&BootSource::MbrBootloader));
        // And the partition is NOT marked active.
        assert!(!img
            .boot_sources()
            .iter()
            .any(|s| matches!(s, BootSource::MbrActive { .. })));
    }

    #[test]
    fn iso_with_el_torito_is_bootable() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("disc.iso");
        write_iso_with_el_torito(&p);
        let img = DiskImage::open(&p).unwrap();
        assert!(img.is_bootable());
        assert!(img.boot_sources().contains(&BootSource::ElTorito));
    }

    #[test]
    fn opening_a_compressed_image_classifies_via_prefix() {
        // gzip a small FAT-ish blob so validate() lands on Valid.
        use flate2::write::GzEncoder;
        use flate2::Compression;
        use std::io::Write;
        let dir = tempdir().unwrap();
        let p = dir.path().join("blob.img.gz");
        let mut fat = vec![0u8; 0x8200];
        fat[0x52..0x5A].copy_from_slice(b"FAT32   ");
        fat[510] = 0x55;
        fat[511] = 0xAA;
        let mut e = GzEncoder::new(Vec::new(), Compression::default());
        e.write_all(&fat).unwrap();
        std::fs::write(&p, e.finish().unwrap()).unwrap();
        let img = DiskImage::open(&p).unwrap();
        assert_eq!(img.format(), ImageFormat::CompressedGz);
        assert!(matches!(img.validate(), ValidationReport::Valid { .. }));
        // The FAT32 OEM string at offset 0x52 lives inside the MBR boot
        // code region, so has_mbr_bootloader fires — that's the correct
        // probe result for a real FAT32 boot sector, surfaced through
        // the prefix view.
        assert!(img.boot_sources().contains(&BootSource::MbrBootloader));
    }

    /// Build a minimal MBR image with one Linux partition, compress it
    /// four different ways, and verify each `DiskImage` surfaces the
    /// partition table through the per-format decoder chain → prefix
    /// view → partition probe pipeline.
    ///
    /// Regression coverage for the bug where `Backend::Compressed`
    /// short-circuited `partitions()` / `boot_sources()` to `None` /
    /// empty regardless of what the prefix actually contained.
    #[test]
    fn compressed_formats_surface_partition_table_through_prefix_view() {
        use std::io::Write;
        let dir = tempdir().unwrap();

        let mut image = vec![0u8; 32 * 1024];
        let entry = 0x1BE;
        image[entry] = 0x80; // active
        image[entry + 4] = 0x83; // Linux
        image[entry + 8..entry + 12].copy_from_slice(&2048u32.to_le_bytes());
        image[entry + 12..entry + 16].copy_from_slice(&8u32.to_le_bytes());
        image[510] = 0x55;
        image[511] = 0xAA;

        let gz_bytes = {
            use flate2::write::GzEncoder;
            use flate2::Compression;
            let mut e = GzEncoder::new(Vec::new(), Compression::default());
            e.write_all(&image).unwrap();
            e.finish().unwrap()
        };
        let xz_bytes = {
            use xz2::write::XzEncoder;
            let mut e = XzEncoder::new(Vec::new(), 1);
            e.write_all(&image).unwrap();
            e.finish().unwrap()
        };
        let bz2_bytes = {
            use bzip2::write::BzEncoder;
            let mut e = BzEncoder::new(Vec::new(), bzip2::Compression::default());
            e.write_all(&image).unwrap();
            e.finish().unwrap()
        };
        let zst_bytes = zstd::encode_all(&image[..], 0).unwrap();

        let cases: [(&str, &[u8], ImageFormat); 4] = [
            ("disk.img.gz", &gz_bytes, ImageFormat::CompressedGz),
            ("disk.img.xz", &xz_bytes, ImageFormat::CompressedXz),
            ("disk.img.bz2", &bz2_bytes, ImageFormat::CompressedBz2),
            ("disk.img.zst", &zst_bytes, ImageFormat::CompressedZst),
        ];

        for (name, body, expected_format) in cases {
            let p = dir.path().join(name);
            std::fs::write(&p, body).unwrap();
            let img = DiskImage::open(&p).unwrap_or_else(|e| panic!("open {name}: {e}"));
            assert_eq!(img.format(), expected_format, "format for {name}");

            let parts = img
                .partitions()
                .unwrap_or_else(|| panic!("{name}: partitions() returned None"));
            assert_eq!(parts.table_kind, "MBR", "{name}: table kind");
            assert_eq!(parts.partitions.len(), 1, "{name}: partition count");
            assert_eq!(
                parts.partitions[0].kind_label, "Linux filesystem",
                "{name}: partition type"
            );

            assert!(
                matches!(img.validate(), ValidationReport::Valid { .. }),
                "{name}: validate should report Valid"
            );

            // The partition is flagged active in the MBR, so the boot
            // probe should surface `MbrActive { index: 1 }`.
            let sources = img.boot_sources();
            assert!(
                sources
                    .iter()
                    .any(|s| matches!(s, BootSource::MbrActive { index: 1 })),
                "{name}: expected MbrActive boot source, got {sources:?}"
            );
        }
    }
}
