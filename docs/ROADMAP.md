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
| **SMART preflight** on target | claimed by 1 | parse `diskutil info -plist` `SMARTStatus`; warn on Failing / reallocated sectors |
| Pre-burn snapshot of target's partition table | claimed by 2 | dump first 4 MiB to a recovery file before destruction |
| Pre-burn partition inspection (source side) | claimed by 2 | parse MBR/GPT inside the source image via am-partitions, surface in `inspect_image` |
| Filesystem inspection per partition | claimed by 2 | am-fs-ext4 / am-fs-ntfs detect fs type, label, used/free; display-only |
| Bootability test (QEMU launch after verify) | planned | shell out to `qemu-system-*`; optional |
| Forensic-grade burn record (PDF/JSON export) | planned | image SHA, target serial+model, write timestamps, host machine ID |

## Pipeline / performance

| Feature | Status | Notes |
|---|---|---|
| Concurrent verify (read back as we write) | claimed by 2 | second IO queue on pipelined writer; halves total time on fast media |
| Sparse / skip-zero writing | claimed by 2 | for qcow2 / sparse images; needs upstream `allocated_extents()` on am-img-* BlockRead; massive win on thin-provisioned VM images |
| **Bulk parallel burns** (N USBs, one image) | planned | frontend orchestration of existing `start_write`; per-target progress lane |

## Power-user surface

| Feature | Status | Notes |
|---|---|---|
| CLI front-end (`disk-cutter burn ubuntu.iso /dev/disk5`) | planned | scriptable; talks to same Tauri backend or standalone binary |
| Watch folder (auto-queue dropped images) | planned | `notify` crate; configurable path; uses last target |
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
