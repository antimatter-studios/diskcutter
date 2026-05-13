# Disk Cutter — Roadmap

User-approved feature brainstorm (2026-05-12). Tracked here so the
multi-instance team can claim lanes and avoid collisions.

Status legend:
- **planned** — not started
- **claimed by N** — instance N working on it
- **in flight** — branch exists, work in progress
- **landed** — committed to main

Update this file as work moves; it's the single source of truth.

---

## Source diversity

| Feature | Status | Notes |
|---|---|---|
| URL → burn pipeline (download + cache + verify) | planned | reqwest + progress events; cache under `app_data_dir()/downloads/` |
| GPG signature verification against distro keys | planned | Bundle a small keyring (Ubuntu, Fedora, Debian, Arch); warn on key change |
| Image catalog (curated distro list w/ latest-version pull) | planned | TOML manifest in repo + remote refresh; pin local copies |
| Disk → image (back up a USB to `.img.xz`) | claimed by 2 | reverse of burn — reads device, writes compressed image |

## Safety / forensics

| Feature | Status | Notes |
|---|---|---|
| **SMART preflight** on target | landed (10712f9 on feat/smart-preflight) | `smart::check` parses `diskutil info -plist` SMARTStatus → 4-variant verdict, logged on every burn; `smart_check` Tauri command for future picker badge |
| Pre-burn snapshot of target's partition table | landed (76a1f7f on main) by 2 | snapshot.rs: 4 MiB recovery dump w/ JSON header + sha256 verification on restore; 8 unit tests. (instance 1 also drafted on a feature branch — merge conflict; pick one when reconciling) |
| Pre-burn partition inspection (source side) | landed (10f5acf on main) by 2 | inspect.rs uses am-partitions to parse MBR/GPT in raw image; `inspect_any()` dispatches to qcow2/vhd/vhdx/vmdk readers' BlockRead view for container formats |
| Filesystem inspection per partition | landed (10f5acf on main) by 2 | `am-partitions::sniff::classify` identifies ext2/3/4, NTFS, exFAT, FAT16/32, HFS+, APFS, Linux swap, ISO 9660, SquashFS at the start of each partition |
| Bootability test (QEMU launch after verify) | planned | shell out to `qemu-system-*`; optional |
| Forensic-grade burn record (JSON+Markdown export) | landed (b62ac0f on main) by 2 | forensic.rs: tamper-evident report w/ sha256 digest over canonical JSON; HostInfo + BurnSection + LogEntry; 11 unit tests. Tauri command wiring deferred. (instance 1 also claimed this — see notes; their version on a branch if any) |

## Pipeline / performance

| Feature | Status | Notes |
|---|---|---|
| Concurrent verify (read back as we write) | planned | second IO queue on pipelined writer; halves total time on fast media (was claimed by 2; deferred — pipeline.rs collision risk) |
| Sparse / skip-zero writing | landed-partial (cca8574 on main) by 2 | sparse.rs: SparseFileWriter punches holes for zero chunks on write — saves on disk for backups. Full read-side skip-zero still wants upstream `allocated_extents()` on am-img-* BlockRead — see morning summary for the design sketch |
| **Bulk parallel burns** (N USBs, one image) | planned | frontend orchestration of existing `start_write`; per-target progress lane |

## Power-user surface

| Feature | Status | Notes |
|---|---|---|
| CLI front-end (`disk-cutter burn ubuntu.iso /dev/disk5`) | landed (e166c74 on main) by 2 | cli.rs: inspect / formats / backup / snapshot / restore / version / help subcommands; main.rs routes recognised subcommands to run_cli, no-arg still launches GUI; 24 unit tests |
| Watch folder (auto-queue dropped images) | landed (4ed7fd5 on feat/watch-folder) | `notify` crate; `watch_folder_{start,stop,status}` commands; emits `disk-cutter://image-found`; debounced; needs frontend wire-up |
| Per-chunk latency heatmap in log detail view | planned | needs per-chunk telemetry from pipeline; pure frontend if data exists |

## Wild cards

| Feature | Status | Notes |
|---|---|---|
| Audio cues (per-phase tones, victory chime) | planned | pure frontend, AudioContext API; tiny scope |
| Auto-eject + "next stick please" workflow | planned | eject after success, listen for new insertion, auto-burn last image |
| Multi-boot stick (Ventoy-style) | planned | niche; leaves the brutalist-tool DNA but extremely loved |

---

## Lane allocation rules

1. Pick a row marked `planned`.
2. Edit this file: change to `claimed by N`, add a one-line `Notes` update.
3. Post a `claim` to comm.jsonl with the file lane: `bash ~/.claude/skills/claude-workgroup/post-comm.sh diskcutter N claim "<feature> — files X, Y, Z" reason="..."`
4. Work on a feature branch off main: `feat/<short-name>`.
5. Land via small commits (commit skill).
6. On land: change to `landed`, add commit SHA.

## Conflict map (live)

- `src-tauri/src/disks.rs::start_write` — multiple wanters; coordinate edits via comm
- `src-tauri/src/lib.rs` — invoke_handler list grows with each new command
- `src/App.jsx`, `src/components.jsx` — frontend collisions; instance 3 owns Logs/Prefs frontend
- `src/i18n/locales/*.json` — append at the end; merge conflicts are easy here

## Already landed (from the user's pre-roadmap work)

- SQLite-backed config + burn history (db layer + migrations)
- Logs view UI (instance 3, wave 2 — frontend)
- Pipelined raw writer (Etcher-style worker pool)
- DiskArbitration claim lifecycle
- Multi-format readers: ISO, gzip, xz, qcow2, vhd, vhdx, vmdk, bzip2, zstd (instance 2)
- Linux/Windows disk enumeration (instance 2)
- Pre-commit hook + installer
- Content-based magic-byte detection in readers (instance 2 — 80cb535)
