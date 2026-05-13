import { describe, it, expect } from 'vitest';
import {
  addToast, reapExpired, dismissToast, defaultTtlMs, MAX_TOASTS,
} from '../src/toast.js';

const mkToast = (id, overrides = {}) => ({
  id,
  level: 'info',
  message: `m${id}`,
  expiresAt: 1000 + id,
  ...overrides,
});

describe('addToast', () => {
  it('prepends the new toast (newest first)', () => {
    const list = [mkToast(1), mkToast(2)];
    const next = addToast(list, mkToast(99));
    expect(next[0].id).toBe(99);
    expect(next.map((t) => t.id)).toEqual([99, 1, 2]);
  });

  it('does not mutate the input list', () => {
    const list = [mkToast(1)];
    addToast(list, mkToast(2));
    expect(list).toEqual([mkToast(1)]);
  });

  it('caps the queue at 8 toasts', () => {
    let list = [];
    for (let i = 0; i < 12; i++) list = addToast(list, mkToast(i));
    expect(list).toHaveLength(8);
    expect(MAX_TOASTS).toBe(8);
    // The newest 8 should be retained, oldest dropped.
    expect(list[0].id).toBe(11);
    expect(list[7].id).toBe(4);
  });

  it('tolerates a non-array input', () => {
    const next = addToast(null, mkToast(1));
    expect(next).toEqual([mkToast(1)]);
  });
});

describe('reapExpired', () => {
  it('filters out toasts whose expiresAt is <= now', () => {
    const list = [
      mkToast(1, { expiresAt: 500 }),
      mkToast(2, { expiresAt: 1500 }),
      mkToast(3, { expiresAt: 2500 }),
    ];
    expect(reapExpired(list, 1000).map((t) => t.id)).toEqual([2, 3]);
  });

  it('drops toasts whose expiresAt exactly equals now (strict >)', () => {
    const list = [mkToast(1, { expiresAt: 1000 }), mkToast(2, { expiresAt: 1001 })];
    expect(reapExpired(list, 1000).map((t) => t.id)).toEqual([2]);
  });

  it('returns an empty array when all are expired', () => {
    const list = [mkToast(1, { expiresAt: 1 }), mkToast(2, { expiresAt: 2 })];
    expect(reapExpired(list, 999)).toEqual([]);
  });

  it('returns the full list when none are expired', () => {
    const list = [mkToast(1, { expiresAt: 10000 }), mkToast(2, { expiresAt: 20000 })];
    expect(reapExpired(list, 0)).toEqual(list);
  });

  it('tolerates a non-array input', () => {
    expect(reapExpired(undefined, 100)).toEqual([]);
  });
});

describe('dismissToast', () => {
  it('removes only the matching id', () => {
    const list = [mkToast(1), mkToast(2), mkToast(3)];
    expect(dismissToast(list, 2).map((t) => t.id)).toEqual([1, 3]);
  });

  it('returns the list unchanged when id is not present', () => {
    const list = [mkToast(1), mkToast(2)];
    expect(dismissToast(list, 999)).toEqual(list);
  });

  it('matches on strict equality (numeric vs string ids differ)', () => {
    const list = [mkToast(1), { ...mkToast(1), id: '1' }];
    expect(dismissToast(list, 1)).toEqual([{ ...mkToast(1), id: '1' }]);
  });

  it('tolerates a non-array input', () => {
    expect(dismissToast(null, 1)).toEqual([]);
  });
});

describe('defaultTtlMs', () => {
  it('returns 8000 for error', () => {
    expect(defaultTtlMs('error')).toBe(8000);
  });

  it('returns 4000 for info', () => {
    expect(defaultTtlMs('info')).toBe(4000);
  });

  it('returns 4000 for warn', () => {
    expect(defaultTtlMs('warn')).toBe(4000);
  });
});

// ---------------------------------------------------------------------------
// Gap-fill: malformed/null entries, unknown levels, identity preservation.
// ---------------------------------------------------------------------------

describe('reapExpired malformed-entry handling', () => {
  it('drops null entries (treated as expired since they are falsy)', () => {
    const list = [null, mkToast(1, { expiresAt: 9999 })];
    expect(reapExpired(list, 0).map((t) => t.id)).toEqual([1]);
  });

  it('drops entries missing expiresAt (undefined > now is false)', () => {
    const list = [{ id: 1, level: 'info', message: 'no-ttl' }, mkToast(2, { expiresAt: 9999 })];
    expect(reapExpired(list, 0).map((t) => t.id)).toEqual([2]);
  });
});

describe('dismissToast malformed-entry handling', () => {
  it('drops null entries while filtering', () => {
    const list = [null, mkToast(1), mkToast(2)];
    const after = dismissToast(list, 1);
    // Null entries are filtered out because the predicate `t && t.id !== id`
    // is false for null, dropping them.
    expect(after).toEqual([mkToast(2)]);
  });
});

describe('defaultTtlMs additional cases', () => {
  it('returns 4000 for unknown level strings', () => {
    expect(defaultTtlMs('debug')).toBe(4000);
    expect(defaultTtlMs('')).toBe(4000);
  });

  it('returns 4000 for null/undefined level', () => {
    expect(defaultTtlMs(null)).toBe(4000);
    expect(defaultTtlMs(undefined)).toBe(4000);
  });
});

describe('addToast purity & identity', () => {
  it('preserves identity of toasts that are kept', () => {
    const t1 = mkToast(1);
    const t2 = mkToast(2);
    const next = addToast([t1, t2], mkToast(3));
    expect(next[1]).toBe(t1);
    expect(next[2]).toBe(t2);
  });
});
