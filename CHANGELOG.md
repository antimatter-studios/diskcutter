# Changelog

All notable changes to this project are recorded here. Format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versioning
follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **Curated distro image catalog.** JSON-backed picker (Ubuntu,
  Debian, Fedora, Raspberry Pi OS, …) with remote refresh and a
  bundled fallback so the picker still works offline. Catalog and
  schema are published alongside the website on GitHub Pages.
- **URL fetch.** Paste an image link, the app downloads it and
  queues it for burn.
- **QEMU bootability test.** Post-burn smoke test that the image
  actually boots in a local QEMU snapshot.
- **Doctor / self-diagnostic.** Environment checklist surfaced both
  in the Prefs view and as `diskcutter doctor` on the CLI.
- **Standalone CLI surface** (`diskcutter list` / `burn` / `doctor`
  / …) for scripted use, sharing the same pipeline as the GUI.
- **Tamper-evident forensic burn-record export.** JSON + Markdown,
  for audit trails.
- **Pre-burn content validation.** Partition-table probe + whole-
  device filesystem sniff guards against archives masquerading as
  bootable images (e.g. a `.tar.gz` of photos that passes the gzip
  magic check). Falls back to accepting an `0x55 0xAA` boot
  signature so custom embedded images still burn.
- **Sparse-aware hole-punching writer** and opt-in sparse backups.
- **QCOW2 sparse-aware backup fast path** via
  `allocated_extents` — only reads/writes used clusters.
- **Disk picker grouping** into Allowed / Too Small / Not Permitted.
- **Selectable hash algorithm.** Vendored xxh64 alongside SHA-256,
  selectable via `--hash-algo` and the Prefs view.
- **Hash benchmark example** (`cargo run --example hash_bench`)
  comparing sha256 vs xxh64 throughput.
- **Writer benchmark harness** (`cargo run --example writer_bench`).
- **Auto-eject after a successful burn.**
- **Real hazard-rim sidebar logo** replacing the placeholder.
- **Image readers** for `*.gz`, `*.xz`, `*.qcow2` / `*.qcow`. Gzip
  reports the uncompressed size from the ISIZE trailer; XZ sums the
  stream-footer index records (no full decompression needed to size
  the image); QCOW2 streams through `am-img-qcow2` wrapped in
  `am-fs-core`'s `BlockReadStreamer` so sparse clusters expand to
  zeros transparently.
- **VHD, VHDX, and VMDK readers** wrapping `am-img-vhd`,
  `am-img-vhdx`, and `am-img-vmdk`. Sparse virtual disks emit zeros
  for unallocated clusters. Registry probe order prefers structured
  container formats over the raw fallback to avoid misclassifying
  files like `backup.iso.vhd`.
- **Bzip2 (`.bz2` / `.bzip2`) and Zstd (`.zst` / `.zstd`) readers.**
  Zstd parses the Frame Content Size header (RFC 8478 §3.1.1) when
  present; bzip2 has no size hint so progress falls back to
  compressed size with the pipeline tolerating drift.
- **Content-based magic-byte detection.** Reader factories now
  accept a file when either the extension or the format's magic
  signature matches, so a renamed `ubuntu.iso.gz` → `ubuntu.iso`
  still routes correctly. `magic.rs` centralises signature checks
  for qcow2, vhdx, vmdk, vhd, gzip, xz, bzip2, and zstd.
- Format labels carry both the inner image type and the
  compression layer (e.g. "ISO 9660 / XZ") so the inspect panel
  stays informative.
- **Disk → image backup pipeline** (`backup.rs`). Reverse of
  `pipeline::burn`: reads a source (block device or file), streams
  through an optional compression encoder
  (none / gzip / xz / bzip2 / zstd), hashes the uncompressed
  stream, writes to disk. Emits `BackupProgress` at 250 ms
  intervals so the UI reuses the burn progress component.
- **Source-image inspector.** Partition table + filesystem probe
  before the burn commits.
- **Pre-burn snapshot** of the target's first 4 MiB so a
  mis-targeted disk is recoverable.
- **SQLite-backed config and burn history.** New tables:
  `burn_jobs`, `burn_mismatches`, `burn_logs`, plus a key/value
  `config` table. Tauri commands `config_get`, `config_set`,
  `config_all`, `burn_history_list`, `burn_history_clear`,
  `burn_logs_list` wire them to the UI.
- **Logs view** in the sidebar surfacing past burns from the
  `burn_history` table, with per-row mismatch and log detail.
- **Pre-commit hook** (`.githooks/pre-commit`) and installer
  (`scripts/install-hooks.sh`). Runs `cargo fmt --check`,
  `clippy -D warnings`, and an i18n key-parity check.
- **`PipelinedRawDeviceIo` writer impl.** Worker-thread pool
  `pwrite`s to a shared FD at supplied offsets through a bounded
  `sync_channel`. Keeps the USB driver queue full; roughly 5× the
  throughput of the single-threaded `write_all` loop on the same
  hardware.
- **`BlockDeviceIo` writer impl** writes via `/dev/diskN`
  (buffered block path) as a swappable alternative for
  experimentation.
- **Runtime-configurable burn pipeline.** Helper subprocess accepts
  `--writer={raw|block|pipelined}`, `--chunk-bytes=N`,
  `--workers=N`, `--queue-depth=N`, `--skip-verify=…`,
  `--hash-algo=…`; the main process reads each from `config` and
  forwards.
- **Prefs view** (sidebar `PREFS` nav target) exposes every runtime
  tunable: writer impl, chunk size, worker count, queue depth,
  skip verify, hash algorithm, max mismatches, language, theme,
  density, auto-eject, auto-clear-done.
- **Dark theme** via `:root[data-theme="dark"]` palette in
  `styles.css` — switching themes is one attribute on `<html>`.
- **Orphan-helper detection at startup** with an osascript-admin
  cleanup action.
- **Full Disk Access settings shortcut** opens the right pane in
  System Settings directly.
- i18n catalog entries for every new view — English, German, and
  Spanish kept in sync by the pre-commit hook.
- **Linux disk enumeration.** Walks `/sys/block`, skips
  loop/ram/dm/sr/fd/md/zram, reads vendor / model / size /
  removable from sysfs, resolves bus by canonicalising the
  `device/` symlink. Partition count from sysfs subdirs.
- **Windows disk enumeration** via
  `powershell Get-CimInstance Win32_DiskDrive | ConvertTo-Json`.
  Handles the single-object-vs-array shape and `Size` as either
  numeric or string.
- **Drag-drop overlay** with a brutalist "DROP DISK IMAGE HERE"
  prompt.
- **Global keyboard shortcuts:** Cmd/Ctrl+O for add-image, Return
  for start-queue, Cmd/Ctrl+, for prefs nav, Cmd/Ctrl+L for logs
  nav.
- **Transient toast notifications** for app-level errors.
- **Integration tests** for the burn + verify pipeline end-to-end.
- **Landing site + GitHub Pages workflow.**
- **CONTRIBUTING guide + GitHub Actions CI workflow.**

### Changed

- **Crate / binary renamed** `disk-cutter` → `diskcutter`,
  `disk_cutter_lib` → `diskcutter_lib`.
- `RawDeviceIo` on macOS now sets `fcntl(F_NOCACHE, 1)` so writes
  skip the unified buffer cache regardless of which writer impl is
  active.
- `burn` / `verify` / `verify_hash_only` take an explicit
  `chunk_size` parameter so the caller can override the default at
  runtime.
- `DangerBanner` hides once burns are underway.
- Failure to open the SQLite DB at startup no longer aborts the
  app — it logs and continues without persistence.

### Fixed

- Tightened the macOS `DiskClaim` lifecycle: don't `join()` the DA
  worker thread on `Drop`. `CFRunLoopStop` cannot always promptly
  wake a stuck runloop, and the resulting `join()` could block
  forever and leave the helper as a zombie holding the device FD.
  `std::process::exit` in `main.rs` reaps the thread cleanly.
- Two stale frontend tests (toolbar density, danger-banner).

### Performance

- Default burn chunk size lowered from 16 MiB to 1 MiB
  (`DEFAULT_CHUNK`) to match the typical USB-MSC max transfer
  length on macOS, avoiding kernel-side splitting of larger
  writes. Matches Etcher's default.
- Pipelined writer lifts sustained throughput ~5× on USB-MSC
  sticks by keeping enough in-flight `pwrite`s to saturate the
  device's command queue.

### Refactored

- Migration set extracted into `db/migrations.rs` with idempotent
  apply and a health check at open time.
- Keystroke matching and editable-target detection extracted to a
  pure `src/keymap.js` module with unit coverage.

## [0.4.0-alpha] — 2026-05-12

First alpha with a real burn pipeline. Earlier builds were UI-only
mocks.

### Added

- **Parallel job queue.** Drop multiple disk images, pick a different
  USB or SD target for each, run them in parallel. Sidebar shows live
  queue / done / failed counts.
- **Real disk enumeration on macOS** via `diskutil list -plist` +
  `diskutil info -plist`. Whole-disk filtering, capacity formatting,
  bus protocol, removable / internal flags.
- **Privileged burn helper.** Non-root GUI spawns an
  `osascript`-elevated helper subprocess that opens `/dev/rdiskN`
  with `O_SYNC` and streams 16 MiB chunks; progress + completion
  events are tailed from a JSONL file in `/tmp` and re-emitted into
  the UI.
- **Read-back verification.** Every burn is followed by a second
  pass that re-reads the device and compares against the source
  SHA-256, with the first 256 byte-level mismatches recorded for
  forensics.
- **SQLite history.** `burn_jobs` and `burn_mismatches` tables (via
  `rusqlite`, bundled), recording start / complete / failure for
  every job. Drives the persistent Sidebar counts.
- **Image format support.** ISO 9660, raw `.img`, `.bin`, `.raw`
  via the pluggable `ImageReaderFactory` registry.
- **Cancellation.** Each in-flight job has an `AtomicBool` cancel
  flag wired through the burn + verify loops. UI gets `ECANCELLED`
  and the queue moves on.
- **Drag-and-drop image add** onto the window via Tauri's drag-drop
  event.
- **Full Disk Access guidance.** On `EPERM`-style failures the
  helper surfaces `ENEEDS_FDA` and the UI opens the macOS
  Privacy → Full Disk Access pane via `x-apple.systempreferences:`
  URL.
- **i18n.** `react-i18next` with auto-discovered locale catalogs in
  `src/i18n/locales/` (English, German, Spanish), plural-aware for
  disk and job counts.
- **Brutalist UI.** Custom `WindowChrome` (mac / win / lin
  variants), platform toggle, density toggle, accent picker,
  verbose-title switch, language picker. All persisted to
  `localStorage`.
- **Tweaks panel.** Floating dev panel for runtime theme / platform
  / density switching. Hidden by default; activated via host
  protocol message.
- **198 tests.** 109 Rust unit tests + 89 frontend tests.
- **CI.** GitHub Actions runs `cargo fmt`, `cargo clippy
  -D warnings`, and `cargo test` on ubuntu-22.04 + macos-latest,
  plus `npm test` and `npm run build` on Linux.

### Tech stack

- Tauri 2.0 desktop shell.
- React 18 + Vite 6 frontend.
- Rust pipeline with `sha2`, `plist`, `rusqlite (bundled)`, `libc`,
  `tauri-plugin-dialog`.
- Vitest 2 + happy-dom + React Testing Library for the frontend
  test suite.

[Unreleased]: https://github.com/antimatter-studios/diskcutter/compare/v0.4.0-alpha...HEAD
[0.4.0-alpha]: https://github.com/antimatter-studios/diskcutter/releases/tag/v0.4.0-alpha
