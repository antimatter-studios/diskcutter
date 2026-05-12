# Disk Cutter

![Disk Cutter main window](docs/media/diskcutter-main-window.png)

> Brutalist disk-image writer with a parallel job queue. Burns ISO/IMG/BIN/RAW
> (QCOW2 + compressed formats landing soon) to USB and SD, then verifies every
> sector against SHA-256.

A no-nonsense desktop disk flasher. Drop several disk images, pick a target USB
stick or SD card for each, hit start — Disk Cutter writes them sector-by-sector
and reads every byte back to confirm the write landed. Privileged writes run in
an `osascript`-elevated helper subprocess on macOS so the GUI never has to be
root.

Built with Tauri 2.0, React 18, and a Rust burn pipeline. macOS is the
flagship target today; Linux and Windows ride along with everything except
real disk enumeration / privilege elevation (see [Roadmap](#roadmap)).

![Status: 0.4.0-alpha](https://img.shields.io/badge/status-0.4.0--alpha-orange)
![License: MIT](https://img.shields.io/badge/license-MIT-blue)

## What it does

- **Parallel job queue.** Multiple disk images in flight at once, each writing
  to its own target. Sidebar shows live queue / done / failed counts.
- **Real disk enumeration on macOS** via `diskutil list -plist` /
  `diskutil info -plist`. Whole-disk filtering, capacity / bus protocol /
  removable / internal flags.
- **Privileged burn pipeline.** The GUI runs unprivileged. When a `/dev/disk*`
  target is selected, an `osascript`-elevated helper subprocess opens
  `/dev/rdiskN` with `O_SYNC`, streams 16 MiB chunks, and emits a JSONL
  progress stream that the GUI tails into the UI.
- **Sector-level verification.** Every burn is followed by a second pass that
  re-reads the device and compares it against the source. SHA-256 is computed
  on both sides; the first 256 byte-level mismatches are recorded for
  forensics.
- **SQLite history.** `burn_jobs` and `burn_mismatches` tables (rusqlite,
  bundled) record start / complete / failure for every job — drives the
  persistent Sidebar counts.
- **Cancellation.** Each in-flight job has a cancel flag wired through both
  burn and verify loops.
- **Drag-and-drop.** Drop images onto the window.
- **i18n.** Plural-aware translations via `react-i18next` with auto-discovered
  locale catalogs (English, German, Spanish today). Drop a new
  `src/i18n/locales/<code>.json` and it appears in the language picker.
- **Full Disk Access guidance.** On macOS `EPERM` paths the helper surfaces
  `ENEEDS_FDA`; the UI opens System Settings → Privacy → Full Disk Access
  directly.
- **Brutalist UI.** Custom window chrome (mac / win / lin variants), density
  toggle, accent picker, verbose title switch. Persists to `localStorage`.

## Supported image formats

| Format | Status |
|---|---|
| ISO 9660 (`.iso`) | ✅ ships |
| Raw disk image (`.img`, `.bin`, `.raw`) | ✅ ships |
| QCOW2 (`.qcow2`) | 🟡 reader landing soon |
| gzip / xz / bzip2 / zstd-wrapped | 🟡 planned |

Format support is pluggable via the `ImageReaderFactory` trait in
`src-tauri/src/readers/`. Adding a format means writing a probe (extension /
magic-bytes) plus a `Read`-implementing reader.

## Running it

```bash
# 1. Install Rust  (https://rustup.rs)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# 2. Install Tauri prerequisites for your OS
#    https://v2.tauri.app/start/prerequisites/

# 3. Install JS deps
npm install

# 4. Run in development (hot-reloads both halves)
npm run tauri dev

# 5. Build a production bundle
npm run tauri build
```

> **macOS first-burn:** the helper subprocess needs Full Disk Access. The app
> surfaces this automatically the first time it hits an `EPERM` — accept the
> prompt, then retry the job. After granting access once it sticks.

## Testing

Three test suites, all green:

```bash
# Frontend (Vitest + happy-dom + React Testing Library)
npm test                  # one-shot
npm run test:watch        # interactive
npm run test:coverage     # with V8 coverage

# Rust (cargo test)
npm run test:rust         # via package script, or:
cargo test --manifest-path src-tauri/Cargo.toml

# Both back to back
npm run test:all
```

**Current count: ~200 tests** covering the burn / verify pipeline, plist
parsing, format helpers, helper-line parser, osascript shell escaping, all
leaf React components, every pure reducer / derivation, and the i18n + tweak
hooks.

CI runs all three on every push and PR via [.github/workflows/ci.yml](.github/workflows/ci.yml)
on ubuntu-22.04 (Rust + frontend) and macos-latest (Rust, for the
`#[cfg(target_os = "macos")]` paths).

## Project layout

```
diskcutter/
├── src/                      React UI
│   ├── App.jsx               main app + scene/tweak state + event listeners
│   ├── components.jsx        WindowChrome, Sidebar, JobRow, DiskPicker…
│   ├── tweaks-panel.jsx      dev tweak panel + useTweaks hook
│   ├── format.js             pure byte/duration/speed formatters
│   ├── job-reducers.js       pure listener state mappers
│   ├── app-derive.js         pure scene / session-stats / plan-start derivations
│   ├── i18n/                 react-i18next bootstrap + locale catalogs
│   └── styles.css            brutalist stylesheet
├── src-tauri/                Rust backend
│   ├── src/
│   │   ├── lib.rs            Tauri command registry
│   │   ├── main.rs           entrypoint + --helper-burn dispatch
│   │   ├── disks.rs          enumeration, commands, run_job orchestrator
│   │   ├── pipeline.rs       burn + verify + mismatch scan
│   │   ├── helper.rs         privileged-helper burn (runs as root)
│   │   ├── db/               rusqlite history, migrations
│   │   ├── readers/          ImageReaderFactory + raw reader
│   │   └── writers/          DeviceIo + plain-file and raw-device backends
│   ├── migrations/           SQL migrations applied at startup
│   └── tauri.conf.json       window / decorations / permissions
├── tests/                    Vitest frontend tests
└── .github/workflows/        CI + release
```

## Architecture sketch

```
┌──────────────┐   invoke('start_write')   ┌──────────────┐
│  React UI    │ ────────────────────────▶ │ Tauri main   │
│  (unpriv)    │                           │  process     │
│              │ ◀──── job events ──────── │  (unpriv)    │
└──────────────┘                           └──────┬───────┘
                                                  │ /dev/disk*?
                                                  │ + !root
                                                  ▼
                                          ┌──────────────┐
                                          │ osascript    │
                                          │ (prompts for │
                                          │  password)   │
                                          └──────┬───────┘
                                                 │
                                                 ▼
                                          ┌──────────────┐    ┌─────────────────┐
                                          │ helper bin   │───▶│ /tmp/dc-*.jsonl │
                                          │  (root)      │    │ (progress tail) │
                                          │  --helper-   │    └────────┬────────┘
                                          │  burn        │             │ tail
                                          │  /dev/rdiskN │             │
                                          └──────────────┘             ▼
                                                              re-emitted to UI
```

The unprivileged GUI never opens the device. The same binary is re-exec'd with
`--helper-burn` under `osascript ... with administrator privileges`, opens
`/dev/rdiskN` itself, and streams a JSONL progress feed that the main process
tails and re-emits as Tauri events.

## Errors the UI knows about

| Code | Meaning |
|---|---|
| `ETOOBIG` | Image larger than the selected target. |
| `EHASHMISMATCH` | Burn finished but verify SHA-256 didn't match. |
| `EUNSUPPORTED` | Image format not recognized. |
| `EIMAGE` | Failed to open the source image. |
| `ETARGET` | Failed to open the target device. |
| `EIO` | I/O error during burn or verify. |
| `ECANCELLED` | User cancelled mid-flight. |
| `ESIZEMISMATCH` | Bytes written didn't equal image size. |
| `ENEEDS_FDA` | macOS Full Disk Access required for the helper. |
| `EHELPER` / `EAUTH` | Privileged helper failed to spawn or was denied. |

## Roadmap

See [docs/TODO.md](docs/TODO.md) for the full backlog. Headline items:

- **QCOW2 reader** via `am-img-qcow2` + a `SequentialReader<'a>` adapter
  ([improvement notes](docs/rust-img-qcow2-improvements.md)).
- **Compressed image layering** — `foo.img.xz` probes by stripping the
  compression extension chain.
- **Streaming source-image SHA-256** during `inspect_image` so the UI shows
  "hashing…" before the burn starts.
- **Linux** disk enumeration (`/sys/block`) and privilege elevation (`pkexec`
  / polkit).
- **Windows** disk enumeration (SetupDi APIs) and privilege manifest.
- **Light / dark theme** via `:root[data-theme="dark"]` CSS vars (palette
  already lives in vars).

## Contributing

Pull requests welcome. Two things to keep in mind:

1. **Don't bypass the tests.** CI runs `cargo fmt -- --check`, `cargo clippy -D
   warnings`, `cargo test`, and `npm test` on every push. PRs go red fast.
2. **Add tests for what you change.** The unit test surface is broad and
   cheap — every pure function in `src/format.js`, `src/job-reducers.js`,
   `src/app-derive.js`, and most of `src-tauri/src/disks.rs` is unit-tested
   without touching React or real disks. Follow the pattern.

## License

[MIT](LICENSE) © 2026 Antimatter Studios.

The app links dynamically against LGPL-2.1 system libraries on Linux
(webkit2gtk-4.1, gtk-3, libsoup-3.0, librsvg). LGPL permits this from a
permissively-licensed application — no infectious effect on the source
license.
