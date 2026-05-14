// Pure helpers for keyboard shortcut dispatching. Kept side-effect-free and
// DOM-light so they can be exercised by unit tests without a real `window`.
//
// `matchShortcut` answers "does this keydown event match this binding?"
// `isEditableTarget` answers "is the user typing into a text input?" — used to
// suppress global shortcuts while the focus is in an editable surface so we
// don't swallow keystrokes the user expects to land in a form field.

export function matchShortcut(event, binding) {
  if (!event || !binding) return false;
  const wantMod = !!binding.mod;
  const hasMod = !!(event.metaKey || event.ctrlKey);
  if (wantMod !== hasMod) return false;
  const eventKey = typeof event.key === 'string' ? event.key.toLowerCase() : '';
  const bindKey = typeof binding.key === 'string' ? binding.key.toLowerCase() : '';
  return eventKey === bindKey;
}

export function isEditableTarget(target) {
  if (!target) return false;
  const tag = target.tagName;
  if (tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT') return true;
  if (target.isContentEditable) return true;
  return false;
}

// Toolbar's `ready` predicate, mirrored here so the keyboard handler can
// gate Cmd+Return without reaching into the component. Keep this in sync
// with components.jsx's Toolbar.
export function queueReady(jobs, confirmed) {
  if (!confirmed) return false;
  if (!Array.isArray(jobs)) return false;
  return jobs.some((j) => j && j.state === 'idle' && j.target && j.validation === 'valid');
}
