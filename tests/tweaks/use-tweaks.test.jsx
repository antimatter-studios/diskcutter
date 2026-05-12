import { describe, it, expect } from 'vitest';
import { renderHook, act } from '@testing-library/react';
import { useTweaks } from '../../src/tweaks-panel.jsx';

const defaults = { foo: 1, bar: 'baz', flag: false };

describe('useTweaks', () => {
  it('returns defaults when no value is saved', () => {
    const { result } = renderHook(() => useTweaks(defaults));
    expect(result.current[0]).toEqual(defaults);
  });

  it('persists single-key changes to localStorage', () => {
    const { result } = renderHook(() => useTweaks(defaults));
    act(() => result.current[1]('foo', 9));
    expect(result.current[0].foo).toBe(9);
    const saved = JSON.parse(localStorage.getItem('diskcutter.tweaks'));
    expect(saved.foo).toBe(9);
  });

  it('accepts an object to update multiple keys at once', () => {
    const { result } = renderHook(() => useTweaks(defaults));
    act(() => result.current[1]({ foo: 2, bar: 'qux' }));
    expect(result.current[0]).toEqual({ foo: 2, bar: 'qux', flag: false });
  });

  it('hydrates saved values over defaults on init', () => {
    localStorage.setItem('diskcutter.tweaks', JSON.stringify({ foo: 5 }));
    const { result } = renderHook(() => useTweaks(defaults));
    expect(result.current[0].foo).toBe(5);
    expect(result.current[0].bar).toBe('baz');
  });
});
