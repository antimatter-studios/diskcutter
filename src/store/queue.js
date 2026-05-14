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

  addImage(image, requestedCopies) {
    const requested = Math.max(1, Math.floor(Number(requestedCopies) || 1));
    // Use the eventual num so the generated ID matches the row label.
    const num = get().order.length + 1;
    const job = makeJob(num, image, null);
    if (requested === 1) {
      set((s) => ({
        jobs: { ...s.jobs, [job.id]: job },
        order: [...s.order, job.id],
      }));
    } else {
      const entry = makeEntry(image, requested);
      set((s) => ({
        jobs: { ...s.jobs, [job.id]: { ...job, parentEntryId: entry.id } },
        order: [...s.order, job.id],
        entries: [...s.entries, { ...entry, copiesRemaining: entry.copiesRemaining - 1 }],
      }));
    }
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
      order: [...cur.order, job.id],
      entries: cur.entries
        .map((en) => (en.id === entryId ? decrementEntry(en) : en))
        .filter((en) => en.copiesRemaining > 0),
    }));
  },

  setTarget(jobId, disk) {
    set((s) => {
      const j = s.jobs[jobId];
      if (!j) return s;
      return { jobs: { ...s.jobs, [jobId]: { ...j, target: disk } } };
    });
  },

  removeJob(jobId) {
    set((s) => {
      if (!s.jobs[jobId]) return s;
      const { [jobId]: _drop, ...rest } = s.jobs;
      return { jobs: rest, order: s.order.filter((id) => id !== jobId) };
    });
  },

  clearDone() {
    set((s) => {
      const survivors = s.order.filter((id) => s.jobs[id].state !== 'success');
      if (survivors.length === s.order.length) return s;
      const nextJobs = {};
      for (const id of survivors) nextJobs[id] = s.jobs[id];
      return { jobs: nextJobs, order: survivors };
    });
  },

  removeStaleSuccess(thresholdMs) {
    set((s) => {
      const now = Date.now();
      const survivors = s.order.filter((id) => {
        const j = s.jobs[id];
        if (j.state !== 'success') return true;
        if (!j.finishedAt) return true;
        return (now - j.finishedAt) < thresholdMs;
      });
      if (survivors.length === s.order.length) return s;
      const nextJobs = {};
      for (const id of survivors) nextJobs[id] = s.jobs[id];
      return { jobs: nextJobs, order: survivors };
    });
  },

  flashAnother(jobId) {
    set((s) => {
      const j = s.jobs[jobId];
      if (!j) return s;
      const num = s.order.length + 1;
      const newJob = makeJob(num, j.image, null);
      return {
        jobs: { ...s.jobs, [newJob.id]: newJob },
        order: [...s.order, newJob.id],
      };
    });
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
    // partition probe; invalid means we'd be probing a non-disk and
    // skip the round-trip entirely.
    if (result === 'valid' && imagePath) {
      invoke('inspect_image_partitions', { jobId, path: imagePath })
        .catch((e) => console.error('inspect_image_partitions failed', e));
    }
  },

  setPartitions(jobId, summary) {
    set((s) => {
      const j = s.jobs[jobId];
      if (!j) return s;
      return { jobs: { ...s.jobs, [jobId]: { ...j, partitions: summary || null } } };
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
