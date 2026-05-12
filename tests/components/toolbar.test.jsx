import { describe, it, expect, vi } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/react';
import { Toolbar } from '../../src/components.jsx';

const base = {
  onAdd: () => {},
  onStart: () => {},
  onClearDone: () => {},
  confirmed: false,
  jobs: [],
  accent: '#f00',
  busy: false,
  density: 'comfy',
  onDensity: () => {},
};

describe('Toolbar', () => {
  it('enables Start when confirmed AND an idle job has a target', () => {
    const onStart = vi.fn();
    render(
      <Toolbar
        {...base}
        confirmed
        onStart={onStart}
        jobs={[{ state: 'idle', target: { device: '/dev/disk5' } }]}
      />,
    );
    fireEvent.click(screen.getByText(/START QUEUE/i));
    expect(onStart).toHaveBeenCalled();
  });

  it('does not fire Start when not confirmed', () => {
    const onStart = vi.fn();
    render(
      <Toolbar
        {...base}
        onStart={onStart}
        jobs={[{ state: 'idle', target: { device: '/dev/disk5' } }]}
      />,
    );
    fireEvent.click(screen.getByText(/START QUEUE/i));
    expect(onStart).not.toHaveBeenCalled();
  });

  it('shows the busy label when busy', () => {
    render(<Toolbar {...base} confirmed busy />);
    expect(screen.getByText(/RUNNING|▣/)).toBeInTheDocument();
  });

  it('clear-done does not fire when no successful jobs exist', () => {
    const onClearDone = vi.fn();
    render(<Toolbar {...base} onClearDone={onClearDone} />);
    fireEvent.click(screen.getByText(/CLEAR DONE/i));
    expect(onClearDone).not.toHaveBeenCalled();
  });

  it('clear-done fires when at least one success exists', () => {
    const onClearDone = vi.fn();
    render(<Toolbar {...base} onClearDone={onClearDone} jobs={[{ state: 'success' }]} />);
    fireEvent.click(screen.getByText(/CLEAR DONE/i));
    expect(onClearDone).toHaveBeenCalled();
  });

  it('Add Image fires its handler', () => {
    const onAdd = vi.fn();
    render(<Toolbar {...base} onAdd={onAdd} />);
    fireEvent.click(screen.getByText(/ADD IMAGE/i));
    expect(onAdd).toHaveBeenCalled();
  });

  it('density toggle invokes onDensity with the selected mode', () => {
    const onDensity = vi.fn();
    const { container } = render(<Toolbar {...base} onDensity={onDensity} />);
    const compactBtn = container.querySelector('.density-toggle button');
    fireEvent.click(compactBtn);
    expect(onDensity).toHaveBeenCalledWith('compact');
  });
});
