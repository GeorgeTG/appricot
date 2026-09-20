// @vitest-environment jsdom
import type { ReactElement } from 'react';
import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { clientMock } from './client-mock';
import { AppricotProvider, useAppricot } from './provider';

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
    expect(m.connections()[0]?.close).toHaveBeenCalledTimes(1);
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
