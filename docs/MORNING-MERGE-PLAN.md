# Morning merge plan — instance 1's overnight wave

8 feature branches landed by instance 1, all sitting at the tips of
local `feat/*` branches off `main`. None pushed to `origin`. The
worktrees that built them have been cleaned up; the branches persist
in `.git/refs/heads/`.

Each branch is mechanically green on its own:
- `cargo build --quiet` succeeds
- `cargo test --lib <module>` passes
- `cargo clippy --all-targets -- -D warnings` clean
- `cargo fmt --check` clean (after the per-branch trivial drift fix
  on `forensic.rs` for the four backend-only branches that touch it)

## Heads-up — main has moved since this doc was first written

Four commits landed on `main` after this doc was authored. Read them
before starting the merge so you know what state main is in:

| SHA       | Title                                                                        | Notes |
|-----------|------------------------------------------------------------------------------|-------|
| `ab1f705` | chore: rename crate disk-cutter → diskcutter                                 | Package name + lib name (`diskcutter_lib`) + binary name all renamed. **Event names (`disk-cutter://*`) and sqlite filename intentionally kept** for backwards-compat with the frontend and existing users. Any cluster-A branch that uses `disk_cutter_lib::` in its source will need a one-line rebase to `diskcutter_lib::`. |
| `d7218c1` | feat(commands): wire backup/inspect/snapshot/forensic into Tauri IPC         | New file `src-tauri/src/commands.rs` adds `inspect_partitions`, `capture_snapshot`, `restore_snapshot`, `export_burn_report`, `run_backup` Tauri commands. `db/mod.rs` gains `pub mod migrations`. **Affects every cluster-A branch's lib.rs hunk** — the invoke_handler list is now longer, so the textual-conflict footprint grows. Resolution stays mechanical: keep all the new entries. |
| `85c59e5` | feat(reader): allocated_extents API for sparse-aware consumers               | **Upstream commit on `feat/allocated-extents` in `/Volumes/sdcard256gb/projects/rust-img-qcow2`** — not in this repo. Adds `Qcow2Reader::extents()` + `cluster_status_at()` + `ClusterStatus` enum. Needs publishing as `am-img-qcow2 0.3` before the `[patch.crates-io]` block below can be dropped. |
| `f315f6f` | feat(backup): qcow2 sparse-aware fast path via allocated_extents             | Consumes the upstream API: skips reading zero/unallocated clusters on qcow2 sources. **Adds a `[patch.crates-io]` override block to `src-tauri/Cargo.toml`** pointing am-img-qcow2 + am-fs-core + am-partitions + am-img-vhd/vhdx/vmdk at local working trees. Watch for conflicts when merging `feat/watch-folder` and `feat/url-fetch` (both touch Cargo.toml — see "Cargo.toml" subsection below). |

The morning summary doc [`docs/morning-2026-05-13.md`](./morning-2026-05-13.md)
covers all four in more depth.

The 8 branches in the table below are all real and 1+ commits ahead
of main. `feat/forensic-record` exists but is empty (0 ahead) and
`feat/snapshot` was never created — both lanes' work landed directly
on main as `b62ac0f` (forensic) and `76a1f7f` (snapshot). Cleanup
script at the bottom drops the empty branch.

## What landed (in time order)

| # | Branch | SHA | Adds (Rust) | Touches | Tauri commands | Frontend |
|---|---|---|---|---|---|---|
| 1 | `feat/smart-preflight` | `10712f9` | `smart.rs` (~256 LOC, 12 tests) | `lib.rs` (+1 mod, +1 cmd), `disks.rs` (+17 LOC in `start_write` to log SMART verdict) | `smart_check` | none |
| 2 | `feat/audio-cues` | `d711faa` | — | `App.jsx` (~44 LOC: phase tracking, prefsRef, unlock listener), `components.jsx` (+2: pref toggle), `i18n/locales/{en,de,es}.json` | none | NEW `src/audio.js` (~120 LOC, WebAudio synth) |
| 3 | `feat/watch-folder` | `4ed7fd5` | `watch_folder.rs` (~427 LOC, 17 tests) | `Cargo.toml` (+`notify`), `Cargo.lock`, `lib.rs` (+1 mod, +3 cmds, +`manage()` setup) | `watch_folder_start`, `_stop`, `_status` (emits `disk-cutter://image-found`) | not wired |
| 4 | `feat/url-fetch` | `e81a781` | `url_fetch.rs` (~443 LOC, 14 tests) | `Cargo.toml` (+`ureq`), `Cargo.lock`, `lib.rs` (+1 mod, +2 cmds, +`manage()`), `App.jsx` (~58 LOC: `addImageFromUrl` handler + 3 download event listeners + `addImageFromPathRef`), `components.jsx` (FROM URL toolbar button), `i18n/locales/*.json` (+`url.*`, +`toolbar.from_url`) | `start_download`, `cancel_download` (emits `disk-cutter://download-{progress,complete,error}`) | wired |
| 5 | `feat/image-catalog` | `e06898b` | `catalog.rs` (~263 LOC, 9 tests) | `lib.rs` (+1 mod, +1 cmd), `forensic.rs` (digit-grouping fix + rustfmt) | `catalog_list` | not wired |
| 6 | `feat/qemu-test` | `e839c71` | `qemu.rs` (~382 LOC, 14 tests) | `lib.rs` (+1 mod, +2 cmds), `forensic.rs` (digit-grouping fix) | `qemu_check`, `qemu_test_image` | not wired |
| 7 | `feat/eject` | `96dba76` | `eject.rs` (~230 LOC, 8 tests) | `lib.rs` (+1 mod, +1 cmd), `forensic.rs` (digit-grouping fix), `App.jsx` (~49 LOC: `prefsRef`, ejector in `job-complete` listener, removes the stale `// TODO: invoke('eject_disk', …)` comment) | `eject_disk` | wired |
| 8 | `feat/doctor` | `4339968` (+ `1a313b8` for ROADMAP) | `doctor.rs` (~421 LOC, 15 tests) | `lib.rs` (+1 mod, +1 cmd), `forensic.rs` (digit-grouping fix), `docs/ROADMAP.md` | `doctor` | not wired |

Total new lines (excluding tests + Cargo.lock): ~2,400. Total new
unit tests: 89.

## Recommended merge order

Two clusters by surface area:

**Cluster A — backend-only, zero frontend collisions, merge first**
1. `feat/smart-preflight` (touches `disks.rs::start_write` — should
   go in first because it's the smallest backend change to that
   contested function)
2. `feat/image-catalog`
3. `feat/qemu-test`
4. `feat/doctor`
5. `feat/watch-folder` (touches `Cargo.toml`; rebase against any
   other Cargo.toml changes first)

**Cluster B — frontend-touching, merge after the live tree's WIP
settles**
6. `feat/audio-cues` (App.jsx, components.jsx, i18n)
7. `feat/url-fetch` (App.jsx, components.jsx, i18n, Cargo.toml)
8. `feat/eject` (App.jsx — also removes a stale TODO; carries
   `forensic.rs` fix already in cluster A's branches)

If you want one merged-up integration branch instead of 8 PRs:

```bash
# Make a fresh integration branch off the latest main.
git fetch
git switch -c integration/instance1-wave origin/main
# Cluster A first.
for b in feat/smart-preflight feat/image-catalog feat/qemu-test feat/doctor feat/watch-folder; do
  git merge --no-ff $b -m "merge $b"
  cargo test --manifest-path src-tauri/Cargo.toml --lib && \
    cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings || break
done
# Then cluster B.
for b in feat/audio-cues feat/url-fetch feat/eject; do
  git merge --no-ff $b -m "merge $b"
  npm test --silent && \
    cargo test --manifest-path src-tauri/Cargo.toml --lib || break
done
```

## Conflicts to expect

### `src-tauri/src/lib.rs`
Each cluster-A branch adds one `pub mod X;` and one entry to the
`invoke_handler!` macro list. They'll merge as parallel additions to
the same lines — git will flag textual conflicts because the lines
sit next to each other. Resolution is mechanical: keep all the new
`pub mod` lines (alphabetically), keep all the new handler entries.

### `src-tauri/src/forensic.rs`
Branches 5, 6, 7, 8 each carry the same two-line digit-grouping fix
(`1715600000_000` → `1_715_600_000_000`). Whichever lands first
makes the rest no-ops; expect "already applied" merges.

### `src-tauri/src/disks.rs::start_write`
Only `feat/smart-preflight` touches this. Land it first to avoid
later branches having to rebase around it.

### `src/App.jsx`
Branches 2, 4, 7 add code to the `useEffect` that wires up the
event listeners. They each introduce different `prefsRef` /
`addImageFromPathRef` / `phaseRef` patterns — when integrating, keep
one canonical `prefsRef` declaration and let the listeners share it.
Specifically:
- `feat/audio-cues` adds `phaseRef` and an unlock-on-first-input
  listener; merge before url-fetch + eject.
- `feat/url-fetch` adds `addImageFromPathRef` + 3 download event
  listeners.
- `feat/eject` consumes `prefsRef.current['auto.eject']` inside the
  `job-complete` listener and removes the stale TODO.

If you cherry-pick instead of merging, the order **audio → url →
eject** in cluster B is the cleanest sequence to avoid conflict
churn on `App.jsx`.

### `src/components.jsx`
- `feat/audio-cues` adds `sound.enabled` to PREFS_DEFAULTS and the
  behavior section.
- `feat/url-fetch` adds an `onAddFromUrl` prop to `Toolbar`.
Both are additive at different lines; should merge cleanly.

### `src/i18n/locales/{en,de,es}.json`
Strict key-parity hook is enforced in `.githooks/pre-commit` (see
instance 3's commit `9f5b48b`). Three branches add new keys:
- `feat/audio-cues` adds `prefs.label.sound_enabled`
- `feat/url-fetch` adds `toolbar.from_url` + `url.*` block
- (No others touch i18n.)

All three branches added matching keys to all three locales. As long
as you don't drop a locale on conflict resolution, the parity hook
will pass.

### `src-tauri/Cargo.toml` / `Cargo.lock`
- `feat/watch-folder` adds `notify` (and pulls in its deps).
- `feat/url-fetch` adds `ureq` (with `tls,native-certs`).
- They land in different rows of `[dependencies]` (alphabetical) so
  no textual conflict, but the lock file regenerates on each merge.
  After merging both, run `cargo build` once and commit any updated
  Cargo.lock.
- **Heads-up from main:** the `[package].name` is now `diskcutter`
  (not `disk-cutter`) and there's a `[patch.crates-io]` override
  block at the bottom of Cargo.toml pinning am-img-qcow2 + sibling
  crates to local working trees (needed until am-img-qcow2 0.3
  publishes). Don't strip that block during merge resolution — the
  sparse-qcow2 backup path won't compile without it.

## Frontend wireup still TODO

Five backend features ship without frontend yet. They're useful from
the CLI front-end and Tauri-debug, but have no in-app affordance:

| Feature | Suggested wireup |
|---|---|
| Image catalog | A "Catalog" sheet/tab next to the URL prompt: list distros from `catalog_list()`, click → `start_download` with the entry's `download_url`. ~80 lines of React |
| Watch folder | Settings entry "Watch folder path" (text field). On change, call `watch_folder_start` for the new path. App.jsx subscribes to `disk-cutter://image-found` and routes through `addImageFromPath`. ~40 lines |
| QEMU bootability test | "Boot test" button in JobRow's expanded detail when state == 'success'. Calls `qemu_check` on mount; greys button if unavailable. ~30 lines |
| Doctor | "Run diagnostics" button in Prefs view. Renders the returned `DoctorReport` as a checklist with status pills. ~60 lines |
| SMART chip | Disk picker chips per row showing `smart_check` verdict. (My initial draft of this was overwritten by instance 3's preferred backend-only design — re-add the picker chip if desired.) ~30 lines |

## After merging, clean up the branches

```bash
# Eight feature branches with actual work.
for b in feat/{smart-preflight,audio-cues,watch-folder,url-fetch,image-catalog,qemu-test,eject,doctor}; do
  git branch -D $b
done

# Two phantom branches with 0 commits ahead of main — safe to drop now.
git branch -D feat/forensic-record 2>/dev/null  # never produced a commit
# (No feat/snapshot branch was ever created.)
```

## Tracking the upstream qcow2 work

Separate from the in-repo branches: there's a feature branch in the
sibling project `rust-img-qcow2` that needs publishing before this
repo's `[patch.crates-io]` block can come out:

```bash
cd /Volumes/sdcard256gb/projects/rust-img-qcow2
git log --oneline feat/allocated-extents | head -3
# 85c59e5 feat(reader): allocated_extents API for sparse-aware consumers
```

Steps when ready to release:
1. Merge `feat/allocated-extents` into `rust-img-qcow2`'s `main`.
2. Bump version to `0.3.0` in its `Cargo.toml`.
3. `cargo publish` (or push the version tag — the repo has a
   `release-on-tag` workflow per commit `a9add7f`).
4. In this repo, update the dep in `src-tauri/Cargo.toml` to
   `am-img-qcow2 = "0.3"` and **remove the entire `[patch.crates-io]`
   block** (along with the comment explaining it).
5. `cargo build` once, commit the regenerated Cargo.lock.

## Per-branch verification recipe

If you'd rather merge individually and verify each step:

```bash
git switch main && git pull
git merge --no-ff feat/<one>
cargo test --manifest-path src-tauri/Cargo.toml --lib
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings
cargo fmt --manifest-path src-tauri/Cargo.toml --check
npm test --silent
npm run build
# If anything fails, `git reset --hard HEAD~1` and rebase the branch
# before retrying.
```

## Caveats worth re-reading

- `qemu.rs::test_image` returns "passed" on a heuristic (QEMU
  stayed alive past a 5s warmup). It is not a proof of bootability.
- `doctor.rs::check_full_disk_access` uses TCC.db readability as a
  proxy for FDA — works on macOS shipped under default TCC but
  could give a false-positive if TCC.db is world-readable for some
  unusual reason.
- `catalog.rs` ships hardcoded URLs to specific releases. Bump
  these in the catalog source when distros release new versions; no
  remote refresh is implemented.
- `url_fetch.rs` doesn't verify a downloaded file against any
  bundled SHA256SUMS yet — the GPG-verify lane on the roadmap is
  the natural follow-up that closes that hole.
- **`commands::run_backup`** (added on main as part of `d7218c1`) is
  synchronous — it blocks the Tauri thread for the duration of the
  backup. Fine for small images, but a 100 GiB qcow2 will lock the
  IPC. The natural follow-up is an `async` variant that emits
  `disk-cutter://backup-progress` events the way `start_write` does;
  the worker-pool plumbing is already there to copy from.
- **`[patch.crates-io]` in `src-tauri/Cargo.toml`** is required
  until `am-img-qcow2 0.3` publishes (see the "Tracking the upstream
  qcow2 work" section above). Removing it without publishing will
  break the build of the sparse-qcow2 backup path.
