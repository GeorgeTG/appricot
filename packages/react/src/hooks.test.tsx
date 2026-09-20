// @vitest-environment jsdom
import type { ReactElement } from 'react';
import { act, cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { clientMock, fakeSurfaceRecord } from './client-mock';
import type { SessionEvent } from './hooks';
import { useConnectionStatus, useSessionEvents, useSurfaceMeta, useWindows } from './hooks';
import { AppricotProvider } from './provider';

vi.mock('@appricot/client', async (importOriginal) => {
  const { mockClientModule } = await import('./client-mock');
  return {
    ...(await importOriginal<Record<string, unknown>>()),
    ...mockClientModule(),
  };
});

afterEach(() => {
  cleanup();
  clientMock().reset();
});

const TEST_URL = 'wss://appricot.test/session';

function renderInProvider(ui: ReactElement) {
  return render(
    <AppricotProvider url={TEST_URL} token="stream-token">
      {ui}
    </AppricotProvider>,
  );
}

function WindowsList() {
  const windows = useWindows();
  return <output data-testid="windows">{windows.map((w) => `${w.id}:${w.title}`).join('|')}</output>;
}

function MetaProbe({ id }: { readonly id: number }) {
  const meta = useSurfaceMeta(id);
  return (
    <output data-testid="meta">
      {meta === undefined ? 'none' : `${meta.title}@${meta.size.width}x${meta.size.height}`}
    </output>
  );
}

function StatusProbe() {
  const status = useConnectionStatus();
  return <output data-testid="status">{status}</output>;
}

function EventsProbe({ handler }: { readonly handler: (event: SessionEvent) => void }) {
  useSessionEvents(handler);
  return null;
}

describe('useWindows', () => {
  it('lists the toplevel windows and follows adds, changes and removals', () => {
    const m = clientMock();
    m.setSurfaces([fakeSurfaceRecord(1, { title: 'Editor' })]);
    renderInProvider(<WindowsList />);
    expect(screen.getByTestId('windows').textContent).toBe('1:Editor');

    m.setSurfaces([
      fakeSurfaceRecord(1, { title: 'Editor' }),
      fakeSurfaceRecord(2, { title: 'Terminal' }),
      // A popup is not a toplevel window: it never appears in the list.
      fakeSurfaceRecord(3, { title: 'Menu', role: 1, parent: 1 }),
    ]);
    act(() => {
      m.emitRegistry('window-added', { surface: { id: 2 } });
    });
    expect(screen.getByTestId('windows').textContent).toBe('1:Editor|2:Terminal');

    m.setSurfaces([fakeSurfaceRecord(1, { title: 'Renamed' })]);
    act(() => {
      m.emitRegistry('metadata', { surface: { id: 1 } });
      m.emitRegistry('window-removed', { surfaceId: 2, reason: 0 });
    });
    expect(screen.getByTestId('windows').textContent).toBe('1:Renamed');
  });
});

describe('useSurfaceMeta', () => {
  it('reads and follows one surface record', () => {
    const m = clientMock();
    m.setMeta(fakeSurfaceRecord(7, { title: 'Hostile', size: { width: 800, height: 600 } }));
    renderInProvider(<MetaProbe id={7} />);
    expect(screen.getByTestId('meta').textContent).toBe('Hostile@800x600');

    m.setMeta(fakeSurfaceRecord(7, { title: 'Renamed', size: { width: 1024, height: 768 } }));
    act(() => {
      m.emitRegistry('configure-acked', { surfaceId: 7, serial: 1, size: { width: 1024, height: 768 } });
    });
    expect(screen.getByTestId('meta').textContent).toBe('Renamed@1024x768');
  });

  it('reports undefined while the surface is unknown', () => {
    renderInProvider(<MetaProbe id={9} />);
    expect(screen.getByTestId('meta').textContent).toBe('none');
  });
});

describe('useConnectionStatus', () => {
  it('tracks the connection status', () => {
    const m = clientMock();
    renderInProvider(<StatusProbe />);
    expect(screen.getByTestId('status').textContent).toBe('connecting');
    act(() => {
      m.setStatus('open');
    });
    expect(screen.getByTestId('status').textContent).toBe('open');
  });
});

describe('useSessionEvents', () => {
  it('reports every registry event with its name, to the latest handler', () => {
    const m = clientMock();
    const first = vi.fn();
    const second = vi.fn();
    const view = renderInProvider(<EventsProbe handler={first} />);

    act(() => {
      m.emitRegistry('focus-ask', { surfaceId: 7 });
      m.emitRegistry('clipboard-ask');
    });
    expect(first).toHaveBeenCalledTimes(2);
    expect(first).toHaveBeenCalledWith({ type: 'focus-ask', surfaceId: 7 });
    expect(first).toHaveBeenCalledWith({ type: 'clipboard-ask' });

    view.rerender(
      <AppricotProvider url={TEST_URL} token="stream-token">
        <EventsProbe handler={second} />
      </AppricotProvider>,
    );
    act(() => {
      m.emitRegistry('resize-ask', { surfaceId: 7, size: { width: 640, height: 480 } });
    });
    expect(second).toHaveBeenCalledWith({
      type: 'resize-ask',
      surfaceId: 7,
      size: { width: 640, height: 480 },
    });
    expect(first).toHaveBeenCalledTimes(2);
    // A new handler never resubscribes: one listener per event name for the whole lifetime.
    expect(m.registryListenerCount('focus-ask')).toBe(1);
    expect(m.registryListenerCount('resize-ask')).toBe(1);
  });
});
