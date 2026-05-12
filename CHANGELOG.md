# Changelog

All notable changes to this project are recorded here. Format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versioning follows
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Planned

- QCOW2 reader (via `am-img-qcow2` with a thin `Read` adapter).
- Compressed image formats: gzip, xz, bzip2, zstd. Probe walks the extension
  chain (`foo.img.xz` → strip `.xz` → probe `foo.img`).
- Streaming SHA-256 of the source image during `inspect_image` so the UI can
  show "hashing…" before the burn even starts.
- Light/dark theme switching driven by `:root[data-theme="dark"]` CSS vars.
- Linux disk enumeration (`/sys/block`) and Windows disk enumeration (SetupDi
  APIs).
- Linux/Windows privilege elevation paths (currently macOS-only via osascript).

## [0.4.0-alpha] — 2026-05-12

First alpha with a real burn pipeline. Earlier builds were UI-only mocks.

### Added

- **Parallel job queue.** Drop multiple disk images, pick a different USB or
  SD target for each, run them in parallel. Sidebar shows live queue / done /
  failed counts.
- **Real disk enumeration on macOS** via `diskutil list -plist` +
  `diskutil info -plist`. Whole-disk filtering, capacity formatting, bus
  protocol, removable / internal flags.
- **Privileged burn helper.** Non-root GUI spawns an `osascript`-elevated
  helper subprocess that opens `/dev/rdiskN` with `O_SYNC` and streams 16 MiB
  chunks; progress + completion events are tailed from a JSONL file in `/tmp`
  and re-emitted into the UI.
- **Read-back verification.** Every burn is followed by a second pass that
  re-reads the device and compares against the source SHA-256, with the first
  256 byte-level mismatches recorded for forensics.
- **SQLite history.** `burn_jobs` and `burn_mismatches` tables (via `rusqlite`,
  bundled), recording start / complete / failure for every job. Drives the
  persistent Sidebar counts.
- **Image format support.** ISO 9660, raw `.img`, `.bin`, `.raw` via the
  pluggable `ImageReaderFactory` registry. QCOW2, gzip, xz, bzip2, and zstd
  are queued (see Unreleased).
- **Cancellation.** Each in-flight job has an `AtomicBool` cancel flag wired
  through the burn + verify loops. UI gets `ECANCELLED` and the queue moves on.
- **Drag-and-drop image add** onto the window via Tauri's drag-drop event.
- **Full Disk Access guidance.** On `EPERM`-style failures the helper surfaces
  `ENEEDS_FDA` and the UI opens the macOS Privacy → Full Disk Access pane via
  `x-apple.systempreferences:` URL.
- **i18n.** `react-i18next` with auto-discovered locale catalogs in
  `src/i18n/locales/` (English, German, Spanish; 155 keys each), plural-aware
  for disk and job counts. New catalogs are picked up by adding a `.json` file.
- **Brutalist UI.** Custom `WindowChrome` (mac / win / lin variants), platform
  toggle, density toggle, accent picker, verbose-title switch, language picker.
  All persisted to `localStorage`.
- **Tweaks panel.** Floating dev panel for runtime theme / platform / density
  switching. Hidden by default; activated via host protocol message.
- **198 tests.** 109 Rust unit tests (`cargo test`) cover the pipeline, plist
  parsing, format helpers, helper-line parser, osascript / shell escaping, and
  command-DB interactions. 89 frontend tests (`vitest` + happy-dom + RTL) cover
  pure helpers, reducers, derivations, and every leaf component.
- **CI.** GitHub Actions runs `cargo fmt`, `cargo clippy -D warnings`, and
  `cargo test` on ubuntu-22.04 + macos-latest (for the `#[cfg(target_os =
  "macos")]` paths), plus `npm test` and `npm run build` on Linux.

### Tech stack

- Tauri 2.0 desktop shell.
- React 18 + Vite 6 frontend.
- Rust pipeline with `sha2`, `plist`, `rusqlite (bundled)`, `libc`,
  `tauri-plugin-dialog`.
- Vitest 2 + happy-dom + React Testing Library for the frontend test suite.

[Unreleased]: https://github.com/antimatter-studios/diskcutter/compare/v0.4.0-alpha...HEAD
[0.4.0-alpha]: https://github.com/antimatter-studios/diskcutter/releases/tag/v0.4.0-alpha
