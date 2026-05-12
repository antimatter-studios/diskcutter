import { describe, it, expect, vi } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/react';
import { DangerBanner } from '../../src/components.jsx';

describe('DangerBanner', () => {
  it('does not render when no job has a target', () => {
    const { container } = render(
      <DangerBanner confirmed={false} onConfirm={() => {}} jobs={[{ state: 'idle' }]} accent="#f00" />,
    );
    expect(container.firstChild).toBeNull();
  });

  it('renders headline area when at least one job has a target', () => {
    render(
      <DangerBanner
        confirmed={false}
        onConfirm={() => {}}
        jobs={[{ target: { device: '/dev/disk5' } }, { target: { device: '/dev/disk6' } }]}
        accent="#f00"
      />,
    );
    expect(document.querySelector('.banner-headline')).toBeInTheDocument();
  });

  it('toggles confirmation through the checkbox', () => {
    const onConfirm = vi.fn();
    render(
      <DangerBanner
        confirmed={false}
        onConfirm={onConfirm}
        jobs={[{ target: { device: '/dev/disk5' } }]}
        accent="#f00"
      />,
    );
    fireEvent.click(screen.getByRole('checkbox'));
    expect(onConfirm).toHaveBeenCalledWith(true);
  });

  it('reflects confirmed prop in checkbox state', () => {
    render(
      <DangerBanner
        confirmed
        onConfirm={() => {}}
        jobs={[{ target: { device: '/dev/disk5' } }]}
        accent="#f00"
      />,
    );
    expect(screen.getByRole('checkbox')).toBeChecked();
  });
});
