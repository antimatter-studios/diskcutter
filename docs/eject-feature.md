# Eject feature

## Status: stubbed, not implemented

The UI exposes eject affordances and the doctor probes for a backend, but no
`eject_disk` Tauri command exists yet. Both the button and the auto-eject
toggle are dead wiring until that command lands.

## UI surface

- **Job detail "EJECT" button** — [src/components.jsx:366](../src/components.jsx#L366).
  Rendered on a successful job. No `onClick` handler — it does nothing when
  clicked.
- **`auto.eject` preference toggle** — [src/components.jsx:652](../src/components.jsx#L652),
  default value at [src/components.jsx:678](../src/components.jsx#L678) (`'false'`).
  Persisted via `config_set` like any other pref.
- **Doctor panel mapping** — [src/components.jsx:717](../src/components.jsx#L717)
  surfaces the backend probe's `eject` check under
  `doctor.check.eject`.

## Pref wiring (no-op)

[src/App.jsx:154-157](../src/App.jsx#L154-L157):

```js
if (key === 'auto.eject' && v === 'true') {
  // TODO: invoke('eject_disk', { device }) once the backend ships it.
  console.warn('auto.eject enabled, but eject_disk command is not implemented yet');
}
```

Flipping the toggle logs a warning and nothing else.

## Doctor backend probe

[src-tauri/src/doctor.rs:144-192](../src-tauri/src/doctor.rs#L144-L192),
wired into `run_all` at [src-tauri/src/doctor.rs:277](../src-tauri/src/doctor.rs#L277).

| Platform | Backend lookup | Result |
| --- | --- | --- |
| macOS | `diskutil` | pass if present; fail if missing (built-in, so fail = PATH misconfigured) |
| Linux | `udisksctl` (preferred) → `eject(1)` (fallback) | pass / warn / fail |
| Other | — | warn: "not implemented on this platform yet" |

Test coverage: [src-tauri/src/doctor.rs:383-385](../src-tauri/src/doctor.rs#L383-L385)
asserts the check is included in `run_all`.

## Adjacent reference

The raw writer suggests Finder-eject as a manual recovery path when the disk
is held by another process —
[src-tauri/src/writers/raw.rs:134](../src-tauri/src/writers/raw.rs#L134).
Unrelated to auto-eject; just user-facing copy.

## What's missing to ship the feature

1. **Tauri command** `eject_disk(device: String)` registered in the
   `invoke_handler`.
2. **Platform shell-out:**
   - macOS: `diskutil eject <device>`
   - Linux: `udisksctl power-off -b <device>` (preferred), falling back to
     `eject <device>`
   - Other: return a "not supported" error.
3. **Wire-up:**
   - Replace the `console.warn` at [src/App.jsx:154-157](../src/App.jsx#L154-L157)
     with `invoke('eject_disk', { device })` after a successful burn when
     `auto.eject === 'true'`.
   - Add `onClick={() => invoke('eject_disk', { device: job.device })}` to
     the EJECT button at [src/components.jsx:366](../src/components.jsx#L366).
4. **Error surfacing** via `pushToast('error', …)` on failure.

## i18n keys already in place

- `detail.actions.eject` — button label (en: `EJECT`)
- `prefs.auto_eject` — preference label (en: `AUTO EJECT`)
- `doctor.check.eject` — doctor row title (en: `Eject backend`)
- Translations exist in `en.json`, `de.json`, `es.json`.
