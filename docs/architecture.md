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
                                                    │ osascript / sudo
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

`src-tauri/src/main.rs` dispatches on `argv[1]`:

- `--helper-burn` → privileged helper (`helper::run_helper`); never
  initializes Tauri.
- `help | -h | --help | version | -v | --version | formats | inspect |
  backup | snapshot | restore | doctor` → headless CLI
  (`cli::run_cli`); shares the same pipeline as the GUI.
- Anything else (including no args) → GUI.

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
- [src-tauri/src/main.rs](../src-tauri/src/main.rs) — argv dispatch.
- [src-tauri/src/helper.rs](../src-tauri/src/helper.rs) — privileged burn driver, JSONL emitter.
- [src-tauri/src/cli.rs](../src-tauri/src/cli.rs) — headless subcommand runner.
- [src-tauri/src/disks.rs](../src-tauri/src/disks.rs) — `start_write`, osascript spawn, progress tail, event re-emit.

## DiskArbitration session (macOS)

[src-tauri/src/disk_arb.rs](../src-tauri/src/disk_arb.rs) hand-rolls FFI bindings to
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
state `CFRunLoopStop` cannot wake.

The approval callback dissents any mount whose BSD name matches the
whole-disk prefix or any of its slices (`disk5`, `disk5s1`, …).
Other disks are passed through.

## Source layer — decoder chain + container dispatch

[src-tauri/src/source.rs](../src-tauri/src/source.rs) is the single front door for
"give me a streaming reader for this file". Two flavours of image
co-exist:

1. **Compressed or raw** (xz / gz / bz2 / zstd / plain) — streaming-only;
   the [decoder_chain](../src-tauri/src/decoder_chain/) module peels compression layers
   one at a time and yields raw bytes.
2. **Container** (qcow2 / vhd / vhdx / vmdk) — random-access metadata;
   upstream crates expose `BlockRead`, which we wrap in
   `BlockReadStreamer` (from `am-fs-core`) for the streaming Read
   interface the burn pipeline wants.

Dispatch is **extension first, magic second** — the magic-number fallback
handles the "user renamed `foo.iso.xz` to `foo.iso`" footgun.

The decoder chain is registry-driven: each compression format is a unit
struct implementing `FormatTryOpen`, registered in a static `READERS`
slice. `identify_data_stream` walks the registry, asks each format to
`try_open` (peek + rewind), and on the first match wraps the source and
recurses. The chain stops when no format matches.

Adding a new compression format is mechanical: implement `FormatTryOpen`,
add a `&FOO_FACTORY` entry to `READERS`. See
[decoder_chain/mod.rs](../src-tauri/src/decoder_chain/mod.rs) for the full how-to.

The legacy [readers/mod.rs](../src-tauri/src/readers/mod.rs) is now a stub holding
only the magic-number sniff helpers — the old `ImageReaderRegistry` +
per-format factory layer was removed once `source.rs` became the single
front door.

## DeviceIo trait

[src-tauri/src/writers/mod.rs](../src-tauri/src/writers/mod.rs) defines:

```rust
pub trait DeviceIo: Send + Sync {
    fn name(&self) -> &'static str;
    fn open_write(&self, device: &Path) -> Result<Box<dyn DeviceWriter>>;
    fn open_read(&self, device: &Path)  -> Result<Box<dyn DeviceReader>>;
}
```

Four impls live alongside each other:

| Impl                  | File                                    | Platform | Path translation     | Notes |
|-----------------------|------------------------------------------|----------|-----------------------|-------|
| `RawDeviceIo`         | [writers/raw.rs](../src-tauri/src/writers/raw.rs)           | Unix     | `/dev/diskN` → `/dev/rdiskN` | Single-threaded `write_all` to the char device. `O_EXLOCK` + `F_NOCACHE` on macOS. Simple, correct, slower in practice than `pipelined`. |
| `BlockDeviceIo`       | [writers/block.rs](../src-tauri/src/writers/block.rs)         | Unix     | `/dev/rdiskN` → `/dev/diskN` | Buffered block-device path. Slower than `raw` on our hardware; kept as an experiment knob. |
| `PipelinedRawDeviceIo`| [writers/pipelined.rs](../src-tauri/src/writers/pipelined.rs) | Unix     | `/dev/diskN` → `/dev/rdiskN` | Worker pool (`worker_threads` × N), `pwrite` at offset (no shared file cursor), `F_NOCACHE`. Producer hands 1 MiB buffers through an `mpsc::sync_channel(queue_depth)`. Empirically ~5× the throughput of `RawDeviceIo` on USB-MSC sticks because the USB driver queue stays full. |
| `PlainFileDeviceIo`   | [writers/plain.rs](../src-tauri/src/writers/plain.rs)         | all      | none                  | Fallback for non-`/dev/` targets (test fixtures, debug runs). |

Selection is runtime, driven by the SQLite config key `writer.impl`.
The main process reads the key in `start_write` and passes it to the
helper via `--writer=raw|block|pipelined`. The helper resolves it in
`helper::pick_device_io`. Order of precedence in the helper:

1. `--writer=` CLI argument
2. `DISKCUTTER_WRITER` env var
3. Default → `pipelined` for `/dev/`, `plain` otherwise.

Unknown values log a warning and fall back to `pipelined`.

The pipelined writer also accepts runtime tuning via `--workers=` and
`--queue-depth=` (default 4 workers × 16 in-flight chunks). Both are
persisted in `config` and editable in the Prefs view.

## Burn / verify pipeline

[src-tauri/src/pipeline.rs](../src-tauri/src/pipeline.rs) exposes three logical
operations, each with a `…_with_hash` variant that takes a selectable
`HashAlgo` ([hash.rs](../src-tauri/src/hash.rs): SHA-256 or xxh64). The
plain entry points (`burn`, `verify_hash_only`, `verify`) default to
SHA-256 for back-compat; the `_with_hash` variants are what the helper
calls in practice.

- **`burn(reader, writer, chunk_size, cancel, on_progress)`** —
  streams the source image through the hasher while it writes, so the
  source hash is "free" by the time the burn finishes. Returns
  `BurnResult { bytes_written, source_hash, elapsed, avg_bytes_per_sec }`.
  Chunk default 1 MiB (`DEFAULT_CHUNK`).
- **`verify_hash_only(device_reader, expected_bytes, …)`** —
  re-reads only the device, hashes as it goes, returns the readback
  hash. Fast path: in the common case where the burn was correct we
  never have to re-read the source. The helper compares
  `readback_hash == source_hash` and ships `verify_match: true` to the
  UI in one event.
- **`verify(source, device, …)`** — slow byte-compare fallback.
  Re-opens both, walks them, collects the first 256 mismatches with
  LBA, byte offset, expected, actual. Only entered when
  `verify_hash_only` returned a hash that disagreed with the burn-side
  source hash.

Read-back is the dominant cost of the whole flow (USB read speed ×
image size). Doing it once when everything is fine saves halving
throughput; doing it twice when something is wrong is rare enough that
the simplicity is worth it.

Cancellation is a single `AtomicBool` reference threaded through all
three functions; they check it once per chunk.

## Config + history DB

[src-tauri/src/db/mod.rs](../src-tauri/src/db/mod.rs) opens a SQLite file at
`app.path().app_data_dir() / "disk-cutter.sqlite"`, WAL mode, foreign
keys on, schema applied by `db::migrations::run`. Five migrations as
of this writing — see [src-tauri/migrations/](../src-tauri/migrations/).

Tables:

- **`config`** — flat key/value. Used for `writer.impl`,
  `pipeline.chunk_size`, `pipeline.workers`, `pipeline.queue_depth`,
  `hash.algo`, `language`, theme prefs, etc. Commands:
  `config_get(key)` / `config_set(key, value)` / `config_all()`.
- **`burn_jobs`** — one row per job. Integer PK (mig 0005). Lifecycle:
  insert at `queued`, transition through `running` to `complete` or
  `failed`. Source of truth for both the live queue and burn history.
- **`burn_mismatches`** — child of `burn_jobs`. Populated only when
  `verify` enters the slow path; up to 256 rows per job (LBA, offset,
  expected, actual hex).
- **`burn_logs`** — append-only log rows per job, FK `job_id`. Drives
  the Logs view.
- **`image_scans`** — cache of per-image scan results (partition table,
  filesystem labels, format chain, boot sources) keyed by
  `(image_path, file_size, file_mtime)`. Lets adding the same image to
  10 burn_jobs trigger only one decompression pass. See
  [migrations/0003](../src-tauri/migrations/0003_image_scans.sql).

Access goes through `Db(Mutex<Connection>)` managed in Tauri state.
The single global Mutex is a known footgun — `Mutex::unwrap()` on
poison would crash the app ([DC-1](http://taskhauler.localhost/DC/1) on
the backlog). Frontend pages query through `burn_history_list` /
`burn_logs_list` / `burn_history_clear` commands.

Plain `rusqlite` (bundled feature) — no ORM, no async, no
`tauri-plugin-sql`. The plugin's runtime cost wasn't justified for a
schema this small, and migrations stay embedded next to the code that
consumes them.

## Frontend state

[src/App.jsx](../src/App.jsx) is the single source of truth for everything
queue-related. The shape that matters is `jobs: Job[]`. Three handlers
mutate it:

- `listen('disk-cutter://job-update', …)` → `applyJobUpdate(jobs, payload)`
  (bytes, bytes/sec, state transition: `queued → writing → verifying`).
- `listen('disk-cutter://job-complete', …)` → `applyJobComplete(jobs, payload)`
  (final hashes, mismatches, elapsed, success/failure).
- `listen('disk-cutter://job-error', …)` → `applyJobFailure(jobs, payload)`
  (terminal failure with an `error_code`).

The reducers live in [src/job-reducers.js](../src/job-reducers.js) as pure
functions so they can be unit-tested without React. Derivations (scene
name, session stats, plan-start gating) are pure functions in
[src/app-derive.js](../src/app-derive.js). Queue persistence lives in
[src/store/queue.js](../src/store/queue.js).

The Sidebar provides three nav targets: `queue`, `logs`, `prefs`. The
Prefs view writes through `config_set` on every change so the backend
reads the live value on the next burn (no app restart needed). Theme,
density, accent, and the verbose-title toggle persist to
`localStorage` because they need to be applied before React mounts —
see `useTweaks` in [src/tweaks-panel.jsx](../src/tweaks-panel.jsx).

`PREFS_DEFAULTS` mirrors the keys backend code expects. Empty strings
from the DB are coerced to defaults at hydration so the UI never
renders an empty `<select>`.

## i18n

[src/i18n/index.js](../src/i18n/index.js) uses `react-i18next` plus an
`import.meta.glob` trick:

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

Current catalogs: `en.json`, `de.json`, `es.json` — 339 keys each,
parity enforced by the pre-commit hook.

## Pre-commit hook

[.githooks/pre-commit](../.githooks/pre-commit) runs four checks:

```sh
# 1. linear-history guard — no merge commits
git rev-parse -q --verify MERGE_HEAD            # reject in-progress merge
git log --merges HEAD --format=%H               # reject historical merges

# 2. i18n key parity across all locales
node scripts/check-i18n.mjs

# 3. rustfmt
cargo fmt --check

# 4. clippy
cargo clippy --all-targets -- -D warnings
```

Both cargo steps run against `src-tauri/`. The hook is opt-in per
clone:

```sh
git config core.hooksPath .githooks
# or
./scripts/install-hooks.sh
```

The hook is load-bearing — it's the guardrail against pushing broken
code or merge commits (the project mandates linear history). Don't
weaken it; if it flags something, fix the underlying issue.

## Where things live

```
src-tauri/
├── src/
│   ├── main.rs              # argv dispatch (UI / --helper-burn / CLI)
│   ├── lib.rs               # Tauri command registry + setup
│   ├── helper.rs            # privileged burn driver, JSONL emitter
│   ├── cli.rs               # headless subcommand runner
│   ├── commands.rs          # Tauri command surface (inspect, backup, …)
│   ├── pipeline.rs          # burn + verify_hash_only + verify
│   ├── hash.rs              # selectable streaming hashers (sha256, xxh64)
│   ├── disks.rs             # disk enumeration, start_write, event relay
│   ├── disk_arb.rs          # DiskArbitration FFI + DiskClaim RAII (macOS)
│   ├── source.rs            # single front-door: probe + open_streaming
│   ├── decoder_chain/       # recursive compressed-image decoder
│   │   ├── identify.rs      # registry walker
│   │   ├── interface.rs     # ReaderInterface trait
│   │   ├── raw.rs           # leaf reader
│   │   └── formats/         # xz, gzip, bzip2, zstd
│   ├── readers/             # legacy stub — magic-number sniffs only
│   ├── writers/             # DeviceIo trait + raw/block/pipelined/plain
│   ├── image.rs             # image-add + size resolution
│   ├── image_scan.rs        # cached deep scan, populates image_scans
│   ├── inspect.rs           # partition / FS-label probe
│   ├── validate.rs          # source/target validation gates
│   ├── catalog.rs           # curated distro list + remote refresh
│   ├── url_fetch.rs         # streaming download + sha256 (no verify yet)
│   ├── backup.rs            # disk → image (qcow2 / raw / sparse)
│   ├── snapshot.rs          # snapshot/restore subcommands
│   ├── sparse.rs            # sparse-file helpers
│   ├── doctor.rs            # environment / FDA self-check
│   ├── qemu.rs              # post-burn bootability test
│   ├── forensic.rs          # tamper-evident burn-record export
│   ├── joblog.rs            # structured per-job logger
│   ├── xz_footer.rs         # parses xz footer index for true size
│   └── db/                  # rusqlite open + migrations + commands
└── migrations/
    ├── 0001_initial.sql
    ├── 0002_burn_jobs.sql
    ├── 0003_image_scans.sql
    ├── 0004_burn_jobs_unique_job_id.sql
    └── 0005_integer_primary_keys.sql

src/
├── main.jsx                 # React entry
├── App.jsx                  # job queue state, event listeners, scene
├── components.jsx           # WindowChrome, Sidebar, JobRow, PrefsView, …
├── job-reducers.js          # pure mutators for jobs[]
├── app-derive.js            # pure derivations (scene, stats, plan-start)
├── format.js                # bytes/duration/speed formatters
├── keymap.js                # keyboard shortcuts
├── toast.js                 # transient notifications
├── tweaks-panel.jsx         # useTweaks hook + dev tweak panel
├── hooks/useFda.js          # Full-Disk-Access status hook (macOS)
├── store/queue.js           # queue persistence
└── i18n/
    ├── index.js             # auto-discovery + react-i18next init
    └── locales/{en,de,es}.json   # 339 keys each
```
