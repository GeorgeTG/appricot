// @vitest-environment jsdom
/**
 * The surface and the hooks against the REAL registry and the REAL input mapping: only the
 * connection and the renderer are faked. What these tests watch is what the user meets: where
 * a click lands when the host draws the canvas at another CSS size, and what the hooks show
 * after a resume re-announced the windows (v0 §7).
 */
import { act, cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { clientMock } from './client-mock.js';
import { useSessionEvents, useWindows } from './hooks.js';
import type { SessionEvent } from './hooks.js';
import { AppricotProvider } from './provider.js';
import { AppricotSurface } from './surface.js';

vi.mock('@app-ricot/client', async (importOriginal) => {
  const { mockClientModule } = await import('./client-mock.js');
  const actual = await importOriginal<Record<string, unknown>>();
  // Everything fake except the registry and the input mapping, which stay the shipped code.
  return {
    ...actual,
    ...mockClientModule(),
    SurfaceRegistry: actual['SurfaceRegistry'],
    attachInput: actual['attachInput'],
  };
});

afterEach(() => {
  cleanup();
  clientMock().reset();
});

function surfaceNew(
  surfaceId: number,
  title: string,
  size: { width: number; height: number },
): unknown {
  return {
    kind: 'surfaceNew',
    surfaceNew: { surfaceId, role: 0, title, appId: `app.${surfaceId}`, scale120ths: 120, size },
  };
}

function helloReply(resumed: boolean): unknown {
  return {
    kind: 'helloReply',
    helloReply: {
      protocolVersion: 0,
      sessionId: 'session-1',
      maxFrameCredits: 4,
      codecs: [1],
      resumed,
      resumeSerial: 1,
      resumeGraceMs: 10_000,
    },
  };
}

/** Gives `canvas` a CSS box of `width` x `height` at the page's top-left. */
function cssBox(canvas: HTMLCanvasElement, width: number, height: number): void {
  const box = { x: 0, y: 0, left: 0, top: 0, width, height, right: width, bottom: height };
  vi.spyOn(canvas, 'getBoundingClientRect').mockReturnValue({
    ...box,
    toJSON: () => box,
  } as DOMRect);
}

describe('AppricotSurface input', () => {
  it('scales a pointer from the CSS box to the surface size the registry holds', () => {
    const m = clientMock();
    const { container } = render(
      <AppricotProvider url="wss://appricot.test/session" token="t">
        <AppricotSurface id={1} />
      </AppricotProvider>,
    );
    act(() => {
      m.deliverEnvelope(surfaceNew(1, 'Editor', { width: 800, height: 600 }));
    });
    const canvas = container.querySelector('canvas');
    if (canvas === null) throw new Error('the surface renders a canvas');
    // The host draws the 800x600 surface at half size.
    cssBox(canvas, 400, 300);
    const send = m.latest()?.send;

    canvas.dispatchEvent(new MouseEvent('pointermove', { clientX: 200, clientY: 150 }));
    expect(send).toHaveBeenLastCalledWith({
      kind: 'pointerMove',
      pointerMove: { surfaceId: 1, x: 400, y: 300 },
    });

    // The app took a new size: the next position is scaled to it, with no re-attach.
    act(() => {
      m.deliverEnvelope({
        kind: 'configureAck',
        configureAck: { surfaceId: 1, serial: 0, size: { width: 1600, height: 1200 } },
      });
    });
    canvas.dispatchEvent(new MouseEvent('pointermove', { clientX: 200, clientY: 150 }));
    expect(send).toHaveBeenLastCalledWith({
      kind: 'pointerMove',
      pointerMove: { surfaceId: 1, x: 800, y: 600 },
    });
  });
});

describe('the hooks across a resume', () => {
  function WindowsList() {
    const windows = useWindows();
    return (
      <output data-testid="windows">
        {windows.map((w) => `${w.id}:${w.title}:${w.size.width}x${w.size.height}`).join('|')}
      </output>
    );
  }

  it('follow the re-announcement in place, and drop only what did not come back', () => {
    const m = clientMock();
    const events: SessionEvent[] = [];
    function Recorder() {
      useSessionEvents((event) => events.push(event));
      return null;
    }
    render(
      <AppricotProvider url="wss://appricot.test/session" token="t">
        <WindowsList />
        <Recorder />
      </AppricotProvider>,
    );
    act(() => {
      m.deliverEnvelope(helloReply(false));
      m.deliverEnvelope(surfaceNew(1, 'Editor', { width: 320, height: 200 }));
      m.deliverEnvelope(surfaceNew(2, 'Dialog', { width: 200, height: 100 }));
    });
    expect(screen.getByTestId('windows').textContent).toBe('1:Editor:320x200|2:Dialog:200x100');
    events.length = 0;

    // The socket dropped and the session resumed: surface 1 comes back renamed and larger,
    // surface 2 does not come back, and the first message after the SurfaceNews ends it.
    act(() => {
      m.deliverEnvelope(helloReply(true));
      m.deliverEnvelope(surfaceNew(1, 'Editor - notes.txt', { width: 640, height: 400 }));
    });
    // The hooks follow the in-place update at once, before the re-announcement ends.
    expect(screen.getByTestId('windows').textContent).toBe(
      '1:Editor - notes.txt:640x400|2:Dialog:200x100',
    );
    act(() => {
      m.deliverEnvelope({ kind: 'cursorGone', cursorGone: {} });
    });
    expect(screen.getByTestId('windows').textContent).toBe('1:Editor - notes.txt:640x400');

    // Surface 1 was updated, never removed and added again.
    expect(events.map((e) => e.type)).toEqual([
      'metadata',
      'configure-acked',
      'window-removed',
      'cursor-changed',
    ]);
    expect(events[1]).toEqual({
      type: 'configure-acked',
      surfaceId: 1,
      serial: 0,
      size: { width: 640, height: 400 },
    });
    expect(events[2]).toMatchObject({ type: 'window-removed', surfaceId: 2 });
  });
});
