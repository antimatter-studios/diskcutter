import { describe, it, expect, vi } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/react';
import { DiskPickerSheet } from '../../src/components.jsx';

const disks = [
  {
    device: '/dev/disk1',
    model: 'INTERNAL NVME',
    capacity: '1 TB',
    bytes: 1e12,
    bus: 'NVME',
    partitions: 'APFS',
    flags: ['INTERNAL'],
  },
  {
    device: '/dev/disk5',
    model: 'GENERIC USB',
    capacity: '16 GB',
    bytes: 16e9,
    bus: 'USB',
    partitions: 'UNFORMATTED',
    flags: ['REMOVABLE'],
  },
  {
    device: '/dev/disk6',
    model: 'TINY USB',
    capacity: '512 MB',
    bytes: 512e6,
    bus: 'USB',
    partitions: 'UNFORMATTED',
    flags: ['REMOVABLE'],
  },
];
const jobImage = { name: 'big.iso', bytes: 4e9 };

const baseProps = {
  disks,
  jobImage,
  onPick: () => {},
  onClose: () => {},
  onRefresh: () => {},
  accent: '#f00',
};

describe('DiskPickerSheet', () => {
  it('renders nothing when closed', () => {
    const { container } = render(<DiskPickerSheet {...baseProps} open={false} />);
    expect(container.firstChild).toBeNull();
  });

  it('lists all supplied disks', () => {
    render(<DiskPickerSheet {...baseProps} open />);
    expect(screen.getByText('INTERNAL NVME')).toBeInTheDocument();
    expect(screen.getByText('GENERIC USB')).toBeInTheDocument();
    expect(screen.getByText('TINY USB')).toBeInTheDocument();
  });

  it('refuses to pick internal or too-small disks', () => {
    const onPick = vi.fn();
    render(<DiskPickerSheet {...baseProps} open onPick={onPick} />);
    fireEvent.click(screen.getByText('INTERNAL NVME').closest('button'));
    fireEvent.click(screen.getByText('TINY USB').closest('button'));
    expect(onPick).not.toHaveBeenCalled();
  });

  it('picks an eligible disk', () => {
    const onPick = vi.fn();
    render(<DiskPickerSheet {...baseProps} open onPick={onPick} />);
    fireEvent.click(screen.getByText('GENERIC USB').closest('button'));
    expect(onPick).toHaveBeenCalledWith(expect.objectContaining({ device: '/dev/disk5' }));
  });

  it('invokes onClose when the X is clicked', () => {
    const onClose = vi.fn();
    render(<DiskPickerSheet {...baseProps} open disks={[]} jobImage={null} onClose={onClose} />);
    fireEvent.click(screen.getByText('✕'));
    expect(onClose).toHaveBeenCalled();
  });

  it('invokes onRefresh from the refresh control', () => {
    const onRefresh = vi.fn(() => Promise.resolve());
    render(
      <DiskPickerSheet
        {...baseProps}
        open
        disks={[]}
        jobImage={null}
        onRefresh={onRefresh}
      />,
    );
    // The sheet footer has both a "refreshed Ns ago" label and a "REFRESH" link.
    // The link is a .picker-link button; pick that explicitly.
    const refreshLink = Array.from(document.querySelectorAll('.picker-link'))
      .find((b) => /REFRESH/i.test(b.textContent));
    fireEvent.click(refreshLink);
    expect(onRefresh).toHaveBeenCalled();
  });
});
