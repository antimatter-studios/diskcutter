# Human-code report — 2026-06-10

**Scope:** the cloud-materialize work — `src-tauri/src/cloud.rs` and the
`spawn_materialize_then_burn` / `begin_burn` paths in `src-tauri/src/disks.rs`.

**Counts:** 6 items found, 6 fixed, 0 skipped. No functional bugs found — these
are readability/maintainability improvements. All changes are behaviour-
preserving and guarded by the existing 491-test suite.

---

## Changes made

### H1 — Module doc described the old serial design (comment that lies)

- **File:** [src-tauri/src/cloud.rs](../src-tauri/src/cloud.rs)
- **What changed:**

  _Before:_
  ```rust
  //! ... a single sequential read-through triggers an orderly fault-in ...
  ```
  _After:_
  ```rust
  //! ... a pool of workers reads every byte (discarding it) to fault the file
  //! fully in ... The workers pull fixed-size blocks off a shared cursor that
  //! advances start→end and retry transient provider timeouts ...
  ```
- **Why it's better:** the module is the first thing a reader sees, and it was
  describing a design that no longer exists (the code became a parallel
  work-queue with retries). The doc now matches the implementation, so a reader
  isn't actively misled about the core mechanism.

### M1 — `materialize` was a god function

- **File:** [src-tauri/src/cloud.rs](../src-tauri/src/cloud.rs)
- **What changed:** the ~70-line body that built five `Arc`s, spawned the
  worker pool inline, ran the monitor loop, joined, and reconciled
  cancel-vs-error is now orchestration over named pieces:
  ```rust
  let state = Arc::new(MaterializeState { cursor, done, stop, live, first_err });
  let handles = spawn_fault_workers(path, len, workers, &state);
  run_progress_monitor(&state, &mut on_progress, &should_cancel);
  for handle in handles { let _ = handle.join(); }
  ```
  The five coordination atomics are bundled into a documented `MaterializeState`
  struct; `spawn_fault_workers` and `run_progress_monitor` hold the thread and
  loop bodies respectively.
- **Why it's better:** `materialize` now reads top-to-bottom as "build state →
  spawn → monitor → join → report," each step a named noun/verb. The struct
  also names what each atomic is *for* (e.g. `live` = workers still running),
  which previously a reader had to infer. Bundling them avoids an 8-argument
  helper.

### L1 — Magic `250` and overflow-coupled shift in the backoff

- **File:** [src-tauri/src/cloud.rs](../src-tauri/src/cloud.rs)
- **What changed:**

  _Before:_
  ```rust
  let backoff = Duration::from_millis(250u64 << (attempt - 1));
  std::thread::sleep(backoff.min(MATERIALIZE_BACKOFF_CAP));
  ```
  _After:_
  ```rust
  const MATERIALIZE_BACKOFF_BASE: Duration = Duration::from_millis(250);
  ...
  let factor = 1u32.checked_shl(attempt - 1).unwrap_or(u32::MAX);
  let backoff = MATERIALIZE_BACKOFF_BASE
      .checked_mul(factor)
      .unwrap_or(MATERIALIZE_BACKOFF_CAP)
      .min(MATERIALIZE_BACKOFF_CAP);
  ```
- **Why it's better:** the base delay now has a name, and the `checked_*` calls
  make the exponential growth obviously safe even if `MATERIALIZE_BLOCK_ATTEMPTS`
  is later raised past the point where `<<` would overflow — the previous form
  was a latent panic coupled to an unrelated constant.

### M2 — Duplicated config lookup

- **File:** [src-tauri/src/disks.rs](../src-tauri/src/disks.rs)
- **What changed:** the `try_state::<Db>()` + `SELECT value FROM config WHERE
  key = ?1` block was copy-pasted in `spawn_materialize_then_burn` and as a
  closure in `begin_burn`. Both now go through one helper:
  ```rust
  fn config_value(app: &AppHandle, key: &str) -> Option<String> { ... }
  ```
  `begin_burn`'s local `read_config` closure became a one-line forwarder
  (`|key| config_value(&app, key)`), so its seven call sites are untouched.
- **Why it's better:** the SQL and the not-yet-initialised-DB handling live in
  exactly one place. A change to how config is read (or the query) no longer has
  to be made in two spots that could drift.

### M3 — `spawn_materialize_then_burn` mixed concerns

- **File:** [src-tauri/src/disks.rs](../src-tauri/src/disks.rs)
- **What changed:** the inline, stateful progress-throttle closure (tracking
  `last_emit`/`last_done`, computing the instantaneous rate, emitting) is now a
  small `MaterializeProgress` reporter:
  ```rust
  let mut progress = MaterializeProgress::new(app.clone(), job_id, total);
  let result = cloud::materialize(&path, workers, |done| progress.report(done), || sentinel.exists());
  ```
- **Why it's better:** the spawn function now reads as its actual job
  (register → emit 0% → resolve workers → materialize → dispatch result) instead
  of interleaving rate-math and event-throttling. The throttle/rate logic is
  named and self-contained, with the "why instantaneous, not average" rationale
  attached to the type.

### L2 — Magic `250` ms progress throttle

- **File:** [src-tauri/src/disks.rs](../src-tauri/src/disks.rs)
- **What changed:** the bare `Duration::from_millis(250)` throttle interval is
  now `const PROGRESS_EMIT_INTERVAL`.
- **Why it's better:** the number has a name explaining what it gates (UI update
  frequency), separate from the unrelated `250` in cloud.rs's backoff.

---

## Items skipped

None — all six confirmed items were implemented.

| Item | Reason |
|---|---|
| — | — |

---

## Test results

| Metric | Before | After |
|---|---|---|
| Tests passing | 491 | 491 |
| Tests failing | 0 | 0 |
| Baseline tests lost/regressed | — | 0 (`comm -23` empty) |
| `cargo clippy --all-targets -- -D warnings` | clean | clean |

**Coverage:** preserved by construction — every extraction (M1's helpers, M3's
`MaterializeProgress`, M2's `config_value`) keeps the same code paths under the
same callers/tests; no tested code was removed. The cloud.rs paths remain
covered by the four `cloud::tests::*` unit tests. A separate instrumented
coverage run was not taken (full-Tauri-crate `llvm-cov` is disproportionately
slow for a no-behaviour-change refactor, and the test contract already pins the
behaviour).

**New tests:** none. The new code that wasn't already covered (`config_value`,
`MaterializeProgress`) is coupled to the Tauri runtime / DB state; unit-testing
it would require fragile runtime mocking, which the dev-loop guidance says to
avoid. No new *pure* functions were introduced.
