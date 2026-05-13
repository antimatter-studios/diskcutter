// Pure helpers for the transient toast queue rendered in App.jsx.
//
// Toasts are app-level, time-bounded notifications. They are distinct from
// the persistent per-job `.error-strip` (which lives until the user clears
// the failed job) — toasts cover transient failures that don't belong to a
// single job: failed list_disks, config writes, drop-rejected files, etc.
//
// Shape of a toast object:
//   { id: string|number, level: 'info'|'warn'|'error', message: string, expiresAt: number }
//
// All exports are pure so they can be exercised without a DOM.

// Maximum number of toasts retained in the queue. Anything older falls off.
// Eight is enough to surface a flurry without becoming an unreadable wall.
export const MAX_TOASTS = 8;

// Prepend a new toast and cap the list at MAX_TOASTS. Newest-first ordering
// matches the visual stack (top of the column is most recent).
export function addToast(list, toast) {
  const safeList = Array.isArray(list) ? list : [];
  return [toast, ...safeList].slice(0, MAX_TOASTS);
}

// Drop toasts whose expiresAt has already passed `now`. `now` is injected so
// callers (and tests) control the clock.
export function reapExpired(list, now) {
  const safeList = Array.isArray(list) ? list : [];
  return safeList.filter((t) => t && t.expiresAt > now);
}

// Remove a single toast by id (for the manual ✕ button).
export function dismissToast(list, id) {
  const safeList = Array.isArray(list) ? list : [];
  return safeList.filter((t) => t && t.id !== id);
}

// Errors stay long enough to read and copy; info/warn are quicker.
export function defaultTtlMs(level) {
  return level === 'error' ? 8000 : 4000;
}
