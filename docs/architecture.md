# Architecture

Developer-facing tour of how Disk Cutter is wired together. Code
references are absolute paths into the repo; line numbers are
approximate and meant as starting points rather than anchors.

## Process model

Two processes, same binary:

```
┌──────────────────────┐    invoke()      ┌────────────────────┐
│ React UI             │ ───────────────▶ │ Tauri main (rust)  │
│ (unprivileged)       │ ◀── events ───── │ (unprivileged)     │
└──────────────────────┘                  └─────────┬──────────┘
                                                    │ spawn via
                                                    │ osascript
                                                    ▼
                                          ┌────────────────────┐
                                          │ disk-cutter         │
                                          │ --helper-burn       │
                                          │ (root, no UI)       │
                                          └─────────┬──────────┘
                                                    │ JSONL
                                                    ▼
                                  /tmp/disk-cutter-progress-<jobid>.jsonl
                                          (tailed by main; relayed to UI)
```

The entry point at `src-tauri/src/main.rs` checks `argv[1]`: if it is
`--helper-burn` the binary becomes the privileged helper and never
touches Tauri; otherwise it boots the regular UI process. `run_helper`
lives in `src-tauri/src/helper.rs`.

The helper performs the actual `pwrite` to the raw device, then a
read-back, hashing both streams as it goes. Each progress update is one
JSON object per line, written to the progress file at the path the main
process passed in `--progress=`. The main process tails that file and
turns every line into a `disk-cutter://job-update`, `://job-complete`
or `://job-error` Tauri event.

This split keeps the GUI process unprivileged. The user is prompted for
their password exactly once per burn (osascript caches credentials in
the agent for the cookie window). If the helper fails to spawn or the
user cancels the auth sheet, the main app receives `EHELPER` /
`EAUTH` and surfaces them in the queue row.

Key files:
- `src-tauri/src/main.rs` — argv dispatch.
- `src-tauri/src/helper.rs` — privileged burn driver, JSONL emitter.
- `src-tauri/src/disks.rs` — `start_write`, osascript spawn, progress
  tail, event re-emit.

## DiskArbitration session

`src-tauri/src/disk_arb.rs` hand-rolls FFI bindings to
`DiskArbitration.framework`. We unmount the target via `DADiskUnmount`
*inside* a session that has also registered a
`DARegisterDiskMountApprovalCallback` — and we hold both for the
lifetime of the burn.

Why not `diskutil unmountDisk`? Because `diskutil` exits after the
unmount call returns, which closes its DiskArbitration session, which
means there is nothing left to dissent the next mount request. On
modern macOS, Spotlight, Time Machine, or just `diskarbitrationd`'s
own retry logic will try to re-attach the volume within
milliseconds. By the time we `open(2)` the raw device, we're racing a
remount and frequently lose to `EBUSY`. Etcher hit the same wall and
solved it the same way (it's the reason Etcher links against
DiskArbitration directly rather than shelling out to `diskutil`).

`DiskClaim::for_dev` returns an RAII handle. The handle owns a
background thread running a `CFRunLoop`; the thread schedules the
session on its runloop, registers the approval callback, calls
`DADiskUnmount`, and parks. On `Drop` we `CFRunLoopStop` so the thread
unwinds the session cleanly. We don't `join()` the thread on drop
because the helper exits the process immediately afterwards and the
kernel reaps everything; joining could deadlock if the runloop is in a
state `CFRunLoopStop` cannot wake (see `disk_arb.rs:280`).

The approval callback (`mount_approval_cb`) dissents any mount whose
BSD name matches the whole-disk prefix or any of its slices
(`disk5`, `disk5s1`, …). Other disks are passed through.

## DeviceIo trait

`src-tauri/src/writers/mod.rs` defines:

```rust
pub trait DeviceIo: Send + Sync {
    fn name(&self) -> &'static str;
    fn open_write(&self, device: &Path) -> Result<Box<dyn DeviceWriter>>;
    fn open_read(&self, device: &Path)  -> Result<Box<dyn DeviceReader>>;
}
```

Three impls live alongside each other:

| Impl                  | File                                    | Path translation     | Notes |
|-----------------------|------------------------------------------|-----------------------|-------|
| `RawDeviceIo`         | `src-tauri/src/writers/raw.rs`           | `/dev/diskN` → `/dev/rdiskN` | Single-threaded `write_all` to the char device. `O_EXLOCK` + `F_NOCACHE` on macOS. Simple, correct, slower in practice than `pipelined`. |
| `BlockDeviceIo`       | `src-tauri/src/writers/block.rs`         | `/dev/rdiskN` → `/dev/diskN` | Buffered block-device path. Slower than `raw` on our hardware; kept as an experiment knob. |
| `PipelinedRawDeviceIo`| `src-tauri/src/writers/pipelined.rs`     | `/dev/diskN` → `/dev/rdiskN` | Worker pool (`worker_threads` × N), `pwrite` at offset (no shared file cursor), `F_NOCACHE`. The producer hands 1 MiB buffers through an `mpsc::sync_channel(queue_depth)`. Empirically ~5× the throughput of `RawDeviceIo` on USB-MSC sticks because the USB driver queue stays full. |
| `PlainFileDeviceIo`   | `src-tauri/src/writers/plain.rs`         | none                  | Fallback for non-`/dev/` targets (test fixtures, debug runs). |

Selection is runtime, driven by the SQLite config key `writer.impl`.
The main process reads the key in `start_write`
(`src-tauri/src/disks.rs:680`) and passes it to the helper via
`--writer=raw|block|pipelined`. The helper resolves it in
`helper::pick_device_io`. Order of precedence in the helper:

1. `--writer=` CLI argument
2. `DISKCUTTER_WRITER` env var
3. Default → `pipelined` for `/dev/`, `plain` otherwise.

Unknown values log a warning and fall back to `pipelined`.

The pipelined writer also accepts runtime tuning via
`--workers=` and `--queue-depth=` (default 4 × 15). Both are persisted
in `config` and editable in the Prefs view.

## Burn / verify pipeline

`src-tauri/src/pipeline.rs` exposes three functions; the helper calls
them in sequence.

- **`burn(reader, writer, chunk_size, cancel, on_progress)`** —
  streams the source image through a SHA-256 hasher while it writes,
  so the source hash is "free" by the time the burn finishes. Returns
  `BurnResult { bytes_written, source_sha256, elapsed,
  avg_bytes_per_sec }`. Chunk default 1 MiB (`DEFAULT_CHUNK`).
- **`verify_hash_only(device_reader, expected_bytes, …)`** —
  re-reads only the device. Hashes as it goes and returns the readback
  SHA-256. This is the fast path: in the common case where the burn
  was correct, we never have to re-read the source. The helper
  compares `fast.readback_sha256 == burn.source_sha256` and ships
  `verify_match: true` to the UI in one event.
- **`verify(source, device, …)`** — the slow byte-compare fallback.
  Re-opens the source image, re-opens the device, walks both,
  collecting the first 256 mismatches with LBA, byte offset,
  expected, actual. Only entered when `verify_hash_only` returned a
  hash that disagreed with the burn-side source hash.

The split matters because read-back from the device is the dominant
cost of the whole flow (USB read speed × image size). Doing it once
when everything is fine saves halving throughput; doing it twice when
something is wrong is rare enough that the simplicity is worth it.

Cancellation is a single `AtomicBool` reference threaded through all
three functions; they check it once per chunk.

## Config + history DB

`src-tauri/src/db/mod.rs` opens a SQLite file at
`app.path().app_data_dir() / "disk-cutter.sqlite"`, WAL mode, foreign
keys on, schema applied by `db::migrations::run`. Tables:

- `config` — flat key/value. Used for `writer.impl`,
  `pipeline.chunk_size`, `pipeline.workers`, `pipeline.queue_depth`,
  `language`, theme prefs, etc. Two Tauri commands:
  `config_get(key)` / `config_set(key, value)` /
  `config_all() → HashMap`.
- `burn_jobs` — one row per job. Lifecycle: insert at `running`,
  update to `complete` or `failed` when the helper finishes. Drives
  the Sidebar counts in the UI.
- `burn_mismatches` — child table of `burn_jobs`. Populated only when
  `verify` enters the slow path; up to 256 rows per job (LBA, offset,
  expected, actual hex strings).
- `burn_logs` — append-only log rows per job. Used by the Logs view in
  the UI.

Access goes through `Db(Mutex<Connection>)` managed in Tauri state.
Frontend pages query through `burn_history_list` / `burn_logs_list` /
`burn_history_clear` commands.

The whole module is plain `rusqlite` (bundled feature) — no ORM, no
async, no `tauri-plugin-sql`. (Earlier drafts considered the plugin;
the bundled-rusqlite path won out because the schema is so small that
the plugin's runtime cost wasn't justified, and migrations stay
embedded next to the code that consumes them.)

## Frontend state

`src/App.jsx` is the single source of truth for everything queue-
related. The shape that matters is `jobs: Job[]`. Three handlers
mutate it:

- `listen('disk-cutter://job-update', …)` → `applyJobUpdate(jobs, payload)`
  (bytes, bytes/sec, state transition: `queued → writing → verifying`).
- `listen('disk-cutter://job-complete', …)` → `applyJobComplete(jobs, payload)`
  (final hashes, mismatches, elapsed, success/failure).
- `listen('disk-cutter://job-error', …)` → `applyJobFailure(jobs, payload)`
  (terminal failure with an `error_code`).

The reducers live in `src/job-reducers.js` as pure functions so they
can be unit-tested without React. Derivations (scene name, session
stats, plan-start gating) are pure functions in `src/app-derive.js`.

The Sidebar provides three nav targets: `queue`, `logs`, `prefs`. The
Prefs view (`PrefsView` in `src/components.jsx`) writes through
`config_set` on every change so the backend reads the live value on
the next burn (no app restart needed). Theme, density, accent, and the
verbose-title toggle persist to `localStorage` because they need to be
applied before React mounts — see `useTweaks` in
`src/tweaks-panel.jsx`.

`PREFS_DEFAULTS` mirrors the keys backend code expects. Empty strings
from the DB are coerced to defaults at hydration so the UI never
renders an empty `<select>`.

## i18n

`src/i18n/index.js` uses `react-i18next` plus an `import.meta.glob`
trick:

```js
const modules = import.meta.glob('./locales/*.json', { eager: true });
```

Vite walks the directory at build time and inlines every catalog.
Adding a language is one step: drop `src/i18n/locales/<code>.json`
with a top-level `"language": { "name": "Deutsch" }` block. It appears
in the language picker, sorted alphabetically by its native name.

Initial language selection at boot:

1. `localStorage['diskcutter.language']` (warm cache so the first
   paint isn't English-then-flash).
2. `navigator.language` primary tag, then short tag.
3. `en`.
4. First available catalog.

After mount, the SQLite config key `language` is read via
`config_get`; if it differs from the warm-cache pick, `changeLanguage`
fires. From then on every `languageChanged` event writes both
locations.

Current catalogs: `en.json`, `de.json`, `es.json`.

## Pre-commit hook

`.githooks/pre-commit` runs the two checks CI enforces:

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
```

Both run against `src-tauri/`. The hook is opt-in per clone:

```sh
git config core.hooksPath .githooks
# or
./scripts/install-hooks.sh
```

`--no-verify` skips the hook for WIP commits; CI catches anything
that slips through.

## Where things live

```
src-tauri/
├── src/
│   ├── main.rs              # argv dispatch (UI vs --helper-burn)
│   ├── lib.rs               # Tauri command registry + setup
│   ├── helper.rs            # privileged burn driver, JSONL emitter
│   ├── pipeline.rs          # burn + verify_hash_only + verify
│   ├── disks.rs             # disk enumeration, start_write, event relay
│   ├── disk_arb.rs          # DiskArbitration FFI + DiskClaim RAII
│   ├── readers/             # ImageReader trait + raw/gzip/xz/qcow2
│   ├── writers/             # DeviceIo trait + raw/block/pipelined/plain
│   └── db/                  # rusqlite open + migrations + commands
└── migrations/0001_initial.sql

src/
├── App.jsx                  # job queue state, event listeners, scene
├── components.jsx           # WindowChrome, Sidebar, JobRow, PrefsView, …
├── job-reducers.js          # pure mutators for jobs[]
├── app-derive.js            # pure derivations (scene, stats, plan-start)
├── format.js                # bytes/duration/speed formatters
├── tweaks-panel.jsx         # useTweaks hook + dev tweak panel
└── i18n/
    ├── index.js             # auto-discovery + react-i18next init
    └── locales/{en,de,es}.json
```
