import { describe, it, expect, vi } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/react';
import { TweakToggle, TweakRadio, TweakSelect, TweakButton } from '../../src/tweaks-panel.jsx';

describe('TweakToggle', () => {
  it('reflects value through aria-checked', () => {
    render(<TweakToggle label="X" value onChange={() => {}} />);
    expect(screen.getByRole('switch')).toHaveAttribute('aria-checked', 'true');
  });

  it('inverts value on click', () => {
    const onChange = vi.fn();
    render(<TweakToggle label="X" value={false} onChange={onChange} />);
    fireEvent.click(screen.getByRole('switch'));
    expect(onChange).toHaveBeenCalledWith(true);
  });
});

describe('TweakRadio', () => {
  it('renders segment buttons for short labels and emits the underlying value', () => {
    const onChange = vi.fn();
    const { container } = render(
      <TweakRadio
        label="L"
        value="a"
        options={[{ value: 'a', label: 'A' }, { value: 'b', label: 'B' }]}
        onChange={onChange}
      />,
    );
    // TweakRadio uses pointerdown on the track, not click on the buttons —
    // simulate a press inside the second segment's region.
    const track = container.querySelector('[role="radiogroup"]');
    // happy-dom returns a zero-sized rect. segAt's math then maps clientX=0 to
    // segment index 1 (value 'b'), which is the unselected option here.
    fireEvent.pointerDown(track, { clientX: 0 });
    expect(onChange).toHaveBeenCalledWith('b');
  });

  it('falls back to a select when labels overflow', () => {
    const longOpts = [
      { value: 'opt-a', label: 'A REALLY LONG LABEL THAT WILL NOT FIT' },
      { value: 'opt-b', label: 'ANOTHER ONE THAT IS ALSO WAY TOO LONG' },
    ];
    render(<TweakRadio label="L" value="opt-a" options={longOpts} onChange={() => {}} />);
    expect(document.querySelector('select')).toBeInTheDocument();
  });
});

describe('TweakSelect', () => {
  it('emits the new value on change', () => {
    const onChange = vi.fn();
    render(
      <TweakSelect
        label="L"
        value="a"
        options={[{ value: 'a', label: 'A' }, { value: 'b', label: 'B' }]}
        onChange={onChange}
      />,
    );
    fireEvent.change(document.querySelector('select'), { target: { value: 'b' } });
    expect(onChange).toHaveBeenCalledWith('b');
  });
});

describe('TweakButton', () => {
  it('fires onClick when pressed', () => {
    const onClick = vi.fn();
    render(<TweakButton label="DO IT" onClick={onClick} />);
    fireEvent.click(screen.getByText('DO IT'));
    expect(onClick).toHaveBeenCalled();
  });

  it('applies secondary styling when requested', () => {
    render(<TweakButton label="X" onClick={() => {}} secondary />);
    expect(screen.getByText('X')).toHaveClass('secondary');
  });
});
