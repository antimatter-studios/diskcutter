# Burn queue — design notes

Status: not started. `feat/burn-queue` branch existed but never landed real
work; it was deleted during the linearization sweep. This doc is the starting
point for picking it up later.

## The idea

Operator burns N USB sticks in a session. Today the workflow is: add image,
pick target, start, wait, eject, insert next stick, repeat. The queue
already exists as a UI concept (`jobs[]` in `App.jsx`) but it's flat: one
row per (image, target) pair, no notion of "I want 20 copies of this
image."

Burn queue reshapes it: each queue entry carries a **copies-remaining
counter**. The operator dispatches one copy per inserted stick. When a
burn finishes, the counter decrements; the entry stays in the queue until
zero, then drops out.

This makes bulk burns first-class without forcing parallelism. Parallel
multi-target ("flash 4 sticks at once") is a follow-up that just dispatches
N at a time against the same entry.

## Why this shape (and what we explicitly rejected)

The load-bearing constraint: **the operator is always at the machine**.
Burning requires physical media insertion. There is no "drop and walk
away" workflow that's worth building.

Rejected alternatives, with reasons:

- **Watch folder / auto-queue from a directory** (was landed on
  `feat/watch-folder` as `notify`-based `watch_folder_{start,stop,status}`
  commands; explicitly de-prioritized after pushback). Removes the
  operator. Operator is already there.
- **Unattended re-burn loop**. Same problem.
- **Auto-detect on disk insertion → start burn**. Same problem; also
  destructive-by-default, which is exactly the safety stance the app is
  built to reject.

The endorsed direction is **queue + counter + per-dispatch operator
choice**. It accepts the human-in-the-loop constraint and optimizes the
operator's flow instead of fighting it. Subsumes "Bulk parallel burns"
from `ROADMAP.md` cleanly — parallel becomes "dispatch K from this entry
in parallel" rather than its own feature.

## Caveats not to write down

Don't add tradeoffs the operator already knows from doing the job (e.g.
"copies counter assumes identical-image bulk" — yes, that's the use
case). These came up before and got pushback.

## Current code surface

### Frontend

`src/App.jsx:58` — `const [jobs, setJobs] = useState([]);`

Job shape (`src/format.js:32` `makeJob`):

```js
{
  id: `job-${Date.now()}-${num}`,
  num, image, target,
  state: 'idle' | 'writing' | 'verifying' | 'success' | 'error',
  progress, verifyProgress,
  speed, eta, elapsed,
  errorCode, errorMessage,
  verification,
}
```

Job is created via `setJobs((js) => [...js, makeJob(js.length + 1, image, null)])`
in two places (`addImageFromPath`, `retryJob`). Lifecycle events
(`applyJobUpdate`, `applyJobComplete`, `applyJobFailure`) come from the
Rust backend and mutate the job in place.

Counts the UI cares about today (`src/components.jsx:81–84`):

```js
counts.queue = jobs.length
failedCount  = jobs.filter(j => j.state === 'error').length
```

`queueReady(jobs, confirmed)` (in `src/keymap.js`) gates the start
shortcut. There is also a `queue.depth` config in preferences (default
15) which is **unrelated** — it's the writer's internal pipelined-write
queue depth, not the user-facing burn queue.

### Backend

No queue model in Rust. Each `start_write` is a separate IPC call.
Burns are recorded one-at-a-time in `burn_history` (rusqlite, see
`src-tauri/src/db/migrations.rs`); there is no `burn_queue` table.

`burn_history` is the right reference for what's been done, but it's
write-once-on-completion and rowid-per-burn. It is **not** the queue
state and shouldn't be reused as one.

## What "implement burn queue" actually means

Two layers of change. They can land separately.

### Layer 1 — UI: per-image counter

Smallest viable shape, no persistence, no schema change. Goal: operator
adds one queue entry per image, gives it a copies-remaining count,
dispatches one burn at a time, counter decrements.

Concrete changes:

1. Introduce `entries[]` alongside `jobs[]`. Each entry:
   ```js
   {
     id, image, copiesRemaining, copiesGoal,
     // optional: per-entry target hint (operator can override per dispatch)
   }
   ```
2. UI: queue view shows entries with `n / N` remaining. "Dispatch next"
   button on each entry that takes a target and creates a single `job`
   from `makeJob` (existing flow). On `applyJobComplete` for a job
   sourced from an entry, decrement the entry's `copiesRemaining`. When
   it hits 0, mark entry done (or auto-remove — operator preference).
3. Decide: do `jobs[]` and `entries[]` stay separate, or do jobs get a
   `parentEntryId` and the entries view derives state from jobs? Most
   reversible: keep them separate, add `parentEntryId` to `makeJob` as
   an optional field. Avoids forcing a refactor of every place that
   reads `jobs[]`.
4. Toolbar: "ADD IMAGE" stays. New input: copies count (default 1, so
   today's behavior is preserved when count=1). Adding image with
   count=1 creates one entry with `copiesGoal=1` — UI can collapse this
   visually to the existing "single burn" row.

The dispatch model that keeps things simple: dispatching a copy from
an entry is just `makeJob(..., entry.image, target)` with
`parentEntryId = entry.id`. Existing `start_write` / event plumbing is
untouched.

### Layer 2 — Persistence: queue survives restart

Today `jobs[]` is ephemeral. So `entries[]` would be too. Persistence
is a separate decision because the SQLite layer is already wired
(`config`, `burn_history`, `burn_logs`).

Add a `burn_queue` table (proposal):

```sql
CREATE TABLE burn_queue (
  id INTEGER PRIMARY KEY,
  image_path TEXT NOT NULL,
  image_bytes INTEGER NOT NULL,
  copies_goal INTEGER NOT NULL,
  copies_done INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL,
  -- nullable: operator can set a preferred target hint that the UI
  -- pre-selects on dispatch, but per-dispatch target is still chosen
  -- at burn time. Not a foreign key — devices are not persistent.
  target_hint TEXT
);
```

Tauri commands needed (mirror the `burn_history_*` pattern):
- `burn_queue_list`
- `burn_queue_add(image_path, copies_goal, target_hint?)`
- `burn_queue_increment_done(id)` — called on burn success
- `burn_queue_remove(id)`
- maybe `burn_queue_set_copies_goal(id, n)` for mid-flight edits

Wire `applyJobComplete` to call `burn_queue_increment_done(parentEntryId)`
when present. When `copies_done >= copies_goal`, the entry is "done" —
keep it visible briefly or auto-remove, operator preference (config
key, default auto-remove).

The reason persistence matters: image-catalog downloads + queueing is
becoming a multi-step session. If the app restarts (or crashes — and
the `db::open` error path explicitly continues without persistence)
mid-batch, the operator loses their place. Burn history survives
because we already persist it; queue should too, for parity.

## Open decisions for when this lands

- **Is `copies_goal` mutable mid-flight?** If yes, what happens if the
  operator lowers it below `copies_done`? Probably: clamp `copies_goal`
  to `>= copies_done`, treat as "done after current dispatches."
- **Auto-remove on completion, or keep in a "completed entries" tray?**
  Default to auto-remove; completed burns are already in burn_history,
  no need to double-track.
- **Per-dispatch target memory.** Should the UI remember the last target
  used for a given entry as the default for next dispatch? Probably
  yes — operator likely cycles through identical sticks.
- **Parallel dispatch from one entry.** When this lands, the UI needs
  a "dispatch K at once" affordance. The `start_write` command and job
  events already support concurrent jobs; the only thing missing is
  the entry → multiple-jobs fan-out and counter semantics (decrement
  K once on dispatch, or one-at-a-time as each completes? — the latter
  matches reality better: if one fails, counter only decrements for
  the K-1 successes).

## Integration points to remember

- `src/App.jsx` — `setJobs` callers (lines 280, 454) are the spots
  where dispatch hooks in.
- `src/App.jsx:208/214/224` — job-event handlers; complete handler is
  where `burn_queue_increment_done` calls in.
- `src/format.js:32` — `makeJob`; add optional `parentEntryId`.
- `src-tauri/src/db/migrations.rs` — schema lives here, follow the
  existing `burn_history` / `burn_logs` patterns.
- `src-tauri/src/lib.rs` invoke_handler list — register the new
  `burn_queue_*` commands.
- `src/components.jsx` — Toolbar (counts), Sidebar (queue label/count),
  queue view rendering.

## What to ignore from the deleted branch

`feat/burn-queue` and its tip `b759cbc` contained zero burn-queue code —
all 13 commits were pre-linearization duplicates of catalog / url-fetch /
doctor work that's now on main. Don't try to recover anything from those
SHAs; there is nothing there. The name `b759cbc` does not need to appear
in any future commit message.
