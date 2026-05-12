import { formatBps, formatBytes, formatSession } from './format.js';

export function computeScene(jobs, pickerJob, errorJob) {
  if (pickerJob !== null) return 'diskpicker';
  if (jobs.length === 0) return 'empty';
  if (jobs.some((j) => j.state === 'writing')) return 'writing';
  if (jobs.some((j) => j.state === 'verifying')) return 'verifying';
  if (errorJob) return 'error';
  if (jobs.every((j) => j.state === 'success')) return 'success';
  return 'idle';
}

export function sceneToTitleKey(scene, verbose) {
  if (!verbose) return 'app.title_short';
  switch (scene) {
    case 'success': return 'app.title_complete';
    case 'error': return 'app.title_error';
    case 'writing': return 'app.title_writing';
    case 'verifying': return 'app.title_verifying';
    default: return 'app.title_ready';
  }
}

export function planStart(jobs) {
  const ready = jobs.filter((j) => j.state === 'idle' && j.target);
  const tooSmall = ready.filter((j) => j.target.bytes && j.image.bytes && j.target.bytes < j.image.bytes);
  const okToBurn = ready.filter((j) => !tooSmall.includes(j));
  return { ready, tooSmall, okToBurn };
}

export function computeSessionStats(jobs, sessionStartMs, nowMs) {
  const completed = jobs.filter((j) => typeof j.bytesWritten === 'number');
  const totalWritten = completed.reduce((s, j) => s + (j.bytesWritten || 0), 0);
  const avgBps = completed.length
    ? completed.reduce((s, j) => s + (j.avgBps || 0), 0) / completed.length
    : 0;
  return {
    session: formatSession(nowMs - sessionStartMs),
    written: totalWritten > 0 ? formatBytes(totalWritten) : '—',
    avg: avgBps > 0 ? formatBps(avgBps) : '—',
  };
}
