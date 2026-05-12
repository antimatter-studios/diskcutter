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
  const job = (id, state, imageBytes, targetBytes) => ({
    id,
    state,
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
});
