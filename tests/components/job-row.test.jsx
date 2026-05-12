import { describe, it, expect, vi } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/react';
import { JobRow } from '../../src/components.jsx';

const image = {
  name: 'boot.iso',
  path: '/tmp/boot.iso',
  size: '4 GB',
  bytes: 4_000_000_000,
  sectors: 7_812_500,
  format: 'ISO 9660 / RAW',
  sha256: 'abcdef0123456789'.repeat(4),
};

const target = {
  device: '/dev/disk5',
  model: 'GENERIC USB',
  capacity: '16 GB',
  bytes: 16_000_000_000,
  bus: 'USB',
  partitions: 'UNFORMATTED',
};

const base = {
  num: 1,
  image,
  target,
  state: 'idle',
  progress: 0,
  verifyProgress: 0,
  speed: '—',
  eta: '—',
  elapsed: '—',
};

const props = (job, overrides = {}) => ({
  job,
  accent: '#f00',
  density: 'comfy',
  expanded: false,
  onToggle: () => {},
  onSelectTarget: () => {},
  onCancel: () => {},
  onRetry: () => {},
  onCopyHash: () => {},
  onCopyError: () => {},
  onFlashAnother: () => {},
  ...overrides,
});

describe('JobRow', () => {
  it('renders the job number zero-padded to two digits', () => {
    render(<JobRow {...props({ ...base, num: 3 })} />);
    expect(screen.getByText('#03')).toBeInTheDocument();
  });

  it('shows pick-target affordance when no target is set', () => {
    const onSelectTarget = vi.fn();
    render(<JobRow {...props({ ...base, target: null }, { onSelectTarget })} />);
    fireEvent.click(screen.getByText(/SELECT TARGET/i));
    expect(onSelectTarget).toHaveBeenCalled();
  });

  it('renders writing progress with percentage and speed', () => {
    render(
      <JobRow
        {...props({ ...base, state: 'writing', progress: 42.5, speed: '120 MB/s' })}
      />,
    );
    expect(screen.getByText('42.5%')).toBeInTheDocument();
    expect(screen.getByText('120 MB/s')).toBeInTheDocument();
  });

  it('renders verify progress when state is verifying', () => {
    render(
      <JobRow {...props({ ...base, state: 'verifying', verifyProgress: 12.0, speed: '90 MB/s' })} />,
    );
    expect(screen.getByText('12.0%')).toBeInTheDocument();
  });

  it('shows the error code chip when state is error', () => {
    render(<JobRow {...props({ ...base, state: 'error', errorCode: 'EIO' })} />);
    expect(screen.getByText('EIO')).toBeInTheDocument();
  });

  it('fires onToggle when the chevron is clicked', () => {
    const onToggle = vi.fn();
    render(<JobRow {...props(base, { onToggle })} />);
    fireEvent.click(screen.getByText('▶'));
    expect(onToggle).toHaveBeenCalled();
  });
});
