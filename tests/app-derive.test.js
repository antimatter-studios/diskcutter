import { describe, it, expect } from 'vitest';
import { computeScene, sceneToTitleKey, computeSessionStats, planStart } from '../src/app-derive.js';

const j = (state, extra = {}) => ({ state, ...extra });

describe('computeScene', () => {
  it('picks diskpicker whenever pickerJob is set', () => {
    expect(computeScene([], 0, null)).toBe('diskpicker');
    expect(computeScene([j('writing')], 5, null)).toBe('diskpicker');
  });

  it('returns empty when the queue is empty and no picker is open', () => {
    expect(computeScene([], null, null)).toBe('empty');
  });

  it('returns writing when any job is writing', () => {
    expect(computeScene([j('idle'), j('writing')], null, null)).toBe('writing');
  });

  it('writing wins over verifying when both states coexist', () => {
    expect(computeScene([j('writing'), j('verifying')], null, null)).toBe('writing');
  });

  it('returns verifying when no writes and at least one verify is in flight', () => {
    expect(computeScene([j('idle'), j('verifying')], null, null)).toBe('verifying');
  });

  it('returns error when no writes/verifies and an errorJob is provided', () => {
    const err = j('error');
    expect(computeScene([j('idle'), err], null, err)).toBe('error');
  });

  it('returns success only when every job has succeeded', () => {
    expect(computeScene([j('success'), j('success')], null, null)).toBe('success');
    expect(computeScene([j('success'), j('idle')], null, null)).toBe('idle');
  });

  it('returns idle when jobs are present but none are running, failed, or all-success', () => {
    expect(computeScene([j('idle'), j('idle')], null, null)).toBe('idle');
  });
});

describe('sceneToTitleKey', () => {
  it('returns the short key when verbose is false', () => {
    expect(sceneToTitleKey('writing', false)).toBe('app.title_short');
    expect(sceneToTitleKey('idle', false)).toBe('app.title_short');
  });

  it('maps each scene to a scene-specific key when verbose', () => {
    expect(sceneToTitleKey('success', true)).toBe('app.title_complete');
    expect(sceneToTitleKey('error', true)).toBe('app.title_error');
    expect(sceneToTitleKey('writing', true)).toBe('app.title_writing');
    expect(sceneToTitleKey('verifying', true)).toBe('app.title_verifying');
  });

  it('falls back to the ready key for scenes without a verbose variant', () => {
    expect(sceneToTitleKey('idle', true)).toBe('app.title_ready');
    expect(sceneToTitleKey('empty', true)).toBe('app.title_ready');
    expect(sceneToTitleKey('diskpicker', true)).toBe('app.title_ready');
  });
});

describe('computeSessionStats', () => {
  it('renders dash placeholders when no job has completed yet', () => {
    const stats = computeSessionStats([j('idle'), j('writing')], 0, 1_000);
    expect(stats.written).toBe('—');
    expect(stats.avg).toBe('—');
  });

  it('sums bytesWritten and averages avgBps over completed jobs', () => {
    const jobs = [
      { state: 'success', bytesWritten: 1_000_000_000, avgBps: 100_000_000 },
      { state: 'success', bytesWritten: 3_000_000_000, avgBps: 200_000_000 },
      { state: 'idle' }, // not completed → excluded
    ];
    const stats = computeSessionStats(jobs, 0, 0);
    expect(stats.written).toBe('4.00 GB');
    expect(stats.avg).toBe('150.0 MB/s');
  });

  it('formats elapsed session time as Xh MMm', () => {
    const stats = computeSessionStats([], 0, 3_660_000);
    expect(stats.session).toBe('1h 01m');
  });

  it('excludes jobs that never recorded bytesWritten', () => {
    const stats = computeSessionStats([{ state: 'error' }], 0, 0);
    expect(stats.written).toBe('—');
    expect(stats.avg).toBe('—');
  });
});

describe('planStart', () => {
  const job = (id, state, imageBytes, targetBytes, validation = 'valid') => ({
    id,
    state,
    validation,
    image: { bytes: imageBytes },
    target: targetBytes != null ? { bytes: targetBytes, device: `/dev/${id}` } : null,
  });

  it('returns empty plan when nothing is idle', () => {
    const r = planStart([job('a', 'writing', 100, 200), job('b', 'success', 100, 200)]);
    expect(r.ready).toEqual([]);
    expect(r.tooSmall).toEqual([]);
    expect(r.okToBurn).toEqual([]);
  });

  it('skips idle jobs without a target', () => {
    const without = job('a', 'idle', 100, null);
    const r = planStart([without]);
    expect(r.ready).toEqual([]);
    expect(r.okToBurn).toEqual([]);
  });

  it('flags jobs whose target is smaller than the image as too small', () => {
    const small = job('a', 'idle', 1000, 500);
    const big = job('b', 'idle', 1000, 2000);
    const r = planStart([small, big]);
    expect(r.tooSmall).toContain(small);
    expect(r.tooSmall).not.toContain(big);
    expect(r.okToBurn).toEqual([big]);
  });

  it('treats missing image.bytes or target.bytes as non-flag (eligible to burn)', () => {
    const noBytes = job('a', 'idle', 0, 500);    // image bytes 0 → falsy → not flagged
    const noTarget = job('b', 'idle', 1000, 0);  // target bytes 0 → falsy → not flagged
    const r = planStart([noBytes, noTarget]);
    expect(r.tooSmall).toEqual([]);
    expect(r.okToBurn).toEqual([noBytes, noTarget]);
  });

  it('returns equal-size as burnable (target.bytes >= image.bytes)', () => {
    const eq = job('a', 'idle', 1000, 1000);
    const r = planStart([eq]);
    expect(r.tooSmall).toEqual([]);
    expect(r.okToBurn).toEqual([eq]);
  });

  it('excludes jobs whose validation is still pending', () => {
    const pending = job('a', 'idle', 1000, 2000, 'pending');
    const r = planStart([pending]);
    expect(r.ready).toEqual([]);
    expect(r.okToBurn).toEqual([]);
  });

  it('excludes jobs whose validation came back invalid', () => {
    const bad = job('a', 'idle', 1000, 2000, 'invalid');
    const r = planStart([bad]);
    expect(r.ready).toEqual([]);
    expect(r.okToBurn).toEqual([]);
  });
});

// ---------------------------------------------------------------------------
// Gap-fill: purity, falsy inputs, all-state combinations, scene edge cases.
// ---------------------------------------------------------------------------

describe('computeScene additional edge cases', () => {
  it('returns success when single job has succeeded', () => {
    expect(computeScene([j('success')], null, null)).toBe('success');
  });

  it('returns error when errorJob is set even if some jobs are still idle', () => {
    const err = j('error');
    expect(computeScene([j('idle'), j('idle'), err], null, err)).toBe('error');
  });

  it('writing wins over error (any in-flight write takes precedence)', () => {
    const err = j('error');
    expect(computeScene([err, j('writing')], null, err)).toBe('writing');
  });

  it('verifying wins over error', () => {
    const err = j('error');
    expect(computeScene([err, j('verifying')], null, err)).toBe('verifying');
  });

  it('does not mutate the jobs array', () => {
    const jobs = [j('idle'), j('writing')];
    const snap = jobs.map((x) => ({ ...x }));
    computeScene(jobs, null, null);
    expect(jobs).toEqual(snap);
  });

  it('treats a single mixed-state list with one error and rest success as error', () => {
    const err = j('error');
    expect(computeScene([j('success'), err], null, err)).toBe('error');
  });
});

describe('sceneToTitleKey additional cases', () => {
  it('returns short key when verbose is undefined (falsy)', () => {
    expect(sceneToTitleKey('writing', undefined)).toBe('app.title_short');
  });

  it('returns short key when verbose is null', () => {
    expect(sceneToTitleKey('writing', null)).toBe('app.title_short');
  });

  it('returns ready key when scene is null but verbose is true', () => {
    expect(sceneToTitleKey(null, true)).toBe('app.title_ready');
  });

  it('returns ready key when scene is undefined but verbose is true', () => {
    expect(sceneToTitleKey(undefined, true)).toBe('app.title_ready');
  });
});

describe('computeSessionStats additional cases', () => {
  it('single completed job reports its own bytesWritten and avgBps', () => {
    const jobs = [{ state: 'success', bytesWritten: 5_000_000, avgBps: 10_000_000 }];
    const stats = computeSessionStats(jobs, 0, 0);
    expect(stats.written).toBe('5.0 MB');
    expect(stats.avg).toBe('10.0 MB/s');
  });

  it('treats job with bytesWritten=0 as completed (typeof number) and averages avgBps in', () => {
    const jobs = [
      { state: 'success', bytesWritten: 0, avgBps: 0 },
      { state: 'success', bytesWritten: 4_000_000, avgBps: 20_000_000 },
    ];
    const stats = computeSessionStats(jobs, 0, 0);
    // total bytesWritten = 4_000_000, avg = (0 + 20_000_000)/2 = 10_000_000
    expect(stats.written).toBe('4.0 MB');
    expect(stats.avg).toBe('10.0 MB/s');
  });

  it('handles an empty job list', () => {
    const stats = computeSessionStats([], 1000, 5000);
    expect(stats.session).toBe('0h 00m');
    expect(stats.written).toBe('—');
    expect(stats.avg).toBe('—');
  });

  it('does not mutate the jobs array', () => {
    const jobs = [{ state: 'success', bytesWritten: 100, avgBps: 50 }];
    const snap = jobs.map((x) => ({ ...x }));
    computeSessionStats(jobs, 0, 0);
    expect(jobs).toEqual(snap);
  });

  it('handles negative elapsed gracefully (now < sessionStart)', () => {
    // formatSession uses (ms || 0) — a negative value is truthy and will fall
    // through to Math.floor(neg/1000); the formatted output reflects the math.
    const stats = computeSessionStats([], 5000, 1000);
    // total = Math.floor(-4000/1000) = -4 → h=0, m=Math.floor((-4%3600)/60)=0
    // The exact string isn't the point — assert it's a string and doesn't throw.
    expect(typeof stats.session).toBe('string');
  });
});

describe('planStart purity & additional cases', () => {
  const job = (id, state, imageBytes, targetBytes, validation = 'valid') => ({
    id,
    state,
    validation,
    image: { bytes: imageBytes },
    target: targetBytes != null ? { bytes: targetBytes, device: `/dev/${id}` } : null,
  });

  it('does not mutate input jobs', () => {
    const jobs = [job('a', 'idle', 100, 50), job('b', 'idle', 100, 200)];
    const snap = JSON.parse(JSON.stringify(jobs));
    planStart(jobs);
    expect(jobs).toEqual(snap);
  });

  it('partitions ready into okToBurn and tooSmall with no overlap', () => {
    const small = job('a', 'idle', 1000, 500);
    const big = job('b', 'idle', 1000, 2000);
    const r = planStart([small, big]);
    expect(r.ready).toEqual([small, big]);
    // every job in `ready` is in exactly one of okToBurn / tooSmall
    r.ready.forEach((rj) => {
      const inSmall = r.tooSmall.includes(rj);
      const inOk = r.okToBurn.includes(rj);
      expect(inSmall !== inOk).toBe(true); // XOR — exactly one
    });
  });

  it('empty input produces empty plan', () => {
    const r = planStart([]);
    expect(r.ready).toEqual([]);
    expect(r.tooSmall).toEqual([]);
    expect(r.okToBurn).toEqual([]);
  });
});
