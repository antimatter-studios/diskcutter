import { describe, it, expect, vi } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/react';
import { Toolbar } from '../../src/components.jsx';

const base = {
  onAdd: () => {},
  onClearDone: () => {},
  jobs: [],
  accent: '#f00',
};

describe('Toolbar', () => {
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
});
