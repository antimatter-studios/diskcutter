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

  it('does not mint an id — the backend assigns the integer PK on enqueue', () => {
    const job = makeJob(3, {}, null);
    expect(job.id).toBeUndefined();
  });

  it('accepts null target for an awaiting-target job', () => {
    const job = makeJob(1, { name: 'x' }, null);
    expect(job.target).toBeNull();
  });
});

// ---------------------------------------------------------------------------
// Gap-fill: boundary semantics, large values, purity.
// ---------------------------------------------------------------------------

describe('formatBytes boundary semantics', () => {
  it('exactly 1000 promotes to kB (>= 1e3 boundary)', () => {
    expect(formatBytes(1000)).toBe('1 kB');
  });

  it('999 stays in raw bytes', () => {
    expect(formatBytes(999)).toBe('999 B');
  });

  it('exactly 1_000_000 promotes to MB', () => {
    expect(formatBytes(1_000_000)).toBe('1.0 MB');
  });

  it('999_999 stays in kB', () => {
    expect(formatBytes(999_999)).toBe('1000 kB');
  });

  it('exactly 1_000_000_000 promotes to GB', () => {
    expect(formatBytes(1_000_000_000)).toBe('1.00 GB');
  });

  it('999_999_999 stays in MB', () => {
    expect(formatBytes(999_999_999)).toBe('1000.0 MB');
  });

  it('handles very large GB values', () => {
    expect(formatBytes(1_500_000_000_000)).toBe('1500.00 GB');
  });

  it('handles 1023 (just under historical 1024 boundary — decimal, not binary)', () => {
    // formatBytes uses decimal (1e3) thresholds, so 1023 is already > 1000 → kB.
    expect(formatBytes(1023)).toBe('1 kB');
    expect(formatBytes(1024)).toBe('1 kB');
  });
});

describe('formatBps boundary semantics', () => {
  it('exactly 1000 promotes to kB/s', () => {
    expect(formatBps(1000)).toBe('1 kB/s');
  });

  it('999 stays in raw B/s', () => {
    expect(formatBps(999)).toBe('999 B/s');
  });

  it('exactly 1_000_000 promotes to MB/s', () => {
    expect(formatBps(1_000_000)).toBe('1.0 MB/s');
  });

  it('exactly 1_000_000_000 promotes to GB/s', () => {
    expect(formatBps(1_000_000_000)).toBe('1.00 GB/s');
  });
});

describe('formatDuration edge cases', () => {
  it('truncates sub-second values toward zero', () => {
    // 999ms → still 0 seconds total.
    expect(formatDuration(999)).toBe('00:00:00');
  });

  it('exactly 1000ms produces 00:00:01', () => {
    expect(formatDuration(1000)).toBe('00:00:01');
  });

  it('exactly 60_000ms produces 00:01:00', () => {
    expect(formatDuration(60_000)).toBe('00:01:00');
  });

  it('exactly 3_600_000ms produces 01:00:00', () => {
    expect(formatDuration(3_600_000)).toBe('01:00:00');
  });

  it('handles very large hour counts without wrapping', () => {
    // 250h 30m 45s
    const ms = (250 * 3600 + 30 * 60 + 45) * 1000;
    expect(formatDuration(ms)).toBe('250:30:45');
  });
});

describe('formatSession edge cases', () => {
  it('treats null/undefined as 0h 00m', () => {
    expect(formatSession(null)).toBe('0h 00m');
    expect(formatSession(undefined)).toBe('0h 00m');
  });

  it('exactly 59 minutes stays in 0h', () => {
    expect(formatSession(59 * 60_000)).toBe('0h 59m');
  });

  it('60 minutes flips to 1h 00m', () => {
    expect(formatSession(60 * 60_000)).toBe('1h 00m');
  });

  it('handles very long sessions (>24h)', () => {
    expect(formatSession(48 * 3_600_000 + 7 * 60_000)).toBe('48h 07m');
  });
});

describe('makeJob purity & uniqueness', () => {
  it('does not mutate the image or target objects', () => {
    const img = { name: 'a.iso', bytes: 100 };
    const tgt = { device: '/dev/disk5', bytes: 200 };
    const imgSnap = JSON.parse(JSON.stringify(img));
    const tgtSnap = JSON.parse(JSON.stringify(tgt));
    makeJob(1, img, tgt);
    expect(img).toEqual(imgSnap);
    expect(tgt).toEqual(tgtSnap);
  });

  it('does not stamp an id; the integer PK comes from the backend', () => {
    // makeJob is purely a UI defaults factory now. The store calls
    // enqueue_burn first, gets the backend-assigned integer, and
    // attaches it as `.id` before inserting the row.
    const a = makeJob(1, {}, null);
    const b = makeJob(2, {}, null);
    expect(a.id).toBeUndefined();
    expect(b.id).toBeUndefined();
  });

  it('preserves identity of image and target (reference, not copy)', () => {
    const img = { name: 'x' };
    const tgt = { device: '/dev/y' };
    const job = makeJob(0, img, tgt);
    expect(job.image).toBe(img);
    expect(job.target).toBe(tgt);
  });
});
