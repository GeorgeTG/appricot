// @vitest-environment jsdom
/**
 * The provider and the hooks against the REAL SurfaceRegistry: only the connection, the
 * renderer and input are faked. The other test files mock the registry, so they can prove
 * what the provider calls, but not what a host ends up seeing. These tests look at the
 * windows themselves: what a url or token switch leaves behind, and whether the hooks'
 * records are safe to key a memo on.
 */
import { memo, type ReactElement } from 'react';
import { act, cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { clientMock } from './client-mock';
import type { SurfaceRecord } from './client';
import { useSurfaceMeta, useWindows } from './hooks';
import { AppricotProvider } from './provider';
import { AppricotSurface } from './surface';

vi.mock('@appricot/client', async (importOriginal) => {
  const { mockClientModule } = await import('./client-mock');
  const actual = await importOriginal<Record<string, unknown>>();
  // Everything fake except the registry, which stays the shipped code.
  return { ...actual, ...mockClientModule(), SurfaceRegistry: actual['SurfaceRegistry'] };
});

afterEach(() => {
  cleanup();
  clientMock().reset();
});

function surfaceNew(surfaceId: number, title: string): unknown {
  return {
    kind: 'surfaceNew',
    surfaceNew: {
      surfaceId,
      role: 0,
      title,
      appId: `app.${surfaceId}`,
      scale120ths: 120,
      size: { width: 320, height: 200 },
    },
  };
}

function metadata(surfaceId: number, title: string): unknown {
  return { kind: 'surfaceMetadata', surfaceMetadata: { surfaceId, title } };
}

function WindowsList() {
  const windows = useWindows();
  return <output data-testid="windows">{windows.map((w) => `${w.id}:${w.title}`).join('|')}</output>;
}

function Session({ url, token, children }: { url: string; token: string; children: ReactElement }) {
  return (
    <AppricotProvider url={url} token={token}>
      {children}
    </AppricotProvider>
  );
}

describe('a url or token switch starts from an empty registry', () => {
  it('drops the old windows and shows the new session under the same ids', () => {
    const m = clientMock();
    const view = render(
      <Session url="wss://one.test/session" token="t">
        <WindowsList />
      </Session>,
    );
    act(() => {
      m.deliverEnvelope(surfaceNew(1, 'Old app'));
    });
    expect(screen.getByTestId('windows').textContent).toBe('1:Old app');

    view.rerender(
      <Session url="wss://two.test/session" token="t">
        <WindowsList />
      </Session>,
    );
    // Nothing of session one survives the switch.
    expect(screen.getByTestId('windows').textContent).toBe('');

    // The new streamer numbers its surfaces from 1 as well: id 1 is a NEW window here.
    act(() => {
      m.deliverEnvelope(surfaceNew(1, 'New app'));
    });
    expect(screen.getByTestId('windows').textContent).toBe('1:New app');

    // The replaced connection feeds nothing any more.
    act(() => {
      m.connections()[0]?.events.emit('message', surfaceNew(2, 'ghost'));
    });
    expect(screen.getByTestId('windows').textContent).toBe('1:New app');
  });

  it('does the same on a token switch, and the surface re-attaches to the new registry', () => {
    const m = clientMock();
    const ui = (token: string): ReactElement => (
      <Session url="wss://one.test/session" token={token}>
        <>
          <WindowsList />
          <AppricotSurface id={1} />
        </>
      </Session>
    );
    const view = render(ui('ticket-a'));
    act(() => {
      m.deliverEnvelope(surfaceNew(1, 'Session A'));
    });
    expect(m.rendererCalls()).toHaveLength(1);
    const firstRegistry = (m.rendererCalls()[0]?.deps as { registry: unknown }).registry;

    view.rerender(ui('ticket-b'));
    expect(screen.getByTestId('windows').textContent).toBe('');
    // The old renderer went with the old session; nothing attaches before B announces.
    expect(m.rendererDetach).toHaveBeenCalledTimes(1);
    expect(m.rendererCalls()).toHaveLength(1);

    act(() => {
      m.deliverEnvelope(surfaceNew(1, 'Session B'));
    });
    expect(screen.getByTestId('windows').textContent).toBe('1:Session B');
    expect(m.rendererCalls()).toHaveLength(2);
    const second = m.rendererCalls()[1]?.deps as { registry: unknown; conn: unknown };
    expect(second.registry).not.toBe(firstRegistry);
    expect(second.conn).toBe(m.latest());
  });
});

describe('the hooks hand out snapshots, not the registry records', () => {
  it('changes a record snapshot exactly when the record changes, so memo sees titles', () => {
    const m = clientMock();
    const renders: string[] = [];
    const Title = memo(function Title({ meta }: { meta: SurfaceRecord }) {
      renders.push(meta.title);
      return <span data-testid={`title-${meta.id}`}>{meta.title}</span>;
    });
    function Frame({ id }: { id: number }) {
      const meta = useSurfaceMeta(id);
      return meta === undefined ? null : <Title meta={meta} />;
    }
    render(
      <Session url="wss://one.test/session" token="t">
        <>
          <Frame id={1} />
          <Frame id={2} />
        </>
      </Session>,
    );
    act(() => {
      m.deliverEnvelope(surfaceNew(1, 'Editor'));
      m.deliverEnvelope(surfaceNew(2, 'Terminal'));
    });
    expect(screen.getByTestId('title-1').textContent).toBe('Editor');
    renders.length = 0;

    act(() => {
      m.deliverEnvelope(metadata(1, 'notes.txt - Editor'));
    });
    // The memo'd title of window 1 saw the change; window 2's did not re-render at all.
    expect(screen.getByTestId('title-1').textContent).toBe('notes.txt - Editor');
    expect(renders).toEqual(['notes.txt - Editor']);
  });

  it('keeps the useWindows list identity until a window changes, and freezes it', () => {
    const m = clientMock();
    const lists: Array<readonly SurfaceRecord[]> = [];
    function Collect() {
      lists.push(useWindows());
      return null;
    }
    render(
      <Session url="wss://one.test/session" token="t">
        <Collect />
      </Session>,
    );
    act(() => {
      m.deliverEnvelope(surfaceNew(1, 'Editor'));
    });
    const withOne = lists.at(-1);
    expect(withOne?.map((w) => w.title)).toEqual(['Editor']);
    expect(Object.isFrozen(withOne)).toBe(true);
    expect(Object.isFrozen(withOne?.[0])).toBe(true);

    // A cursor event is not a record change: no new list.
    const count = lists.length;
    act(() => {
      m.deliverEnvelope({ kind: 'cursorGone', cursorGone: {} });
    });
    expect(lists.length).toBe(count);

    act(() => {
      m.deliverEnvelope(metadata(1, 'Renamed'));
    });
    expect(lists.at(-1)).not.toBe(withOne);
    expect(lists.at(-1)?.[0]?.title).toBe('Renamed');
    // The old snapshot is untouched: it is a copy, not the live record.
    expect(withOne?.[0]?.title).toBe('Editor');
  });
});
