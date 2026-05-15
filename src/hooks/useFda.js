import { useCallback, useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { getCurrentWindow } from '@tauri-apps/api/window';

// useFda — encapsulates the macOS Full Disk Access plumbing that used to
// live inline in App.jsx. Owns:
//   - `blocked` state (true when FDA is not granted)
//   - the initial check_fda probe at mount
//   - re-probing on window focus (BOTH the DOM 'focus' event AND Tauri's
//     native 'tauri://focus' window event — the DOM one doesn't fire
//     reliably on macOS Tauri when the user returns from System Settings,
//     which is exactly the moment we need to re-probe)
//   - a manual `recheck()` for explicit user-driven re-probe (the
//     "RE-CHECK FDA" button in the error strip)
//   - `handleFdaError()` for ENEEDS_FDA helper errors: flip blocked,
//     auto-open System Settings ONCE per outage (de-duped via ref),
//     reset the de-dup flag the moment FDA flips back to granted.
//
// `onGranted` fires the instant the probe sees blocked → unblocked; App.jsx
// uses it to clear stale ENEEDS_FDA error rows from the queue.
export function useFda({ onGranted, onOpenSettingsError } = {}) {
  const [blocked, setBlocked] = useState(false);
  // True once we've kicked System Settings open for the current FDA outage.
  // Cleared when check_fda reports granted again, so the next outage opens it.
  const settingsOpenedRef = useRef(false);
  // Keep callbacks in refs so the long-lived focus listeners always reach
  // the latest closure without re-subscribing on every render.
  const onGrantedRef = useRef(onGranted);
  const onOpenSettingsErrorRef = useRef(onOpenSettingsError);
  useEffect(() => { onGrantedRef.current = onGranted; }, [onGranted]);
  useEffect(() => { onOpenSettingsErrorRef.current = onOpenSettingsError; }, [onOpenSettingsError]);

  const probe = useCallback(() => {
    invoke('check_fda').then((granted) => {
      setBlocked(!granted);
      if (granted) {
        settingsOpenedRef.current = false;
        onGrantedRef.current?.();
      }
    }).catch(() => {});
  }, []);

  // Initial probe at mount.
  useEffect(() => { probe(); }, [probe]);

  // Focus probe — re-run check_fda whenever the user comes back to the
  // window. Wire BOTH the DOM 'focus' event and Tauri's native
  // 'tauri://focus' window event because the DOM one is unreliable on
  // macOS Tauri (it often doesn't fire returning from System Settings,
  // which leaves FDA-blocked jobs stuck).
  useEffect(() => {
    const onFocus = () => probe();
    window.addEventListener('focus', onFocus);
    let unlistenTauri = null;
    let cancelled = false;
    getCurrentWindow().listen('tauri://focus', onFocus).then((u) => {
      if (cancelled) u();
      else unlistenTauri = u;
    }).catch(() => {});
    return () => {
      cancelled = true;
      window.removeEventListener('focus', onFocus);
      if (unlistenTauri) unlistenTauri();
    };
  }, [probe]);

  // Public on-demand re-probe (used by the RE-CHECK FDA escape hatch).
  const recheck = useCallback(() => probe(), [probe]);

  // Helper-reported ENEEDS_FDA: flip blocked + auto-open System Settings
  // once per outage. The de-dup ref clears on the next granted probe.
  const handleFdaError = useCallback(() => {
    setBlocked(true);
    if (!settingsOpenedRef.current) {
      settingsOpenedRef.current = true;
      invoke('open_fda_settings').catch((err) => {
        console.error('open_fda_settings failed', err);
        onOpenSettingsErrorRef.current?.(err);
      });
    }
  }, []);

  return { blocked, recheck, handleFdaError };
}

export default useFda;
