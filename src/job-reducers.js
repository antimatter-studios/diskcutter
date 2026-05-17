import { formatBps, formatDuration } from './format.js';

export function applyJobUpdate(jobs, u) {
  return jobs.map((j) => {
    if (j.id !== u.job_id) return j;
    if (u.state === 'writing') {
      // Stamp startedAt on the transition INTO writing so the row can show a
      // live elapsed counter alongside the ETA. Preserved across subsequent
      // 'writing' progress events and across the writing→verifying handoff so
      // the elapsed reflects the whole job run, not just the current phase.
      const startedAt = (j.state === 'writing' || j.state === 'verifying') ? j.startedAt : Date.now();
      return { ...j, state: 'writing', progress: u.progress, speed: u.speed, eta: u.eta, startedAt };
    }
    if (u.state === 'verifying') {
      return { ...j, state: 'verifying', verifyProgress: u.progress, speed: u.speed, eta: u.eta };
    }
    return j;
  });
}

export function applyJobComplete(jobs, c) {
  return jobs.map((j) => {
    if (j.id !== c.job_id) return j;
    const match = c.verify_match;
    const sectors = Math.floor(c.bytes_written / 512);
    return {
      ...j,
      state: match ? 'success' : 'error',
      progress: 100,
      verifyProgress: 100,
      errorCode: match ? undefined : 'EHASHMISMATCH',
      elapsed: formatDuration(c.elapsed_ms),
      speed: formatBps(c.avg_write_bps),
      bytesWritten: c.bytes_written,
      avgBps: c.avg_write_bps,
      verification: {
        sourceHash: c.source_sha256,
        readHash: c.readback_sha256,
        match,
        checked: sectors,
        total: sectors,
        mismatches: (c.mismatches || []).map((m) => ({ ...m, note: m.note || '' })),
        throughput: `${formatBps(c.avg_verify_bps)} avg`,
      },
    };
  });
}

export function applyJobFailure(jobs, f) {
  return jobs.map((j) => (
    j.id !== f.job_id ? j : { ...j, state: 'error', errorCode: f.error_code, errorMessage: f.error_message }
  ));
}
