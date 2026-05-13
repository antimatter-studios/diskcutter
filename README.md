# Disk Cutter

A desktop disk-image writer: pick an ISO/IMG/QCOW2/gz/xz file, pick a USB
stick or SD card, get a sector-verified flash with progress and history.
Tauri 2 shell, React 18 UI, Rust burn pipeline. Effectively a Rust port of
the Balena Etcher core idea, with a parallel job queue, an SQLite-backed
history, and a swappable writer pipeline.

![status: alpha](https://img.shields.io/badge/status-alpha-orange)
![platform: macOS](https://img.shields.io/badge/platform-macOS-blue)
![tauri: 2.0](https://img.shields.io/badge/tauri-2.0-24c8db)
![license: MIT](https://img.shields.io/badge/license-MIT-green)

<!-- TODO: capture docs/screenshot.png (light) and docs/screenshot-dark.png (dark) -->

![Disk Cutter main window](docs/screenshot.png)
![Disk Cutter dark theme](docs/screenshot-dark.png)

## Status

Alpha. macOS is the primary target — raw-device I/O, DiskArbitration
unmount, and `osascript` privilege elevation all work end-to-end there.
Linux and Windows builds compile and run the UI; disk enumeration is
partial and the elevation paths are not wired up yet.

## Features

- Write disk images to USB sticks and SD cards
- Multi-format readers: raw ISO/IMG, gzip, xz, bzip2, zstd, qcow2,
  VHD, VHDX, VMDK
- Content-based magic-byte detection — not just file-extension sniffing
- SHA-256 verification of every write, with byte-by-byte fallback on
  hash mismatch
- Multi-disk queue: flash the same image to many targets in parallel
- Persistent burn history (SQLite, surfaced in the Logs view)
- Pre-burn snapshot of the target's first 4 MiB so a mis-targeted disk
  is recoverable
- Disk → image backup pipeline (read a disk back to a sparse image)
- Source-image inspector: partition table + filesystem probe before
  you commit to a write
- SMART preflight check on the target drive
- i18n (English, German, Spanish) and a light/dark theme toggle
- macOS-native privilege escalation via `osascript` + a
  DiskArbitration session that sidesteps the `diskutil unmountDisk`
  remount race
- Drag-and-drop overlay and keyboard shortcuts in the UI
- Transient toast notifications for app-level errors
- 12 user-tunable performance prefs (chunk size, worker threads, queue
  depth, hash algorithm, writer impl, etc.) in the Preferences view

## Install (macOS)

1. Download the latest `.dmg` from the Releases page (or build one
   yourself — see [Development](#development)).
2. Drag **Disk Cutter.app** to **/Applications**.
3. Launch it once. The first time you start a burn, macOS will block
   `/dev/rdiskN` access. Open **System Settings → Privacy & Security →
   Full Disk Access**, flip the toggle for **Disk Cutter**, and retry.
   The app surfaces an `ENEEDS_FDA` banner with a one-click shortcut to
   the right pane when this happens.

## Usage

- Add an image (drag-and-drop onto the window, or use the toolbar).
- Pick a disk (the picker only shows whole, removable targets).
- Hit start. Verification runs automatically — fast hash compare on the
  happy path, byte-by-byte fallback if hashes diverge.

## Development

```sh
npm install
npx tauri dev          # hot-reloads UI + Rust
npx tauri build        # bundled .app / .dmg in src-tauri/target/release/bundle
```

For Rust-only iteration (skip the JS toolchain):

```sh
cargo check --manifest-path src-tauri/Cargo.toml
cargo test  --manifest-path src-tauri/Cargo.toml
```

Pre-commit hook (`cargo fmt --check` + `clippy -D warnings`) lives at
`.githooks/pre-commit`; enable with `git config core.hooksPath .githooks`
or run `./scripts/install-hooks.sh`.

## Architecture

The UI runs unprivileged. When a burn starts, the main binary re-execs
itself with `--helper-burn` under `osascript "with administrator
privileges"`; the elevated helper opens the raw device, drives the
write+verify pipeline, and streams JSONL progress to a temp file that
the main process tails and re-emits as Tauri events. Unmount goes
through a DiskArbitration session we own (avoids the `diskutil
unmountDisk` remount race). The actual byte-pushing is done by one of
three swappable `DeviceIo` impls — single-thread raw, single-thread
block, or a pipelined worker pool — selected at runtime via a config
key.

Full breakdown lives in [docs/architecture.md](docs/architecture.md).
Per-release notes in [docs/CHANGELOG.md](docs/CHANGELOG.md).

## FAQ

**Why does it ask for my password on every burn?**
macOS requires admin rights to open `/dev/rdiskN` for writing. The
helper process is re-execed under `osascript` for each burn so the
elevated binary lives only as long as the job; there is no persistent
root daemon.

**Why does Full Disk Access keep coming up?**
TCC sits above the admin layer. Even as root, the helper can't touch
removable raw devices until the **Disk Cutter.app** bundle is granted
Full Disk Access. Toggle it once in System Settings and macOS
remembers.

**How do I switch the writer implementation for testing?**
Open **Preferences → Performance** and change the writer impl
(`raw` / `block` / `pipelined`). The setting takes effect on the next
job; nothing to restart.

**What does "verify failed: hash mismatch" mean?**
The image was written but reading it back produced a different SHA-256.
Almost always a flaky SD card or a worn USB stick — try a different
target. If the same target fails repeatedly across known-good images,
retire it.

## License

MIT. See [LICENSE](LICENSE).
