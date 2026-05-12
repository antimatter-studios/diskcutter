// Audio cues — small, deliberately-cheap WebAudio synth. No samples, no
// network, ~30 lines of oscillator setup per cue. Designed for the
// "burning a stack of USBs across the room" workflow: distinct tones per
// phase so you don't have to look at the screen to know what stage a
// burn is at.
//
// Every public function is a no-op when `sound.enabled !== 'true'` in
// the persisted prefs (default 'false' so we don't surprise users).
// Call `playUnlock()` once after a user gesture (any button click) to
// satisfy autoplay policies; otherwise iOS/Safari mute everything.

let ctx = null;
let unlocked = false;

function getCtx() {
  if (typeof window === 'undefined') return null;
  if (!ctx) {
    const C = window.AudioContext || window.webkitAudioContext;
    if (!C) return null;
    try { ctx = new C(); } catch (e) { return null; }
  }
  return ctx;
}

// Call from a click/keydown handler the first time the user interacts.
// Resumes a suspended context (autoplay policy) so subsequent cues play.
export function playUnlock() {
  const c = getCtx();
  if (!c) return;
  if (c.state === 'suspended') {
    c.resume().catch(() => {});
  }
  unlocked = true;
}

function tone(c, { freq, dur, type = 'sine', gain = 0.08, attack = 0.01, release = 0.06, when = 0 }) {
  const osc = c.createOscillator();
  const g = c.createGain();
  osc.type = type;
  osc.frequency.setValueAtTime(freq, c.currentTime + when);
  g.gain.setValueAtTime(0.0001, c.currentTime + when);
  g.gain.exponentialRampToValueAtTime(gain, c.currentTime + when + attack);
  g.gain.exponentialRampToValueAtTime(0.0001, c.currentTime + when + dur - release);
  osc.connect(g).connect(c.destination);
  osc.start(c.currentTime + when);
  osc.stop(c.currentTime + when + dur);
}

function shouldPlay(prefs) {
  if (!prefs) return false;
  return String(prefs['sound.enabled']) === 'true';
}

// Short low-mid blip at the start of each write. ~120ms.
export function playWriteStart(prefs) {
  if (!shouldPlay(prefs)) return;
  const c = getCtx();
  if (!c) return;
  tone(c, { freq: 220, dur: 0.12, type: 'square', gain: 0.06 });
}

// Slightly higher blip when verify phase begins. ~120ms.
export function playVerifyStart(prefs) {
  if (!shouldPlay(prefs)) return;
  const c = getCtx();
  if (!c) return;
  tone(c, { freq: 440, dur: 0.12, type: 'square', gain: 0.06 });
}

// Three-note ascending major chime on verify match. ~600ms total.
export function playSuccess(prefs) {
  if (!shouldPlay(prefs)) return;
  const c = getCtx();
  if (!c) return;
  // C5, E5, G5
  tone(c, { freq: 523.25, dur: 0.18, when: 0.00, gain: 0.07 });
  tone(c, { freq: 659.25, dur: 0.18, when: 0.18, gain: 0.07 });
  tone(c, { freq: 783.99, dur: 0.32, when: 0.36, gain: 0.09 });
}

// Descending minor-second sting on hard failure. ~400ms total.
// Square wave on purpose — buzzy, attention-grabbing.
export function playError(prefs) {
  if (!shouldPlay(prefs)) return;
  const c = getCtx();
  if (!c) return;
  tone(c, { freq: 311.13, dur: 0.18, when: 0.00, type: 'sawtooth', gain: 0.08 });
  tone(c, { freq: 233.08, dur: 0.28, when: 0.18, type: 'sawtooth', gain: 0.08 });
}

// Cancellation = single mid-low click. ~80ms.
export function playCancel(prefs) {
  if (!shouldPlay(prefs)) return;
  const c = getCtx();
  if (!c) return;
  tone(c, { freq: 180, dur: 0.08, type: 'triangle', gain: 0.05 });
}

// Test helper — exposed so a "play sample" button in prefs can demo the
// whole sound bank in sequence.
export function playSampleSequence(prefs) {
  if (!shouldPlay(prefs)) return;
  const c = getCtx();
  if (!c) return;
  tone(c, { freq: 220, dur: 0.12, when: 0.00, type: 'square', gain: 0.06 });
  tone(c, { freq: 440, dur: 0.12, when: 0.30, type: 'square', gain: 0.06 });
  tone(c, { freq: 523.25, dur: 0.16, when: 0.60 });
  tone(c, { freq: 659.25, dur: 0.16, when: 0.76 });
  tone(c, { freq: 783.99, dur: 0.30, when: 0.92 });
}

// Diagnostics for the test suite.
export function _hasUnlocked() {
  return unlocked;
}

export function _resetForTests() {
  ctx = null;
  unlocked = false;
}
