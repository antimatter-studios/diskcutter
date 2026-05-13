import { describe, it, expect } from 'vitest';
import { extractKeys, diffKeys } from '../scripts/check-i18n.mjs';

describe('extractKeys', () => {
  it('returns empty set for empty object', () => {
    expect([...extractKeys({})]).toEqual([]);
  });

  it('extracts top-level keys', () => {
    const s = extractKeys({ a: 'x', b: 'y' });
    expect([...s].sort()).toEqual(['a', 'b']);
  });

  it('recurses into nested objects with dotted paths', () => {
    const s = extractKeys({
      prefs: { section: { performance: 'Perf' } },
      error: { ETOOBIG: { title: 'Too big' } },
    });
    expect([...s].sort()).toEqual([
      'error.ETOOBIG.title',
      'prefs.section.performance',
    ]);
  });

  it('treats numbers, booleans, null as leaf values', () => {
    const s = extractKeys({ a: 1, b: false, c: null, d: { e: 'x' } });
    expect([...s].sort()).toEqual(['a', 'b', 'c', 'd.e']);
  });

  it('treats arrays as leaves (no index recursion)', () => {
    const s = extractKeys({ list: ['a', 'b'], nested: { also: [1] } });
    expect([...s].sort()).toEqual(['list', 'nested.also']);
  });
});

describe('diffKeys', () => {
  it('reports no drift when locales match', () => {
    const r = diffKeys({
      en: { a: '1', nested: { b: '2' } },
      de: { a: '1', nested: { b: '2' } },
    });
    expect(r).toHaveLength(2);
    for (const item of r) {
      expect(item.missing).toEqual([]);
      expect(item.extra).toEqual([]);
    }
  });

  it('detects key missing in one locale', () => {
    const r = diffKeys({
      en: { toast: { saved: 'Saved', deleted: 'Deleted' } },
      de: { toast: { saved: 'Gespeichert' } },
      es: { toast: { saved: 'Guardado' } },
    });
    const by = Object.fromEntries(r.map((x) => [x.code, x]));
    expect(by.en.missing).toEqual([]);
    expect(by.de.missing).toEqual(['toast.deleted']);
    expect(by.es.missing).toEqual(['toast.deleted']);
    // toast.deleted exists in en AND is missing in others; not an orphan
    // because it's in the union from en — but it IS orphan-ish (only-en).
    // By our definition extra = present here and absent everywhere else.
    expect(by.en.extra).toEqual(['toast.deleted']);
    expect(by.de.extra).toEqual([]);
    expect(by.es.extra).toEqual([]);
  });

  it('detects orphan key (typo only in one locale)', () => {
    const r = diffKeys({
      en: { ok: 'OK', happy_typo: 'oops' },
      de: { ok: 'OK' },
      es: { ok: 'OK' },
    });
    const by = Object.fromEntries(r.map((x) => [x.code, x]));
    expect(by.en.extra).toEqual(['happy_typo']);
    expect(by.de.missing).toEqual(['happy_typo']);
    expect(by.es.missing).toEqual(['happy_typo']);
  });

  it('sorts missing/extra alphabetically for stable output', () => {
    const r = diffKeys({
      en: { z: '1', a: '2', m: '3' },
      de: {},
    });
    const by = Object.fromEntries(r.map((x) => [x.code, x]));
    expect(by.de.missing).toEqual(['a', 'm', 'z']);
  });

  it('handles three-locale partial drift', () => {
    const r = diffKeys({
      en: { only_en: '1', shared: 'x' },
      de: { only_de: '2', shared: 'x' },
      es: { shared: 'x' },
    });
    const by = Object.fromEntries(r.map((x) => [x.code, x]));
    expect(by.en.extra).toEqual(['only_en']);
    expect(by.de.extra).toEqual(['only_de']);
    expect(by.es.extra).toEqual([]);
    expect(by.es.missing).toEqual(['only_de', 'only_en']);
  });
});
