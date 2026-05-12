import { describe, it, expect } from 'vitest';
import { render, screen } from '@testing-library/react';
import { WindowChrome } from '../../src/components.jsx';

describe('WindowChrome', () => {
  it('renders mac chrome with title', () => {
    render(<WindowChrome platform="mac" title="HELLO">body</WindowChrome>);
    expect(screen.getByText('HELLO')).toBeInTheDocument();
    expect(document.querySelector('.titlebar--mac')).toBeInTheDocument();
  });

  it('renders windows chrome', () => {
    render(<WindowChrome platform="win" title="X">body</WindowChrome>);
    expect(document.querySelector('.titlebar--win')).toBeInTheDocument();
  });

  it('renders linux chrome', () => {
    render(<WindowChrome platform="lin" title="X">body</WindowChrome>);
    expect(document.querySelector('.titlebar--lin')).toBeInTheDocument();
  });

  it('renders provided children inside the body', () => {
    render(
      <WindowChrome platform="mac" title="X">
        <div data-testid="kid" />
      </WindowChrome>,
    );
    expect(screen.getByTestId('kid')).toBeInTheDocument();
  });
});
