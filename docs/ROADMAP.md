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
| URL → burn pipeline (download + cache + verify) | landed (e81a781 on feat/url-fetch) by 1 | url_fetch.rs: ureq+sha2 streaming download → app_data_dir/downloads/<sanitized>; FROM URL toolbar button + 3 download events; download_complete handler chains into existing inspect_image+queue flow; 14 unit tests |
| GPG signature verification against distro keys | planned | Bundle a small keyring (Ubuntu, Fedora, Debian, Arch); warn on key change |
| Image catalog (curated distro list w/ latest-version pull) | landed (e06898b on feat/image-catalog) by 1 | catalog.rs: hardcoded curated list of 9 distros (Ubuntu Desktop+Server, Fedora, Debian, Mint, Raspberry Pi OS, Tails, FreeBSD, SystemRescue) with download_url + sha256sums_url + homepage; catalog_list Tauri command; frontend picker UI deferred; 9 unit tests |
| Disk → image (back up a USB to `.img.xz`) | claimed by 2 | reverse of burn — reads device, writes compressed image |

## Safety / forensics

| Feature | Status | Notes |
|---|---|---|
| **SMART preflight** on target | landed (10712f9 on feat/smart-preflight) | `smart::check` parses `diskutil info -plist` SMARTStatus → 4-variant verdict, logged on every burn; `smart_check` Tauri command for future picker badge |
| Pre-burn snapshot of target's partition table | landed (76a1f7f on main) by 2 | snapshot.rs: 4 MiB recovery dump w/ JSON header + sha256 verification on restore; 8 unit tests. (instance 1 also drafted on a feature branch — merge conflict; pick one when reconciling) |
| Pre-burn partition inspection (source side) | landed (10f5acf on main) by 2 | inspect.rs uses am-partitions to parse MBR/GPT in raw image; `inspect_any()` dispatches to qcow2/vhd/vhdx/vmdk readers' BlockRead view for container formats |
| Filesystem inspection per partition | landed (10f5acf on main) by 2 | `am-partitions::sniff::classify` identifies ext2/3/4, NTFS, exFAT, FAT16/32, HFS+, APFS, Linux swap, ISO 9660, SquashFS at the start of each partition |
| Bootability test (QEMU launch after verify) | landed (e839c71 on feat/qemu-test) by 1 | qemu.rs: detect qemu-system-x86_64/aarch64/i386, launch in -snapshot mode (read-only behaviour, writes to tmpfile), pragmatic verdict based on warmup-survival heuristic; qemu_check + qemu_test_image Tauri commands; 14 unit tests |
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
| Disk Cutter doctor (env diagnostics) | landed (4339968 on feat/doctor) by 1 | doctor.rs: probes eject backend / QEMU / Full Disk Access / temp-dir writability; pass/warn/fail aggregate; doctor Tauri command; CLI surface deferred; 15 unit tests |

## Wild cards

| Feature | Status | Notes |
|---|---|---|
| Audio cues (per-phase tones, victory chime) | landed (d711faa on feat/audio-cues) by 1 | audio.js: WebAudio synth with write-start tone, verify-start tone, success chime (C5/E5/G5), error sting, cancel click; gated by sound.enabled pref (default off); App.jsx phase tracking via prefsRef + once-per-(job,phase) firing |
| Auto-eject + "next stick please" workflow | landed-partial (96dba76 on feat/eject) by 1 | eject.rs: macOS diskutil eject / Linux udisksctl power-off + eject(1) fallback; auto.eject pref consumed in job-complete listener; "next stick" auto-detect-and-burn part still planned |
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
