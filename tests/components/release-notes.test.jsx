import { describe, it, expect } from 'vitest';
import { render, screen } from '@testing-library/react';
import { ReleaseNotes, releaseNotesOf } from '../../src/components.jsx';

describe('releaseNotesOf', () => {
  // The two channels deliver the same CHANGELOG section under different
  // names: `body` from tauri's updater payload, `notes` from updates.json.
  it('reads body from a stable-channel update', () => {
    expect(releaseNotesOf({ version: '1', body: '### Fixed' })).toBe('### Fixed');
  });

  it('reads notes from a dev-channel entry', () => {
    expect(releaseNotesOf({ version: '1', notes: '### Fixed' })).toBe('### Fixed');
  });

  it('prefers body when a manifest somehow carries both', () => {
    expect(releaseNotesOf({ body: 'from-body', notes: 'from-notes' })).toBe('from-body');
  });

  it.each([
    ['missing', {}],
    ['null body', { body: null }],
    ['empty string', { body: '' }],
    ['whitespace only', { body: '   \n\t ' }],
    ['non-string', { body: 42 }],
    ['undefined update', undefined],
  ])('returns null for %s so no empty expander is offered', (_label, update) => {
    expect(releaseNotesOf(update)).toBeNull();
  });
});

describe('ReleaseNotes', () => {
  it('renders markdown structure rather than raw syntax', () => {
    render(<ReleaseNotes markdown={'### Fixed\n\n- A bug\n- Another bug'} />);
    expect(screen.getByRole('heading', { name: 'Fixed' })).toBeInTheDocument();
    expect(screen.getAllByRole('listitem')).toHaveLength(2);
    // The literal markers must not survive into the output.
    expect(document.querySelector('.update-notes').textContent).not.toContain('###');
  });

  it('renders emphasis and inline code as elements', () => {
    const { container } = render(<ReleaseNotes markdown={'**bold** and `code`'} />);
    expect(container.querySelector('strong')).toHaveTextContent('bold');
    expect(container.querySelector('code')).toHaveTextContent('code');
    expect(container.textContent).not.toContain('**');
  });

  // The notes come off the network and render inside the app's own webview.
  // These three are the security contract, not cosmetics.
  it('escapes embedded HTML instead of rendering it', () => {
    const { container } = render(
      <ReleaseNotes markdown={'<img src=x onerror="alert(1)"><script>alert(2)</script>'} />,
    );
    expect(container.querySelector('script')).toBeNull();
    expect(container.querySelector('img')).toBeNull();
  });

  it('renders links as inert text so a manifest cannot navigate the webview', () => {
    const { container } = render(
      <ReleaseNotes markdown={'see [the docs](https://evil.example/x) for more'} />,
    );
    expect(container.querySelector('a')).toBeNull();
    // The label survives, so the prose still reads correctly.
    expect(container.textContent).toContain('the docs');
    expect(container.querySelector('.update-notes-link')).toHaveTextContent('the docs');
  });

  it('drops images so notes cannot beacon out to a remote host', () => {
    const { container } = render(
      <ReleaseNotes markdown={'![tracker](https://evil.example/pixel.png)'} />,
    );
    expect(container.querySelector('img')).toBeNull();
  });

  it('renders the real 2026.7.29 notes without leaking markdown syntax', () => {
    const real = [
      '### Fixed',
      '',
      '- **The QEMU boot test works again when QEMU came from Homebrew.** A',
      "  Finder-launched app does not inherit your shell's `PATH`.",
      '- **Diagnostics no longer report `diskutil` as missing.**',
    ].join('\n');
    const { container } = render(<ReleaseNotes markdown={real} />);
    expect(screen.getByRole('heading', { name: 'Fixed' })).toBeInTheDocument();
    expect(screen.getAllByRole('listitem')).toHaveLength(2);
    const text = container.textContent;
    expect(text).not.toContain('**');
    expect(text).not.toContain('###');
    expect(text).toContain('QEMU boot test works again');
  });
});
