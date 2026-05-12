import { describe, it, expect } from 'vitest';
import { formatBytes, formatBps, formatDuration, formatSession, makeJob } from '../src/format.js';

describe('formatBytes', () => {
  it('returns the dash placeholder for null/undefined', () => {
    expect(formatBytes(null)).toBe('—');
    expect(formatBytes(undefined)).toBe('—');
  });

  it('renders sub-kilobyte values as raw bytes', () => {
    expect(formatBytes(0)).toBe('0 B');
    expect(formatBytes(512)).toBe('512 B');
    expect(formatBytes(999)).toBe('999 B');
  });

  it('promotes to kB / MB / GB at the right thresholds', () => {
    expect(formatBytes(4_000)).toBe('4 kB');
    expect(formatBytes(1_500_000)).toBe('1.5 MB');
    expect(formatBytes(2_000_000_000)).toBe('2.00 GB');
  });
});

describe('formatBps', () => {
  it('returns the dash placeholder for null/undefined', () => {
    expect(formatBps(null)).toBe('—');
    expect(formatBps(undefined)).toBe('—');
  });

  it('renders sub-kilobyte rates as raw B/s', () => {
    expect(formatBps(0)).toBe('0 B/s');
    expect(formatBps(500)).toBe('500 B/s');
  });

  it('promotes to kB/s, MB/s, GB/s at the right thresholds', () => {
    expect(formatBps(4_000)).toBe('4 kB/s');
    expect(formatBps(2_500_000)).toBe('2.5 MB/s');
    expect(formatBps(3_000_000_000)).toBe('3.00 GB/s');
  });
});

describe('formatDuration', () => {
  it('treats null / undefined / zero as 00:00:00', () => {
    expect(formatDuration(null)).toBe('00:00:00');
    expect(formatDuration(undefined)).toBe('00:00:00');
    expect(formatDuration(0)).toBe('00:00:00');
  });

  it('renders HH:MM:SS', () => {
    expect(formatDuration(1_000)).toBe('00:00:01');
    expect(formatDuration(65_000)).toBe('00:01:05');
    expect(formatDuration(3_661_000)).toBe('01:01:01');
  });

  it('does not wrap at 100 hours', () => {
    // 100h exactly
    expect(formatDuration(100 * 3600 * 1000)).toBe('100:00:00');
  });
});

describe('formatSession', () => {
  it('renders Xh MMm format', () => {
    expect(formatSession(0)).toBe('0h 00m');
    expect(formatSession(60_000)).toBe('0h 01m');
    expect(formatSession(3_600_000)).toBe('1h 00m');
    expect(formatSession(3_660_000)).toBe('1h 01m');
  });

  it('drops the seconds component', () => {
    // 1h 02m 30s — seconds shouldn't appear
    expect(formatSession(3_600_000 + 120_000 + 30_000)).toBe('1h 02m');
  });
});

describe('makeJob', () => {
  it('produces a job in idle state with placeholder defaults', () => {
    const img = { name: 'a.iso' };
    const tgt = { device: '/dev/disk5' };
    const job = makeJob(7, img, tgt);

    expect(job.num).toBe(7);
    expect(job.image).toBe(img);
    expect(job.target).toBe(tgt);
    expect(job.state).toBe('idle');
    expect(job.progress).toBe(0);
    expect(job.verifyProgress).toBe(0);
    expect(job.speed).toBe('—');
    expect(job.eta).toBe('—');
    expect(job.elapsed).toBe('—');
    expect(job.errorCode).toBeUndefined();
    expect(job.errorMessage).toBeUndefined();
    expect(job.verification).toBeNull();
  });

  it('generates a job id that includes the index suffix', () => {
    const job = makeJob(3, {}, null);
    expect(job.id).toMatch(/^job-\d+-3$/);
  });

  it('accepts null target for an awaiting-target job', () => {
    const job = makeJob(1, { name: 'x' }, null);
    expect(job.target).toBeNull();
  });
});
