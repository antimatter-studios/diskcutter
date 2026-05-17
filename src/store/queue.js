// Central store for the burn queue. Holds the `jobs` map + insertion
// order and the `entries` (bulk-copy counter rows), and owns the Tauri
// event listeners that mutate them. App.jsx used to do all of this
// inline with `useState` + a dozen `setJobs((js) => js.map(...))` call
// sites — every new backend event channel was another listener +
// reducer to wire up. Pulling it into a single store lets each event
// channel become one action and each consumer one selector.
//
// Shape: jobs is a `{ [id]: Job }` map for O(1) lookup; `order` is the
// insertion-order list of IDs that drives the queue UI. The existing
// `applyJobUpdate/Complete/Failure` reducers in job-reducers.js stay
// array-based — the store converts at the boundary so we don't churn
// their tests in this refactor.

import { create } from 'zustand';
import { listen } from '@tauri-apps/api/event';
import { invoke } from '@tauri-apps/api/core';

import { makeJob, makeEntry, decrementEntry } from '../format.js';
import {
  applyJobUpdate, applyJobComplete, applyJobFailure,
} from '../job-reducers.js';

function orderedJobs(state) {
  return state.order.map((id) => state.jobs[id]);
}

// Fire-and-forget invoke wrapper. The DB writes for queue persistence
// are non-load-bearing for the UI — if persistence fails the user can
// still burn, they just lose the survive-restart property. So we log
// the failure and move on rather than rolling back the local state.
function fire(name, args) {
  invoke(name, args).catch((e) => console.error(`${name} failed`, e));
}

// Backend BurnJob → frontend Job. Lossy by design: runtime-derived
// fields (validation, partitions, boot, speed/eta/elapsed) are not
// persisted. They re-populate as backend events fire after hydrate.
function jobFromBackend(row, num) {
  // DB state → UI state. 'cancelled' collapses to 'error' for now
  // (the UI doesn't have a dedicated cancelled glyph; the error code
  // ECANCELLED carries the distinction).
  const stateMap = {
    queued: 'idle',
    running: 'writing',
    success: 'success',
    error: 'error',
    cancelled: 'error',
  };
  const image = {
    name: row.image_name,
    path: row.image_path,
    bytes: row.image_bytes,
    // size string is a render artefact; UI will recompute from bytes.
    size: undefined,
    sectors: undefined,
    format: undefined,
    sha256: '—',
  };
  // target_device is the raw device path the user picked previously.
  // We surface it as a stub `target` object so the row renders with
  // its device label; the user may still need to re-pick to populate
  // the richer fields (size, partitions). Empty string means "no
  // target chosen yet" — backend column is NOT NULL so we store an
  // empty string sentinel at enqueue time.
  const target = row.target_device
    ? { device: row.target_device, name: row.target_device }
    : null;
  const uiState = stateMap[row.state] || 'error';
  // Validation badge is meaningless for terminal rows: an errored / cancelled
  // / succeeded row doesn't need its image re-classified, and showing
  // VALIDATING… on top of an error row reads as "still trying", which is a
  // lie. Only queued rows that we'll re-validate post-hydrate stay 'pending'.
  const validation = (uiState === 'success' || uiState === 'error')
    ? 'valid'
    : 'pending';
  return {
    id: row.job_id,
    num,
    image,
    target,
    parentEntryId: null,
    state: uiState,
    progress: 0,
    verifyProgress: 0,
    speed: '—',
    eta: '—',
    elapsed: '—',
    errorCode: row.error_code || undefined,
    errorMessage: row.error_message || undefined,
    verification: null,
    validation,
    validationDetail: null,
    partitions: null,
    boot: null,
    finishedAt: row.finished_at || undefined,
  };
}

// Rebuild map+order from an array. Preserves original insertion order
// for any ID already in `state.order`; appends new IDs (shouldn't
// happen for the existing reducers but the fallback keeps the shape
// honest).
function fromArr(state, arr) {
  const newJobs = {};
  const newOrder = [];
  const seen = new Set();
  for (const id of state.order) {
    const j = arr.find((x) => x.id === id);
    if (j) { newJobs[id] = j; newOrder.push(id); seen.add(id); }
  }
  for (const j of arr) {
    if (!seen.has(j.id)) { newJobs[j.id] = j; newOrder.push(j.id); }
  }
  return { jobs: newJobs, order: newOrder };
}

export const useQueueStore = create((set, get) => ({
  jobs: {},
  order: [],
  entries: [],

  // -- mutations driven by user actions -----------------------------

  // Pull every non-success row from burn_jobs into the store. Called
  // once at app mount so the queue survives a parent-app or dev-server
  // restart. Success rows are intentionally excluded — they'd cause
  // the operator to see ancient already-finished jobs every relaunch.
  async hydrate() {
    try {
      const rows = await invoke('burn_jobs_active');
      if (!Array.isArray(rows)) return;
      // DB returns oldest-first (ORDER BY queued_at ASC); flip so the
      // most recently queued row appears at the top of the visual queue.
      const ordered = rows.slice().reverse();
      const jobs = {};
      const order = [];
      ordered.forEach((row, i) => {
        const j = jobFromBackend(row, i + 1);
        jobs[j.id] = j;
        order.push(j.id);
      });
      set({ jobs, order });
      // Re-kick image validation for any rehydrated idle row whose
      // image_path looks usable. Without this, every queued row from
      // a previous session sits at validation='pending' forever — the
      // Burn button stays grey because the arm gate also checks valid.
      for (const id of order) {
        const j = jobs[id];
        if (j.state === 'idle' && j.image && j.image.path) {
          startValidation(j.id, j.image.path);
        }
      }
    } catch (e) {
      console.error('hydrate failed', e);
    }
  },

  addImage(image, requestedCopies) {
    const requested = Math.max(1, Math.floor(Number(requestedCopies) || 1));
    // New rows land at the TOP of the queue — operators expect the row they
    // just added to be visible without scrolling, and the `num` we stamp is
    // just a stable label that doesn't have to mirror insertion order.
    const num = get().order.length + 1;
    const job = makeJob(num, image, null);
    if (requested === 1) {
      set((s) => ({
        jobs: { ...s.jobs, [job.id]: job },
        order: [job.id, ...s.order],
      }));
    } else {
      const entry = makeEntry(image, requested);
      set((s) => ({
        jobs: { ...s.jobs, [job.id]: { ...job, parentEntryId: entry.id } },
        order: [job.id, ...s.order],
        entries: [...s.entries, { ...entry, copiesRemaining: entry.copiesRemaining - 1 }],
      }));
    }
    fire('enqueue_burn', {
      jobId: job.id,
      imagePath: image.path,
      imageName: image.name,
      imageBytes: image.bytes || 0,
      targetDevice: '',
    });
    return job.id;
  },

  dispatchFromEntry(entryId) {
    const s = get();
    const entry = s.entries.find((en) => en.id === entryId);
    if (!entry || entry.copiesRemaining <= 0) return;
    const num = s.order.length + 1;
    const job = makeJob(num, entry.image, null, entry.id);
    set((cur) => ({
      jobs: { ...cur.jobs, [job.id]: job },
      order: [job.id, ...cur.order],
      entries: cur.entries
        .map((en) => (en.id === entryId ? decrementEntry(en) : en))
        .filter((en) => en.copiesRemaining > 0),
    }));
    fire('enqueue_burn', {
      jobId: job.id,
      imagePath: entry.image.path,
      imageName: entry.image.name,
      imageBytes: entry.image.bytes || 0,
      targetDevice: '',
    });
  },

  setTarget(jobId, disk) {
    set((s) => {
      const j = s.jobs[jobId];
      if (!j) return s;
      return { jobs: { ...s.jobs, [jobId]: { ...j, target: disk } } };
    });
    if (disk?.device) {
      fire('set_burn_target', { jobId, targetDevice: disk.device });
    }
  },

  removeJob(jobId) {
    set((s) => {
      if (!s.jobs[jobId]) return s;
      const { [jobId]: _drop, ...rest } = s.jobs;
      return { jobs: rest, order: s.order.filter((id) => id !== jobId) };
    });
    fire('remove_burn_job', { jobId });
  },

  clearDone() {
    const removed = [];
    set((s) => {
      const survivors = s.order.filter((id) => {
        if (s.jobs[id].state === 'success') {
          removed.push(id);
          return false;
        }
        return true;
      });
      if (survivors.length === s.order.length) return s;
      const nextJobs = {};
      for (const id of survivors) nextJobs[id] = s.jobs[id];
      return { jobs: nextJobs, order: survivors };
    });
    removed.forEach((jobId) => fire('remove_burn_job', { jobId }));
  },

  removeStaleSuccess(thresholdMs) {
    const removed = [];
    set((s) => {
      const now = Date.now();
      const survivors = s.order.filter((id) => {
        const j = s.jobs[id];
        if (j.state !== 'success') return true;
        if (!j.finishedAt) return true;
        if ((now - j.finishedAt) < thresholdMs) return true;
        removed.push(id);
        return false;
      });
      if (survivors.length === s.order.length) return s;
      const nextJobs = {};
      for (const id of survivors) nextJobs[id] = s.jobs[id];
      return { jobs: nextJobs, order: survivors };
    });
    removed.forEach((jobId) => fire('remove_burn_job', { jobId }));
  },

  flashAnother(jobId) {
    let newId = null;
    let imageRef = null;
    set((s) => {
      const j = s.jobs[jobId];
      if (!j) return s;
      const num = s.order.length + 1;
      const newJob = makeJob(num, j.image, null);
      newId = newJob.id;
      imageRef = j.image;
      return {
        jobs: { ...s.jobs, [newJob.id]: newJob },
        order: [...s.order, newJob.id],
      };
    });
    if (newId && imageRef) {
      fire('enqueue_burn', {
        jobId: newId,
        imagePath: imageRef.path,
        imageName: imageRef.name,
        imageBytes: imageRef.bytes || 0,
        targetDevice: '',
      });
    }
  },

  setRetrying(jobId, retrying) {
    set((s) => {
      const j = s.jobs[jobId];
      if (!j) return s;
      const next = retrying
        ? { ...j, state: 'idle', progress: 0, verifyProgress: 0, errorCode: undefined, errorMessage: undefined, verification: null, retrying: true }
        : { ...j, retrying: false };
      return { jobs: { ...s.jobs, [jobId]: next } };
    });
  },

  markTooSmall(ids) {
    set((s) => {
      const next = { ...s.jobs };
      for (const id of ids) {
        if (next[id]) next[id] = { ...next[id], state: 'error', errorCode: 'ETOOBIG' };
      }
      return { jobs: next };
    });
  },

  clearEneedsFdaErrors() {
    set((s) => {
      let changed = false;
      const next = {};
      for (const id of s.order) {
        const j = s.jobs[id];
        if (j.state === 'error' && j.errorCode === 'ENEEDS_FDA') {
          changed = true;
          next[id] = { ...j, state: 'idle', errorCode: undefined, errorMessage: undefined };
        } else {
          next[id] = j;
        }
      }
      return changed ? { jobs: next } : s;
    });
  },

  // -- mutations driven by backend events ---------------------------

  applyJobUpdate(payload) {
    set((s) => fromArr(s, applyJobUpdate(orderedJobs(s), payload)));
  },

  applyJobComplete(payload) {
    let parentEntryId = null;
    set((s) => {
      const before = s.jobs[payload.job_id];
      if (before) parentEntryId = before.parentEntryId || null;
      const finishedAt = Date.now();
      const arr = applyJobComplete(orderedJobs(s), payload).map((j) => (
        j.id === payload.job_id && j.state === 'success' ? { ...j, finishedAt } : j
      ));
      const next = fromArr(s, arr);
      if (parentEntryId) {
        next.entries = s.entries
          .map((en) => (en.id === parentEntryId ? decrementEntry(en) : en))
          .filter((en) => en.copiesRemaining > 0);
      }
      return next;
    });
  },

  applyJobFailure(payload) {
    set((s) => fromArr(s, applyJobFailure(orderedJobs(s), payload)));
  },

  setValidation(jobId, report) {
    const result = report.result === 'valid' ? 'valid' : 'invalid';
    const detail = report.detail || report.reason || null;
    let imagePath = null;
    set((s) => {
      const j = s.jobs[jobId];
      if (!j) return s;
      imagePath = j.image?.path || null;
      return { jobs: { ...s.jobs, [jobId]: { ...j, validation: result, validationDetail: detail } } };
    });
    // A valid verdict means it's worth running the more expensive
    // probes; invalid means we'd be probing a non-disk and skip the
    // round-trips entirely. The partition probe and bootability probe
    // each open their own DiskImage and run independently, so we fire
    // them in parallel.
    if (result === 'valid' && imagePath) {
      invoke('inspect_image_partitions', { jobId, path: imagePath })
        .catch((e) => console.error('inspect_image_partitions failed', e));
      invoke('inspect_image_bootable', { jobId, path: imagePath })
        .catch((e) => console.error('inspect_image_bootable failed', e));
    }
  },

  setPartitions(jobId, summary) {
    set((s) => {
      const j = s.jobs[jobId];
      if (!j) return s;
      return { jobs: { ...s.jobs, [jobId]: { ...j, partitions: summary || null } } };
    });
  },

  setBoot(jobId, boot) {
    set((s) => {
      const j = s.jobs[jobId];
      if (!j) return s;
      return { jobs: { ...s.jobs, [jobId]: { ...j, boot: boot || null } } };
    });
  },
}));

// Selectors -- co-located so consumers import one symbol per concern.

export const selectJobs = (s) => orderedJobs(s);
export const selectEntries = (s) => s.entries;
export const selectJob = (id) => (s) => s.jobs[id];
export const selectJobIndex = (id) => (s) => s.order.indexOf(id);

// Listener wiring: call once at mount, return the unsubscribe. The
// FDA reaction (open System Settings, set fdaBlocked) and the
// "invalid image" toast stay in App.jsx so we don't drag UI side
// effects into the store — we just surface the relevant payloads via
// callbacks.
export function attachQueueListeners({ onFdaError, onImageInvalid } = {}) {
  const subs = [];
  let mounted = true;

  listen('disk-cutter://job-update', (e) => {
    if (!mounted) return;
    useQueueStore.getState().applyJobUpdate(e.payload);
  }).then((u) => subs.push(u));

  listen('disk-cutter://job-complete', (e) => {
    if (!mounted) return;
    useQueueStore.getState().applyJobComplete(e.payload);
  }).then((u) => subs.push(u));

  listen('disk-cutter://job-error', (e) => {
    if (!mounted) return;
    useQueueStore.getState().applyJobFailure(e.payload);
    if (e.payload?.error_code === 'ENEEDS_FDA') {
      onFdaError?.(e.payload);
    }
  }).then((u) => subs.push(u));

  listen('disk-cutter://image-validated', (e) => {
    if (!mounted) return;
    const p = e.payload;
    useQueueStore.getState().setValidation(p.job_id, p);
    if (p.result !== 'valid') {
      onImageInvalid?.(p);
    }
  }).then((u) => subs.push(u));

  listen('disk-cutter://image-partitioned', (e) => {
    if (!mounted) return;
    const p = e.payload;
    useQueueStore.getState().setPartitions(p.job_id, p.summary);
  }).then((u) => subs.push(u));

  listen('disk-cutter://image-boot-checked', (e) => {
    if (!mounted) return;
    const p = e.payload;
    useQueueStore.getState().setBoot(p.job_id, {
      bootable: !!p.bootable,
      sources: Array.isArray(p.sources) ? p.sources : [],
    });
  }).then((u) => subs.push(u));

  return () => {
    mounted = false;
    subs.forEach((u) => u());
  };
}

// Convenience: kicks off the backend validation pipeline for a job.
// Lives here so the call site in App.jsx doesn't have to know about
// the command name.
export function startValidation(jobId, path) {
  return invoke('validate_image_contents', { jobId, path })
    .catch((e) => console.error('validate_image_contents failed', e));
}
