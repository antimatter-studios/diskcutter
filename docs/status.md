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
