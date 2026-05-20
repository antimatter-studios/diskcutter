# Disk Cutter — Status

Snapshot of where the app is and what's coming. Sibling docs:
- [roadmap.md](roadmap.md) — user-approved feature brainstorm, lane-tracked
- [architecture.md](architecture.md) — decoder-chain / source / writer layout
- [burn-queue.md](burn-queue.md) — queue + per-job state machine
- [macos-pipeline.md](macos-pipeline.md) — macOS burn pipeline notes
- [rust-img-qcow2-improvements.md](rust-img-qcow2-improvements.md) — upstream crate wishlist
- [db-cli.md](db-cli.md) — SQLite inspector tooling

## Current state (2026-05-20)

Shipping version: **2026.5.18** — first non-alpha release (CalVer). See [CHANGELOG.md](../CHANGELOG.md).

| Area | State |
| --- | --- |
| Frontend | React + Vite, i18n en/de/es (339 keys each), light/dark theme via `:root[data-theme=…]` |
| Source layer | Single front door in [source.rs](../src-tauri/src/source.rs). Decoder chain ([decoder_chain/](../src-tauri/src/decoder_chain/)) handles raw/iso + xz/gz/bz2/zstd. Container formats qcow2 / vhd / vhdx / vmdk wired via upstream crates. |
| Writer | Real raw-device I/O. `/dev/diskN` → `/dev/rdiskN` on macOS; `O_DIRECT \| O_SYNC` on Linux; pipelined writer with progress events. |
| Disk enumeration | Live probes: `diskutil -plist` (macOS), `/sys/block` (Linux), SetupDi (Windows). |
| DB | 5 migrations: initial → burn_jobs → image_scans → unique job_id → integer PK refactor. Burn history persisted; history UI not yet built (rows surface in-session only). |
| QEMU bootability test, doctor, CLI surface, URL fetch, curated catalog, snapshot/restore, sparse backup, forensic export | shipped in 2026.5.18 |

## Unmerged branches

Three feature branches still ahead of `main`. Recommended land order is
`audio → smart-chip → eject` (audio is the smallest App.jsx surface;
smart-chip is disjoint; eject consumes the `prefsRef` pattern audio
settles). Conflicts to expect: `src-tauri/src/lib.rs` (both eject and
smart-chip add one `mod` + one `invoke_handler!` entry — keep both)
and `src/App.jsx` (eject and audio both touch the event-listener
`useEffect` + add a `prefsRef` — keep one canonical declaration).

- `feat/audio-cues` — WebAudio burn-lifecycle cues
- `feat/eject` — auto-eject on burn complete
- `feat/smart-chip` — SMART status chip in disk picker

## Open backlog

- **Burn-history UI** — `burn_jobs` rows persist, but nothing in the sidebar renders past sessions. Tracked alongside the [known-limitation cards](http://taskhauler.localhost/DC) on the DC board.
- **GPG signature verification on downloaded ISOs** — [DC-19](http://taskhauler.localhost/DC/19); url_fetch already hashes, doesn't verify against a keyring.
- See [roadmap.md](roadmap.md) for the full lane-tracked feature list.

## Image-scan caching — phased plan

**Phase 1 (landed)** — `image_scans` table ([0003_image_scans.sql](../src-tauri/migrations/0003_image_scans.sql)) + eager single-pass scan in [image_scan.rs](../src-tauri/src/image_scan.rs) + progressive UI events. Triggered when the user adds an image (or clicks REFRESH). One sequential decompression per `(image_path, size, mtime)`, stopping at `last_partition_start + 64 KB`. Cached row is shared across every burn_job pointing at the same image path.

**Phase 1 trade-offs to revisit:**
- Source SHA-256 is *not* computed (would require draining the full stream). The user-facing "SHA-256 (SOURCE)" KV was removed from the queue UI, so no immediate cost.
- True uncompressed size is *only* recoverable for xz (footer index via `xz_footer::read_total_uncompressed`). gzip / bzip2 / zstd report compressed file size as an over-conservative placeholder — fine for progress denominator, less fine for an exact display value.

**Phase 2 (deferred)** — temp-file materialization for compressed multi-copy queues only. Decisions locked in:

1. **Trigger**: only when queue entry has `copies > 1`. Single-copy burns stream-decompress as today.
2. **Timing**: temp file materialized when burn process *begins*, not at queue-add. Avoids "10 × 31 GB images = 310 GB temp on queue-add" worst case.
3. **Location**: `~/Library/Caches/com.antimatter-studios.diskcutter/scratch/` on macOS; OS-equivalents elsewhere; pref-overridable.
4. **Cleanup**: ref-counted — temp file lives until the last burn from that copy-pool finishes.
5. **Disk-space guardrail**: refuse and warn before materializing if >50% of free space on cache volume.

Once Phase 2 lands, the per-format restart-point machinery from Phase 3 becomes obsolete for any materialized image.

**Phase 3 (deferred until a feature needs it)** — partition extraction without full materialization. Per-format restart points (xz native block index, bzip2 magic-marker scan, zstd frame boundaries, gzip zran-style window snapshots) so a single partition can be extracted from a compressed source without decompressing the whole file or keeping a temp `.img`. Only relevant on low-disk / sealed-storage systems. The gzip leg is the expensive one — needs a gzip lib that resumes from injected window state, or fallback to Phase 2.

Clean shape: a `DiskOffset` trait alongside `FormatTryOpen` / `ReaderInterface`, one impl per format:

```rust
pub trait DiskOffset: Send + Sync {
    fn to_json(&self) -> serde_json::Value;
    fn open_at(&self, path: &Path) -> io::Result<Box<dyn ReaderInterface>>;
}
```

xz/bzip2/zstd store 8 bytes; gzip stores 8 bytes + 32 KB window snapshot. `image_scans.partition_offsets` is the persistence target.

**Future feature this enables**: "write one partition from a disk image to an empty region of the target drive, without wiping the rest." Phase 2 suffices (random-access on the temp file); Phase 3 is the disk-frugal alternative.
