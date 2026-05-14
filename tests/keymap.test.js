import { describe, it, expect } from 'vitest';
import { matchShortcut, isEditableTarget, queueReady } from '../src/keymap.js';

const evt = (overrides = {}) => ({
  key: 'o',
  metaKey: false,
  ctrlKey: false,
  ...overrides,
});

describe('matchShortcut', () => {
  it('matches Cmd+O on mac (metaKey)', () => {
    expect(matchShortcut(evt({ key: 'o', metaKey: true }), { key: 'o', mod: true })).toBe(true);
  });

  it('matches Ctrl+O on win/linux (ctrlKey)', () => {
    expect(matchShortcut(evt({ key: 'o', ctrlKey: true }), { key: 'o', mod: true })).toBe(true);
  });

  it('matches Cmd+Return for the start binding', () => {
    expect(matchShortcut(evt({ key: 'Enter', metaKey: true }), { key: 'Enter', mod: true })).toBe(true);
  });

  it('is case-insensitive on key', () => {
    expect(matchShortcut(evt({ key: 'O', metaKey: true }), { key: 'o', mod: true })).toBe(true);
    expect(matchShortcut(evt({ key: 'L', ctrlKey: true }), { key: 'l', mod: true })).toBe(true);
  });

  it('does not match without a modifier when one is required', () => {
    expect(matchShortcut(evt({ key: 'o' }), { key: 'o', mod: true })).toBe(false);
  });

  it('does not match the wrong key', () => {
    expect(matchShortcut(evt({ key: 'p', metaKey: true }), { key: 'o', mod: true })).toBe(false);
  });

  it('rejects modifier present when binding requires none', () => {
    expect(matchShortcut(evt({ key: 'o', metaKey: true }), { key: 'o', mod: false })).toBe(false);
  });

  it('returns false on null/undefined inputs', () => {
    expect(matchShortcut(null, { key: 'o', mod: true })).toBe(false);
    expect(matchShortcut(evt(), null)).toBe(false);
  });
});

describe('isEditableTarget', () => {
  it('returns true for INPUT', () => {
    expect(isEditableTarget({ tagName: 'INPUT' })).toBe(true);
  });

  it('returns true for TEXTAREA', () => {
    expect(isEditableTarget({ tagName: 'TEXTAREA' })).toBe(true);
  });

  it('returns true for SELECT', () => {
    expect(isEditableTarget({ tagName: 'SELECT' })).toBe(true);
  });

  it('returns true for contentEditable elements', () => {
    expect(isEditableTarget({ tagName: 'DIV', isContentEditable: true })).toBe(true);
  });

  it('returns false for DIV', () => {
    expect(isEditableTarget({ tagName: 'DIV' })).toBe(false);
  });

  it('returns false for BUTTON', () => {
    expect(isEditableTarget({ tagName: 'BUTTON' })).toBe(false);
  });

  it('returns false for null/undefined', () => {
    expect(isEditableTarget(null)).toBe(false);
    expect(isEditableTarget(undefined)).toBe(false);
  });
});

describe('queueReady', () => {
  it('is false when not confirmed', () => {
    expect(queueReady([{ state: 'idle', target: { device: '/dev/x' }, validation: 'valid' }], false)).toBe(false);
  });

  it('is false when no idle+target job exists', () => {
    expect(queueReady([{ state: 'idle', target: null }], true)).toBe(false);
    expect(queueReady([{ state: 'writing', target: { device: '/dev/x' } }], true)).toBe(false);
    expect(queueReady([], true)).toBe(false);
  });

  it('is true when confirmed and at least one idle+target job exists', () => {
    expect(queueReady([{ state: 'idle', target: { device: '/dev/x' }, validation: 'valid' }], true)).toBe(true);
  });

  it('tolerates non-array jobs', () => {
    expect(queueReady(null, true)).toBe(false);
    expect(queueReady(undefined, true)).toBe(false);
  });
});

// ---------------------------------------------------------------------------
// Gap-fill: malformed events/bindings, non-string keys, queueReady mixed-state.
// ---------------------------------------------------------------------------

describe('matchShortcut malformed-input handling', () => {
  it('returns false when event.key is not a string', () => {
    expect(matchShortcut({ key: 42, metaKey: true }, { key: 'o', mod: true })).toBe(false);
  });

  it('returns false when binding.key is not a string', () => {
    expect(matchShortcut(evt({ metaKey: true }), { key: null, mod: true })).toBe(false);
  });

  it('matches when both key strings are empty and modifiers align', () => {
    // Both lowercase to '', mod requirement matches → returns true.
    // Documents current behaviour rather than asserting a "right" answer.
    expect(matchShortcut({ key: '', metaKey: false, ctrlKey: false }, { key: '', mod: false })).toBe(true);
  });

  it('treats metaKey and ctrlKey as interchangeable mod indicators', () => {
    // Both set is still "mod present".
    expect(matchShortcut(evt({ key: 'o', metaKey: true, ctrlKey: true }), { key: 'o', mod: true })).toBe(true);
  });
});

describe('isEditableTarget additional cases', () => {
  it('returns true for LOWERCASE input tag? (no — tagName is uppercase per DOM)', () => {
    // DOM tagName is always uppercase, so a lowercase 'input' should NOT match.
    expect(isEditableTarget({ tagName: 'input' })).toBe(false);
  });

  it('returns true when contentEditable is set even on INPUT', () => {
    // INPUT path matches first; contentEditable adds nothing but shouldn't break.
    expect(isEditableTarget({ tagName: 'INPUT', isContentEditable: false })).toBe(true);
  });

  it('returns false for A (anchor) and SPAN', () => {
    expect(isEditableTarget({ tagName: 'A' })).toBe(false);
    expect(isEditableTarget({ tagName: 'SPAN' })).toBe(false);
  });
});

describe('queueReady additional cases', () => {
  it('is true when at least one idle+target job is mixed with non-ready jobs', () => {
    const jobs = [
      { state: 'writing', target: { device: '/dev/x' } },
      { state: 'idle', target: null },
      { state: 'idle', target: { device: '/dev/y' }, validation: 'valid' },
    ];
    expect(queueReady(jobs, true)).toBe(true);
  });

  it('tolerates null entries in the jobs array', () => {
    expect(queueReady([null, { state: 'idle', target: { device: '/dev/x' }, validation: 'valid' }], true)).toBe(true);
    expect(queueReady([null, undefined], true)).toBe(false);
  });

  it('rejects success or error states even with target attached', () => {
    expect(queueReady([{ state: 'success', target: { device: '/dev/x' } }], true)).toBe(false);
    expect(queueReady([{ state: 'error', target: { device: '/dev/x' } }], true)).toBe(false);
  });

  it('rejects jobs whose validation is still pending or invalid', () => {
    expect(queueReady([{ state: 'idle', target: { device: '/dev/x' }, validation: 'pending' }], true)).toBe(false);
    expect(queueReady([{ state: 'idle', target: { device: '/dev/x' }, validation: 'invalid' }], true)).toBe(false);
  });
});
