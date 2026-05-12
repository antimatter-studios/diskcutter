# Changelog

All notable changes are recorded here. Format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versioning
follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- New image readers for `*.gz`, `*.xz`, `*.qcow2` / `*.qcow`. Gzip
  reports the uncompressed size from the ISIZE trailer; XZ sums the
  stream-footer index records (no full decompression needed to size
  the image); QCOW2 streams through `am-img-qcow2` wrapped in
  `am-fs-core`'s `BlockReadStreamer` so sparse clusters expand to
  zeros transparently.
- Format labels carry both the inner image type and the compression
  layer (e.g. "ISO 9660 / XZ") so the inspect panel stays
  informative.
- SQLite-backed config and burn history. New tables: `burn_jobs`,
  `burn_mismatches`, `burn_logs`, plus a key/value `config` table for
  runtime tunables. Tauri commands `config_get`, `config_set`,
  `config_all`, `burn_history_list`, `burn_history_clear`,
  `burn_logs_list` wire them to the UI.
- Pre-commit hook (`.githooks/pre-commit`) and installer
  (`scripts/install-hooks.sh`). Hook lives in the repo so every
  clone picks up the same checks without per-machine setup.
- `PipelinedRawDeviceIo` writer impl: worker-thread pool `pwrite`s
  to a shared FD at supplied offsets through a bounded
  `sync_channel`. Keeps the USB driver queue full; roughly 5x the
  throughput of the single-threaded `write_all` loop on the same
  hardware.
- `BlockDeviceIo` writer impl writes via `/dev/diskN` (buffered block
  path) as a swappable alternative for experimentation.
- Runtime-configurable burn pipeline. Helper subprocess accepts
  `--writer={raw|block|pipelined}`, `--chunk-bytes=N`, `--workers=N`,
  `--queue-depth=N`, `--skip-verify=…`; the main process reads each
  from `config` and forwards.
- Prefs view (sidebar `PREFS` nav target) exposes every runtime
  tunable: writer impl, chunk size, worker count, queue depth, skip
  verify, hash algorithm, max mismatches, language, theme, density,
  auto-eject, auto-clear-done. Each control persists via
  `config_set` and applies side effects inline.
- Dark theme via `:root[data-theme="dark"]` palette in `styles.css` —
  switching themes is one attribute on `<html>`.
- Orphan-helper detection at startup with an osascript-admin
  cleanup action (`find_orphan_helpers` / `kill_orphan_helpers`
  commands, banner in the UI).
- Full Disk Access settings shortcut (`open_fda_settings`) opens the
  right pane in System Settings directly.
- i18n catalog entries for the new Prefs view, the orphan-cleanup
  banner, and scene labels — English, German, and Spanish kept in
  sync.
- Linux disk enumeration: walks `/sys/block`, skips
  loop/ram/dm/sr/fd/md/zram, reads vendor / model / size / removable
  from sysfs, resolves bus by canonicalising the `device/` symlink
  and matching path segments (usb / nvme / mmc / virtio / ata /
  scsi). Partition count comes from sysfs subdirs.
- Windows disk enumeration via `powershell Get-CimInstance
  Win32_DiskDrive | ConvertTo-Json`. Handles the single-object-vs-
  array shape, accepts `Size` as either numeric or string (PowerShell
  serialises u64 outside the JS-safe range as a string).
- 12 new tests covering Windows JSON parsing, sysfs layout reading,
  and the Linux skip rules. Pure-function parsers are `cfg`-gated to
  their target OS or test build so cross-compilation stays clean.

### Changed

- `RawDeviceIo` on macOS now sets `fcntl(F_NOCACHE, 1)` so writes
  skip the unified buffer cache regardless of which writer impl is
  active.
- `burn` / `verify` / `verify_hash_only` now take an explicit
  `chunk_size` parameter so the caller can override the default at
  runtime.
- `DangerBanner` hides once burns are underway (no remaining
  idle-with-target jobs).
- Failure to open the SQLite DB at startup no longer aborts the app —
  it logs and continues without persistence.

### Fixed

- Tightened the macOS `DiskClaim` lifecycle: don't `join()` the DA
  worker thread on `Drop`. `CFRunLoopStop` cannot always promptly
  wake a stuck runloop, and the resulting `join()` could block
  forever and leave the helper as a zombie holding the device FD.
  `std::process::exit` in `main.rs` reaps the thread cleanly on
  process teardown.
- Two stale frontend tests (toolbar density, danger-banner) that no
  longer matched the rendered output.

### Performance

- Default burn chunk size lowered from 16 MiB to 1 MiB
  (`DEFAULT_CHUNK`) to match the typical USB-MSC max transfer length
  on macOS, avoiding kernel-side splitting of larger writes. Matches
  Etcher's default.
- Pipelined writer (see Added) lifts sustained throughput ~5x on
  USB-MSC sticks by keeping enough in-flight `pwrite`s to saturate
  the device's command queue.

### Refactored

- Migration set extracted into `db/migrations.rs` with idempotent
  apply and a health check at open time.

[Unreleased]: https://github.com/antimatter-studios/diskcutter/compare/2819e0e...HEAD
