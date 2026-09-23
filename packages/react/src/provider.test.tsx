// @vitest-environment jsdom
import { StrictMode, type ReactElement } from 'react';
import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { clientMock } from './client-mock.js';
import { AppricotProvider, useAppricot } from './provider.js';

vi.mock('@appricot/client', async (importOriginal) => {
  const { mockClientModule } = await import('./client-mock.js');
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

function Probe() {
  const { status, registry, conn, send } = useAppricot();
  return (
    <>
      <output data-testid="status">{status}</output>
      <output data-testid="has-conn">{conn === null ? 'no' : 'yes'}</output>
      <output data-testid="has-registry">{registry === null ? 'no' : 'yes'}</output>
      <button
        data-testid="send"
        type="button"
        onClick={() => {
          send({ kind: 'closeRequest', closeRequest: { surfaceId: 7 } });
        }}
      />
    </>
  );
}

function renderInProvider(ui: ReactElement) {
  return render(
    <AppricotProvider url={TEST_URL} token="stream-token">
      {ui}
    </AppricotProvider>,
  );
}

describe('AppricotProvider', () => {
  it('connects on mount and shares connection, registry and send below itself', () => {
    const m = clientMock();
    renderInProvider(<Probe />);
    expect(m.connect).toHaveBeenCalledTimes(1);
    expect(m.connect).toHaveBeenCalledWith(TEST_URL, {
      token: new TextEncoder().encode('stream-token'),
    });
    expect(screen.getByTestId('has-conn').textContent).toBe('yes');
    expect(screen.getByTestId('has-registry').textContent).toBe('yes');
    expect(screen.getByTestId('status').textContent).toBe('connecting');
  });

  it('passes a Uint8Array token through unchanged', () => {
    const m = clientMock();
    const token = new TextEncoder().encode('raw-bytes');
    render(
      <AppricotProvider url={TEST_URL} token={token}>
        <Probe />
      </AppricotProvider>,
    );
    expect(m.connect).toHaveBeenCalledWith(TEST_URL, { token });
  });

  it('passes reconnect through when asked', () => {
    const m = clientMock();
    render(
      <AppricotProvider url={TEST_URL} token="stream-token" reconnect>
        <Probe />
      </AppricotProvider>,
    );
    expect(m.connect).toHaveBeenCalledWith(TEST_URL, {
      token: new TextEncoder().encode('stream-token'),
      reconnect: true,
    });
  });

  it('applies every inbound envelope to the registry', () => {
    const m = clientMock();
    renderInProvider(<Probe />);
    act(() => {
      m.deliverEnvelope({ kind: 'surfaceNew', surfaceNew: { surfaceId: 3, role: 0, title: '', appId: '', scale120ths: 120 } });
    });
    expect(m.apply).toHaveBeenCalledWith(
      expect.objectContaining({ kind: 'surfaceNew' }),
    );
  });

  it('tracks the connection status', () => {
    const m = clientMock();
    renderInProvider(<Probe />);
    act(() => {
      m.setStatus('open');
    });
    expect(screen.getByTestId('status').textContent).toBe('open');
  });

  it('forwards send to the connection', () => {
    const m = clientMock();
    renderInProvider(<Probe />);
    fireEvent.click(screen.getByTestId('send'));
    expect(m.connections()[0]?.send).toHaveBeenCalledWith({
      kind: 'closeRequest',
      closeRequest: { surfaceId: 7 },
    });
  });

  it('closes the connection on unmount', () => {
    const m = clientMock();
    const view = renderInProvider(<Probe />);
    view.unmount();
    expect(m.connections()[0]?.close).toHaveBeenCalledTimes(1);
  });

  it('closes and reconnects when the url changes', () => {
    const m = clientMock();
    const view = render(
      <AppricotProvider url="wss://one.test/session" token="stream-token">
        <Probe />
      </AppricotProvider>,
    );
    view.rerender(
      <AppricotProvider url="wss://two.test/session" token="stream-token">
        <Probe />
      </AppricotProvider>,
    );
    expect(m.connect).toHaveBeenCalledTimes(2);
    expect(m.connect).toHaveBeenLastCalledWith('wss://two.test/session', {
      token: new TextEncoder().encode('stream-token'),
    });
    const [first, second] = m.connections();
    expect(first).not.toBe(second);
    // The OLD connection is the one closed, and it keeps no listener of the provider's.
    expect(first?.close).toHaveBeenCalledTimes(1);
    expect(first !== undefined && m.connectionListenerCount(first)).toBe(0);
    // The new one is live and followed.
    expect(second?.close).not.toHaveBeenCalled();
    act(() => {
      m.setStatus('open');
    });
    expect(screen.getByTestId('status').textContent).toBe('open');
  });

  it('ignores status and messages from a connection it already replaced', () => {
    const m = clientMock();
    const view = render(
      <AppricotProvider url="wss://one.test/session" token="stream-token">
        <Probe />
      </AppricotProvider>,
    );
    const first = m.latest();
    view.rerender(
      <AppricotProvider url="wss://two.test/session" token="stream-token">
        <Probe />
      </AppricotProvider>,
    );
    act(() => {
      first?.events.emit('status', 'open');
      first?.events.emit('message', { kind: 'surfaceNew', surfaceNew: { surfaceId: 1 } });
    });
    expect(screen.getByTestId('status').textContent).toBe('connecting');
    expect(m.apply).not.toHaveBeenCalled();
  });

  it('reconnects when the token changes', () => {
    const m = clientMock();
    const view = render(
      <AppricotProvider url={TEST_URL} token="ticket-a">
        <Probe />
      </AppricotProvider>,
    );
    view.rerender(
      <AppricotProvider url={TEST_URL} token="ticket-b">
        <Probe />
      </AppricotProvider>,
    );
    expect(m.connect).toHaveBeenCalledTimes(2);
    expect(m.connect).toHaveBeenLastCalledWith(TEST_URL, {
      token: new TextEncoder().encode('ticket-b'),
    });
    expect(m.connections()[0]?.close).toHaveBeenCalledTimes(1);
  });

  it('compares a byte token by content: an equal new Uint8Array does not reconnect', () => {
    const m = clientMock();
    const view = render(
      <AppricotProvider url={TEST_URL} token={new TextEncoder().encode('ticket')}>
        <Probe />
      </AppricotProvider>,
    );
    // An inline encode on every render is the common host pattern.
    view.rerender(
      <AppricotProvider url={TEST_URL} token={new TextEncoder().encode('ticket')}>
        <Probe />
      </AppricotProvider>,
    );
    view.rerender(
      <AppricotProvider url={TEST_URL} token="ticket">
        <Probe />
      </AppricotProvider>,
    );
    expect(m.connect).toHaveBeenCalledTimes(1);
    expect(m.connections()[0]?.close).not.toHaveBeenCalled();

    // Different bytes do reconnect.
    view.rerender(
      <AppricotProvider url={TEST_URL} token={new TextEncoder().encode('other')}>
        <Probe />
      </AppricotProvider>,
    );
    expect(m.connect).toHaveBeenCalledTimes(2);
    expect(m.connect).toHaveBeenLastCalledWith(TEST_URL, {
      token: new TextEncoder().encode('other'),
    });
  });

  it('leaves exactly one live connection after a StrictMode mount', () => {
    const m = clientMock();
    render(
      <StrictMode>
        <AppricotProvider url={TEST_URL} token="stream-token">
          <Probe />
        </AppricotProvider>
      </StrictMode>,
    );
    // StrictMode runs the effect, its cleanup, and the effect again.
    expect(m.connect).toHaveBeenCalledTimes(2);
    const live = m.connections().filter((c) => c.close.mock.calls.length === 0);
    expect(live).toHaveLength(1);
    expect(live[0]).toBe(m.latest());
    // The closed one has no listener left, and the live one drives the status.
    const closed = m.connections()[0];
    expect(closed !== undefined && m.connectionListenerCount(closed)).toBe(0);
    act(() => {
      m.setStatus('open');
    });
    expect(screen.getByTestId('status').textContent).toBe('open');
  });

  it('gives every connection a fresh registry', () => {
    const m = clientMock();
    const view = render(
      <AppricotProvider url="wss://one.test/session" token="stream-token">
        <Probe />
      </AppricotProvider>,
    );
    const before = m.registries().length;
    view.rerender(
      <AppricotProvider url="wss://two.test/session" token="stream-token">
        <Probe />
      </AppricotProvider>,
    );
    expect(m.registries().length).toBe(before + 1);
  });

  it('throws below the provider', () => {
    const errors = vi.spyOn(console, 'error').mockImplementation(() => undefined);
    try {
      expect(() => render(<Probe />)).toThrow(
        'useAppricot() must be called inside <AppricotProvider>.',
      );
    } finally {
      errors.mockRestore();
      cleanup();
    }
  });
});
