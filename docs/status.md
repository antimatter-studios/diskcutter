# Disk Cutter — Status

Snapshot of where the app is and what's coming. Sibling docs:
- [TODO.md](TODO.md) — feature backlog with implementation scope
- [rust-img-qcow2-improvements.md](rust-img-qcow2-improvements.md) — upstream crate notes
- [db-cli.md](db-cli.md) — SQLite inspector tooling
- [macos-pipeline.md](macos-pipeline.md) — macOS burn pipeline notes

## Current state (2026-05-12)

| Area | State |
| --- | --- |
| Frontend | React + Vite, i18n with en/de/es (155 keys each), theme palette via CSS vars |
| Tauri backend | Pipeline, readers, writers, SQLite migrations, disk enumeration scaffolding |
| Reader registry | RAW/ISO only ([readers/mod.rs](../src-tauri/src/readers/mod.rs)). qcow2 + compressed formats not yet wired. |
| Writer | `PlainFileDeviceIo` writes to a temp file; **no real `/dev/diskN` access yet**. |
| DB | SQLite with `0001_initial.sql` migration. `burn_jobs` / `burn_mismatches` schema landed but not all wired to UI. |
| Disk enumeration | `disks.rs` scaffolded; live `diskutil`/`/sys/block`/SetupDi probes still in backlog. |

## Active workstream

### qcow2 integration

Goal: add a qcow2 reader factory next to `RawReaderFactory` so `.qcow2` files burn the same way `.iso` does.

Blocking on upstream crate work. Status of each piece:

| Piece | Where | Status |
| --- | --- | --- |
| `BlockReadStreamer` (the `Read` adapter we need) | `am-fs-core` working tree | code-complete, uncommitted |
| `Qcow2Reader::reader()` convenience | `am-img-qcow2` | not started |
| Device-only open (`open_on_device`) | `am-img-qcow2` v0.2.0 | done on HEAD, unpublished |
| `am-fs-core` v0.2.0 → crates.io | — | needs release |
| `am-img-qcow2` v0.3.0 → crates.io | — | needs release |
| Publish CI workflow | upstream repos | not started |
| `Qcow2ReaderFactory` in Disk Cutter | this repo | not started, blocks on releases |

Full upstream detail in [rust-img-qcow2-improvements.md](rust-img-qcow2-improvements.md).

## Future features

### Backlog (carried from [TODO.md](TODO.md))

- Real raw-device I/O (`/dev/diskN` with `O_DIRECT | O_SYNC`, elevation)
- Real disk enumeration (`diskutil -plist`, `/sys/block`, SetupDi)
- SQLite history surfaced in Sidebar (DONE/FAILED counts persist across sessions)
- Streaming image hash during inspect (async, progress events)
- Compressed image formats (gzip, xz, bzip2, zstd)
- Theme switching (dark theme via `:root[data-theme=...]` vars)

### UI features moved here from qcow2 wishlist

These don't need crate-side changes — Disk Cutter can build them against the existing qcow2 API.

#### Progress UI during image streaming

Count bytes returned from `BlockReadStreamer::read()` in the consumer (hasher / writer / progress thread). Per-byte granularity is fine; no `on_cluster_decoded` hook needed from the crate. Same pattern works for plain ISO and qcow2.

#### Pre-burn image inspection panel

`Qcow2Reader::open()` only parses the header + L1 table (cheap), so a "preview before confirm" UI can:
- show `virtual_size()`, `cluster_size()`, `version()`, `has_backing()`
- detect encryption via `reader.header().crypt_method` (0=none, 1=AES, 2=LUKS — already raw-exposed)
- drop the reader if the user backs out

No "summary without full open" crate primitive needed.

#### Encrypted-image refusal UX

Today an encrypted image would fail with a generic `Error::Unsupported`. Disk Cutter can read `header().crypt_method` itself and present "this image is encrypted; we can't burn it" before attempting the burn. Typed `is_encrypted()` / `EncryptionKind` accessors on `Header` would be cosmetic; not required.

### Considered, not pursued

- **Async / tokio support in the qcow2 crate** — Disk Cutter runs reads on `std::thread::spawn`. No tokio surface needed.

### Image-scan caching — phased plan

We currently slurp a 4 MiB prefix at row-expand time and run the partition / boot
probes over it. Per-partition filesystem labels for compressed sources need bytes
past the prefix, and a future "extract one partition to a target's empty region"
feature needs the full partition bytes. Work is phased so each step solves a real
problem without over-building.

**Phase 1 (in progress)** — `image_scans` table + eager-on-add single-pass scan
+ progressive UI events. Triggered when the user adds an image (or clicks
REFRESH); not lazy on row-expand — the disk cost is small (partition samples
only, ~hundreds of KB) and we'd rather have the data ready by the time the
user picks a target. One sequential decompression per `(image_path, size,
mtime)`, stopping at `last_partition_start + 64 KB`. Outputs cached: format
chain, true uncompressed size (xz only — see trade-off note below),
partition table, per-partition filesystem labels, boot sources. Cached row
shared across every burn_job pointing at the same image path, so adding the
same image to 10 burn_jobs scans once.

**Phase 1 trade-offs to revisit:**
- Source SHA-256 is *not* computed (would require draining the full stream).
  The user-facing "SHA-256 (SOURCE)" KV was removed from the queue UI, so this
  has no immediate cost; revisit if the field returns or if the burn pipeline
  ever wants to skip its own source-hash pass.
- True uncompressed size is *only* recoverable for xz (footer index parsed
  via `xz_footer::read_total_uncompressed`). gzip / bzip2 / zstd report the
  compressed file size as an over-conservative placeholder — fine for the
  progress bar's "we've at least decoded this much" denominator, less fine
  for an exact value to display. Revisit if the UI starts depending on
  exact decompressed bytes for a non-xz compressed source.

**Phase 2 (deferred)** — temp-file materialization for compressed multi-copy
queues only. Decisions locked in:

  1. **Trigger**: only when the queue entry has `copies > 1`. Single-copy
     burns stream-decompress as today.
  2. **Timing**: temp file is materialized when the burn process *begins*,
     not when the image is added to the queue. Avoids the worst case of
     "user adds 10 × 31 GB images = 310 GB temp on queue-add."
  3. **Location**: `~/Library/Caches/com.antimatter-studios.diskcutter/scratch/`
     on macOS; OS-equivalents elsewhere; pref-overridable.
  4. **Cleanup**: ref-counted — temp file lives until the last burn from
     that queue entry's copy-pool finishes (success / error / cancelled).
  5. **Disk-space guardrail**: refuse and warn before materializing if
     we'd consume more than 50% of free space on the cache volume.

Once Phase 2 lands, the offset-map / per-format restart-point machinery from
Phase 3 below becomes obsolete for any image that's been materialized.

**Phase 3 (deferred until a feature needs it)** — partition extraction without
full materialization. Add per-format restart points (xz native block index,
bzip2 magic-marker scan, zstd frame boundaries, gzip zran-style window
snapshots) so a single partition can be extracted from a compressed source
without decompressing the whole file or keeping a temp `.img`. Only relevant
on systems where temp-file materialization is undesirable (low disk, sealed
storage). The gzip leg is the expensive one — needs a gzip lib that can
resume from injected window state, or a fallback to Phase 2.

The clean shape for this: a `DiskOffset` trait alongside the existing
`FormatTryOpen` / `ReaderInterface`, one impl per format:

```rust
pub trait DiskOffset: Send + Sync {
    fn to_json(&self) -> serde_json::Value;
    fn open_at(&self, path: &Path) -> io::Result<Box<dyn ReaderInterface>>;
}
```

Each format stores only what it needs (xz/bzip2/zstd: 8 bytes; gzip:
8 bytes + 32 KB window snapshot). Consumers call `offset.open_at(path)`
and drain — format-specific machinery stays encapsulated. The
`image_scans.partition_offsets` column is the persistence target.

**Future feature this all enables**: "write one partition from a disk image
to an empty region of the target drive, without wiping the rest." Phase 2 is
sufficient (random-access on the temp file); Phase 3 is the disk-frugal
alternative.
