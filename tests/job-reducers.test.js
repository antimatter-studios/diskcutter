import { describe, it, expect } from 'vitest';
import { applyJobUpdate, applyJobComplete, applyJobFailure } from '../src/job-reducers.js';

const idleJob = (id, overrides = {}) => ({
  id,
  num: 1,
  image: { name: 'a.iso', bytes: 1000, sectors: 2, sha256: 'h' },
  target: { device: '/dev/disk5' },
  state: 'idle',
  progress: 0,
  verifyProgress: 0,
  speed: '—',
  eta: '—',
  elapsed: '—',
  verification: null,
  ...overrides,
});

describe('applyJobUpdate', () => {
  it('updates write progress/speed/eta for the matched job in writing state', () => {
    const before = [idleJob('A'), idleJob('B')];
    const after = applyJobUpdate(before, {
      job_id: 'A', state: 'writing', progress: 42.5, speed: '120 MB/s', eta: '00:30',
    });
    expect(after[0]).toMatchObject({
      id: 'A', state: 'writing', progress: 42.5, speed: '120 MB/s', eta: '00:30',
    });
    expect(after[0].verifyProgress).toBe(0);
    expect(after[1]).toBe(before[1]); // unrelated job untouched
  });

  it('routes verifying state into verifyProgress (not progress)', () => {
    const before = [idleJob('A', { progress: 100 })];
    const after = applyJobUpdate(before, {
      job_id: 'A', state: 'verifying', progress: 10, speed: '90 MB/s', eta: '00:05',
    });
    expect(after[0].state).toBe('verifying');
    expect(after[0].verifyProgress).toBe(10);
    expect(after[0].progress).toBe(100); // write progress preserved
  });

  it('returns jobs unchanged when no id matches', () => {
    const before = [idleJob('A')];
    const after = applyJobUpdate(before, {
      job_id: 'ghost', state: 'writing', progress: 50, speed: 'x', eta: 'y',
    });
    expect(after[0]).toBe(before[0]);
  });

  it('returns jobs unchanged for an unknown state', () => {
    const before = [idleJob('A')];
    const after = applyJobUpdate(before, {
      job_id: 'A', state: 'rewinding', progress: 99, speed: 'x', eta: 'y',
    });
    expect(after[0]).toBe(before[0]);
  });
});

describe('applyJobComplete', () => {
  const completePayload = (overrides = {}) => ({
    job_id: 'A',
    verify_match: true,
    bytes_written: 1024,
    source_sha256: 'src',
    readback_sha256: 'dev',
    mismatches: [],
    elapsed_ms: 3_661_000,
    avg_write_bps: 100_000_000,
    avg_verify_bps: 200_000_000,
    ...overrides,
  });

  it('marks success on verify match and clears errorCode', () => {
    const before = [idleJob('A', { errorCode: 'PRIOR' })];
    const after = applyJobComplete(before, completePayload());
    expect(after[0].state).toBe('success');
    expect(after[0].errorCode).toBeUndefined();
    expect(after[0].progress).toBe(100);
    expect(after[0].verifyProgress).toBe(100);
  });

  it('flags hash-mismatch error when verify_match is false', () => {
    const before = [idleJob('A')];
    const after = applyJobComplete(before, completePayload({ verify_match: false }));
    expect(after[0].state).toBe('error');
    expect(after[0].errorCode).toBe('EHASHMISMATCH');
  });

  it('formats elapsed and average write speed', () => {
    const before = [idleJob('A')];
    const after = applyJobComplete(before, completePayload({
      elapsed_ms: 3_661_000,        // 01:01:01
      avg_write_bps: 100_000_000,   // 100.0 MB/s
    }));
    expect(after[0].elapsed).toBe('01:01:01');
    expect(after[0].speed).toBe('100.0 MB/s');
  });

  it('attaches a verification block with sector counts and mismatches', () => {
    const before = [idleJob('A')];
    const after = applyJobComplete(before, completePayload({
      bytes_written: 2048,
      mismatches: [
        { lba: '0x1', byte_offset: '+0x0', expected: 'AA', actual: 'BB' },
      ],
    }));
    const v = after[0].verification;
    expect(v.sourceHash).toBe('src');
    expect(v.readHash).toBe('dev');
    expect(v.match).toBe(true);
    expect(v.checked).toBe(4);
    expect(v.total).toBe(4);
    expect(v.mismatches).toHaveLength(1);
    expect(v.mismatches[0].note).toBe('');
    expect(v.throughput).toMatch(/avg$/);
  });

  it('treats missing mismatches as an empty list', () => {
    const before = [idleJob('A')];
    const after = applyJobComplete(before, completePayload({ mismatches: undefined }));
    expect(after[0].verification.mismatches).toEqual([]);
  });

  it('leaves unrelated jobs untouched', () => {
    const before = [idleJob('A'), idleJob('B')];
    const after = applyJobComplete(before, completePayload({ job_id: 'A' }));
    expect(after[1]).toBe(before[1]);
  });
});

describe('applyJobFailure', () => {
  it('marks state error and records code+message', () => {
    const before = [idleJob('A')];
    const after = applyJobFailure(before, {
      job_id: 'A', error_code: 'EIO', error_message: 'disk on fire',
    });
    expect(after[0]).toMatchObject({
      state: 'error', errorCode: 'EIO', errorMessage: 'disk on fire',
    });
  });

  it('returns jobs unchanged when no id matches', () => {
    const before = [idleJob('A')];
    const after = applyJobFailure(before, {
      job_id: 'ghost', error_code: 'EIO', error_message: 'x',
    });
    expect(after[0]).toBe(before[0]);
  });

  it('preserves the other fields of the failing job', () => {
    const before = [idleJob('A', { progress: 35, speed: '120 MB/s' })];
    const after = applyJobFailure(before, {
      job_id: 'A', error_code: 'EIO', error_message: 'x',
    });
    expect(after[0].progress).toBe(35);
    expect(after[0].speed).toBe('120 MB/s');
  });
});
