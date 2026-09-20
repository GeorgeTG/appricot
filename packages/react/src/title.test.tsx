// @vitest-environment jsdom
import type { ReactElement } from 'react';
import { act, cleanup, render } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { clientMock, fakeSurfaceRecord } from './client-mock';
import { AppricotProvider } from './provider';
import { AppricotTitle } from './title';

vi.mock('@appricot/client', async (importOriginal) => {
  const { mockClientModule } = await import('./client-mock');
  return {
    ...(await importOriginal<Record<string, unknown>>()),
    ...mockClientModule(),
  };
});

// The ADR-0003 fixture: markup that would run script if anything ever parsed it as HTML,
// next to ordinary Greek text.
const HOSTILE_TITLE = '<img src=x onerror="alert(1)"> Τίτλος Παραθύρου';

function renderInProvider(ui: ReactElement) {
  return render(
    <AppricotProvider url="wss://appricot.test/session" token="stream-token">
      {ui}
    </AppricotProvider>,
  );
}

afterEach(() => {
  cleanup();
  clientMock().reset();
});

describe('AppricotTitle', () => {
  it('shows a hostile title as text and creates no element (ADR-0003 §1)', () => {
    const m = clientMock();
    m.setMeta(fakeSurfaceRecord(7, { title: HOSTILE_TITLE }));
    const { container } = renderInProvider(<AppricotTitle id={7} />);
    const title = container.querySelector('span');
    expect(title).not.toBeNull();
    expect(title?.textContent).toBe(HOSTILE_TITLE);
    expect(title?.querySelector('img')).toBeNull();
    expect(title?.childElementCount).toBe(0);
    expect(title?.firstChild?.nodeType).toBe(Node.TEXT_NODE);
  });

  it('renders the element the host chose', () => {
    const m = clientMock();
    m.setMeta(fakeSurfaceRecord(7, { title: HOSTILE_TITLE }));
    const { container } = renderInProvider(<AppricotTitle id={7} as="h2" />);
    expect(container.querySelector('span')).toBeNull();
    const heading = container.querySelector('h2');
    expect(heading?.textContent).toBe(HOSTILE_TITLE);
    expect(heading?.querySelector('img')).toBeNull();
  });

  it('follows title changes and stays text-only', () => {
    const m = clientMock();
    m.setMeta(fakeSurfaceRecord(7, { title: 'Before' }));
    const { container } = renderInProvider(<AppricotTitle id={7} />);
    expect(container.querySelector('span')?.textContent).toBe('Before');

    m.setMeta(fakeSurfaceRecord(7, { title: HOSTILE_TITLE }));
    act(() => {
      m.emitRegistry('metadata', { surface: { id: 7 } });
    });
    const title = container.querySelector('span');
    expect(title?.textContent).toBe(HOSTILE_TITLE);
    expect(title?.querySelector('img')).toBeNull();
  });

  it('shows nothing while the surface is unknown', () => {
    const { container } = renderInProvider(<AppricotTitle id={9} />);
    expect(container.querySelector('span')?.textContent).toBe('');
  });
});
