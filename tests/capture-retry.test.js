import { describe, it, expect, beforeEach } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { useQueueStore } from '../src/store/queue.js';

const SOURCE = { device: '/dev/disk7', model: 'SANDISK ULTRA', bytes: 1000 };

function seedCaptureRow(state, extra = {}) {
  useQueueStore.setState({
    captureRows: {
      cap1: {
        id: 'cap1',
        source: SOURCE,
        outputPath: '/tmp/out.img',
        state,
        progress: 0,
        errorCode: 'EIO',
        errorMessage: 'read failed',
        ...extra,
      },
    },
    captureOrder: ['cap1'],
  });
}

describe('capture retry', () => {
  beforeEach(() => {
    invoke.mockClear();
  });

  it('retries a failed capture: resets to reading, clears the error, invokes start_capture once', async () => {
    seedCaptureRow('error');
    await useQueueStore.getState().startCapture('cap1');

    const row = useQueueStore.getState().captureRows.cap1;
    expect(row.state).toBe('reading');
    expect(row.errorCode).toBeNull();
    expect(row.errorMessage).toBeNull();
    expect(invoke).toHaveBeenCalledTimes(1);
    expect(invoke).toHaveBeenCalledWith(
      'start_capture',
      expect.objectContaining({ captureId: 'cap1', outputPath: '/tmp/out.img' }),
    );
  });

  it('ignores a re-entrant start while a capture is already reading (no stacked invoke)', async () => {
    seedCaptureRow('reading');
    await useQueueStore.getState().startCapture('cap1');
    expect(invoke).not.toHaveBeenCalled();
  });

  it('does nothing when source or output path is missing', async () => {
    seedCaptureRow('error', { source: null, outputPath: null });
    await useQueueStore.getState().startCapture('cap1');
    expect(invoke).not.toHaveBeenCalled();
  });
});
