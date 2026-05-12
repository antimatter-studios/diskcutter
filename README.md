# Disk Cutter

A desktop disk-image writer: pick an ISO/IMG/QCOW2/gz/xz file, pick a USB
stick or SD card, get a sector-verified flash with progress and history.
Tauri 2 shell, React 18 UI, Rust burn pipeline. Effectively a Rust port of
the Balena Etcher core idea, with a parallel job queue, an SQLite-backed
history, and a swappable writer pipeline.

![Disk Cutter main window](docs/screenshot.png)

## Status

Alpha. macOS is the primary target — raw-device I/O, DiskArbitration
unmount, and `osascript` privilege elevation all work end-to-end there.
Linux and Windows builds compile and run the UI; disk enumeration is
partial and the elevation paths are not wired up yet.

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

## License

MIT. See [LICENSE](LICENSE).
