import { describe, it, expect, vi } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/react';
import { Sidebar } from '../../src/components.jsx';

const j = (state) => ({ id: state, state });

describe('Sidebar', () => {
  it('counts total jobs in the queue and pads to two digits', () => {
    const jobs = [
      j('idle'), j('writing'),
      j('success'), j('success'), j('success'),
      j('error'),
    ];
    render(<Sidebar active="queue" onSelect={() => {}} jobs={jobs} accent="#f00" />);
    const counts = document.querySelectorAll('.side-count');
    expect(counts[0]).toHaveTextContent('06');
  });

  it('invokes onSelect with key when a nav item is clicked', () => {
    const onSelect = vi.fn();
    render(<Sidebar active="queue" onSelect={onSelect} jobs={[]} accent="#f00" />);
    fireEvent.click(screen.getByText('LOGS'));
    expect(onSelect).toHaveBeenCalledWith('logs');
  });

  it('renders session stats when provided', () => {
    render(
      <Sidebar
        active="queue"
        onSelect={() => {}}
        jobs={[]}
        accent="#f00"
        sessionStats={{ session: '1h 02m', written: '5 GB', avg: '120 MB/s' }}
      />,
    );
    expect(screen.getByText('1h 02m')).toBeInTheDocument();
    expect(screen.getByText('5 GB')).toBeInTheDocument();
    expect(screen.getByText('120 MB/s')).toBeInTheDocument();
  });

  it('marks the active nav item with the tick glyph', () => {
    render(<Sidebar active="logs" onSelect={() => {}} jobs={[]} accent="#f00" />);
    const ticks = document.querySelectorAll('.side-tick');
    const active = Array.from(ticks).filter((el) => el.textContent === '▶');
    expect(active.length).toBe(1);
  });
});
