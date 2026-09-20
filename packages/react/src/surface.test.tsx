// @vitest-environment jsdom
import type { ReactElement } from 'react';
import { act, cleanup, render } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { clientMock, fakeSurfaceRecord } from './client-mock';
import { AppricotProvider } from './provider';
import { AppricotSurface } from './surface';

vi.mock('@appricot/client', async (importOriginal) => {
  const { mockClientModule } = await import('./client-mock');
  return {
    ...(await importOriginal<Record<string, unknown>>()),
    ...mockClientModule(),
  };
});

const TEST_URL = 'wss://appricot.test/session';

function renderInProvider(ui: ReactElement) {
  return render(
    <AppricotProvider url={TEST_URL} token="stream-token">
      {ui}
    </AppricotProvider>,
  );
}

/** jsdom does no layout: clientWidth/clientHeight are 0 unless defined by hand. */
function sizeOf(canvas: Element, width: number, height: number): void {
  Object.defineProperty(canvas, 'clientWidth', { value: width, configurable: true });
  Object.defineProperty(canvas, 'clientHeight', { value: height, configurable: true });
}

afterEach(() => {
  vi.useRealTimers();
  cleanup();
  clientMock().reset();
});

describe('AppricotSurface', () => {
  it('renders one canvas and nothing else', () => {
    const { container } = renderInProvider(<AppricotSurface id={7} />);
    expect(container.childElementCount).toBe(1);
    expect(container.querySelector('canvas')).not.toBeNull();
  });

  it('does not set the canvas size itself: the renderer owns the backing store', () => {
    const m = clientMock();
    m.setMeta(fakeSurfaceRecord(7, { size: { width: 800, height: 600 } }));
    const { container } = renderInProvider(<AppricotSurface id={7} />);
    const canvas = container.querySelector('canvas');
    expect(canvas?.getAttribute('width')).toBeNull();
    expect(canvas?.getAttribute('height')).toBeNull();
  });

  it('attaches the renderer and input to its canvas once the surface is known', () => {
    const m = clientMock();
    m.setMeta(fakeSurfaceRecord(7));
    const { container } = renderInProvider(<AppricotSurface id={7} />);
    const canvas = container.querySelector('canvas');
    expect(canvas).not.toBeNull();

    expect(m.rendererCalls()).toHaveLength(1);
    const renderer = m.rendererCalls()[0];
    expect(renderer?.canvas).toBe(canvas);
    expect(renderer?.surfaceId).toBe(7);
    expect((renderer?.deps as { conn: unknown }).conn).toBe(m.connections()[0]);

    expect(m.inputCalls()).toHaveLength(1);
    const input = m.inputCalls()[0];
    expect(input?.element).toBe(canvas);
    expect(input?.surfaceId).toBe(7);
    expect((input?.deps as { conn: unknown }).conn).toBe(m.connections()[0]);
  });

  it('attaches nothing while the surface is unknown', () => {
    const m = clientMock();
    renderInProvider(<AppricotSurface id={7} />);
    expect(m.rendererCalls()).toHaveLength(0);
    expect(m.inputCalls()).toHaveLength(0);
  });

  it('detaches input and the renderer on unmount', () => {
    const m = clientMock();
    m.setMeta(fakeSurfaceRecord(7));
    const view = renderInProvider(<AppricotSurface id={7} />);
    view.unmount();
    expect(m.detachInput).toHaveBeenCalledTimes(1);
    expect(m.rendererDetach).toHaveBeenCalledTimes(1);
  });

  it('sends FocusNotify when it becomes focused and the connection is open', () => {
    const m = clientMock();
    m.setMeta(fakeSurfaceRecord(7));
    const view = renderInProvider(<AppricotSurface id={7} />);
    const send = m.connections()[0]?.send;

    // focused flips true while the connection is still connecting: nothing is sent yet,
    // the SDK would drop it.
    view.rerender(
      <AppricotProvider url={TEST_URL} token="stream-token">
        <AppricotSurface id={7} focused />
      </AppricotProvider>,
    );
    expect(send).not.toHaveBeenCalled();

    // The connection opens: the pending focus now goes out.
    act(() => {
      m.setStatus('open');
    });
    expect(send).toHaveBeenCalledTimes(1);
    expect(send).toHaveBeenCalledWith({ kind: 'focusNotify', focusNotify: { surfaceId: 7 } });

    // Losing focus sends nothing: blur is the host's call (ADR-0003 §5).
    view.rerender(
      <AppricotProvider url={TEST_URL} token="stream-token">
        <AppricotSurface id={7} />
      </AppricotProvider>,
    );
    expect(send).toHaveBeenCalledTimes(1);

    // Gaining focus again while open sends again.
    view.rerender(
      <AppricotProvider url={TEST_URL} token="stream-token">
        <AppricotSurface id={7} focused />
      </AppricotProvider>,
    );
    expect(send).toHaveBeenCalledTimes(2);
  });

  it('re-sends FocusNotify when a resume reopens the connection', () => {
    const m = clientMock();
    m.setMeta(fakeSurfaceRecord(7));
    renderInProvider(<AppricotSurface id={7} focused />);
    const send = m.connections()[0]?.send;
    act(() => {
      m.setStatus('open');
    });
    expect(send).toHaveBeenCalledTimes(1);
    // A dropped socket and a resume: closed -> connecting -> open sends focus again.
    act(() => {
      m.setStatus('closed');
      m.setStatus('connecting');
      m.setStatus('open');
    });
    expect(send).toHaveBeenCalledTimes(2);
  });

  it('feeds the focused prop to attachInput through isFocused, without re-attaching', () => {
    const m = clientMock();
    m.setMeta(fakeSurfaceRecord(7));
    const view = renderInProvider(<AppricotSurface id={7} />);
    const deps = m.inputCalls()[0]?.deps as { isFocused: () => boolean };
    expect(deps.isFocused()).toBe(false);

    view.rerender(
      <AppricotProvider url={TEST_URL} token="stream-token">
        <AppricotSurface id={7} focused />
      </AppricotProvider>,
    );
    expect(deps.isFocused()).toBe(true);
    expect(m.inputCalls()).toHaveLength(1);
  });

  describe('autoConfigure', () => {
    it('proposes the observed size with rising serials and drops repeats', () => {
      vi.useFakeTimers();
      const m = clientMock();
      m.setMeta(fakeSurfaceRecord(7));
      const { container } = renderInProvider(<AppricotSurface id={7} autoConfigure />);
      const canvas = container.querySelector('canvas');
      expect(canvas).not.toBeNull();
      const send = m.connections()[0]?.send;

      sizeOf(canvas as Element, 800, 600);
      act(() => {
        vi.advanceTimersByTime(250);
      });
      expect(send).toHaveBeenCalledTimes(1);
      expect(send).toHaveBeenCalledWith({
        kind: 'configure',
        configure: { surfaceId: 7, serial: 1, size: { width: 800, height: 600 } },
      });

      sizeOf(canvas as Element, 1024, 768);
      act(() => {
        vi.advanceTimersByTime(250);
      });
      expect(send).toHaveBeenCalledTimes(2);
      expect(send).toHaveBeenCalledWith({
        kind: 'configure',
        configure: { surfaceId: 7, serial: 2, size: { width: 1024, height: 768 } },
      });

      // Same size again: deduplicated.
      act(() => {
        vi.advanceTimersByTime(1000);
      });
      expect(send).toHaveBeenCalledTimes(2);
    });

    it('re-proposes the same size when the connection opens (sends before it were dropped)', () => {
      vi.useFakeTimers();
      const m = clientMock();
      m.setMeta(fakeSurfaceRecord(7));
      const { container } = renderInProvider(<AppricotSurface id={7} autoConfigure />);
      const canvas = container.querySelector('canvas');
      sizeOf(canvas as Element, 800, 600);
      act(() => {
        vi.advanceTimersByTime(250);
      });
      const send = m.connections()[0]?.send;
      expect(send).toHaveBeenCalledTimes(1);

      act(() => {
        m.setStatus('open');
      });
      expect(send).toHaveBeenCalledTimes(2);
      expect(send).toHaveBeenLastCalledWith({
        kind: 'configure',
        configure: { surfaceId: 7, serial: 2, size: { width: 800, height: 600 } },
      });

      // And it does not loop: the dedupe state was reset exactly once for the reopen.
      act(() => {
        vi.advanceTimersByTime(1000);
      });
      expect(send).toHaveBeenCalledTimes(2);
    });

    it('clamps proposals to the v0 surface limits', () => {
      vi.useFakeTimers();
      const m = clientMock();
      m.setMeta(fakeSurfaceRecord(7));
      const { container } = renderInProvider(<AppricotSurface id={7} autoConfigure />);
      const canvas = container.querySelector('canvas');
      sizeOf(canvas as Element, 5000, 3000);
      act(() => {
        vi.advanceTimersByTime(250);
      });
      expect(m.connections()[0]?.send).toHaveBeenCalledWith({
        kind: 'configure',
        configure: { surfaceId: 7, serial: 1, size: { width: 1920, height: 1200 } },
      });
    });

    it('proposes nothing while the canvas has no size', () => {
      vi.useFakeTimers();
      const m = clientMock();
      m.setMeta(fakeSurfaceRecord(7));
      renderInProvider(<AppricotSurface id={7} autoConfigure />);
      act(() => {
        vi.advanceTimersByTime(1000);
      });
      expect(m.connections()[0]?.send).not.toHaveBeenCalled();
    });

    it('proposes nothing when autoConfigure is off', () => {
      vi.useFakeTimers();
      const m = clientMock();
      m.setMeta(fakeSurfaceRecord(7));
      const { container } = renderInProvider(<AppricotSurface id={7} />);
      const canvas = container.querySelector('canvas');
      sizeOf(canvas as Element, 800, 600);
      act(() => {
        vi.advanceTimersByTime(1000);
      });
      expect(m.connections()[0]?.send).not.toHaveBeenCalled();
    });
  });
});
