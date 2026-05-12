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
    expect(queueReady([{ state: 'idle', target: { device: '/dev/x' } }], false)).toBe(false);
  });

  it('is false when no idle+target job exists', () => {
    expect(queueReady([{ state: 'idle', target: null }], true)).toBe(false);
    expect(queueReady([{ state: 'writing', target: { device: '/dev/x' } }], true)).toBe(false);
    expect(queueReady([], true)).toBe(false);
  });

  it('is true when confirmed and at least one idle+target job exists', () => {
    expect(queueReady([{ state: 'idle', target: { device: '/dev/x' } }], true)).toBe(true);
  });

  it('tolerates non-array jobs', () => {
    expect(queueReady(null, true)).toBe(false);
    expect(queueReady(undefined, true)).toBe(false);
  });
});
