import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useState,
  type ReactNode,
} from 'react';

import {
  connectAppricot,
  SurfaceRegistry,
  type AppricotConnection,
  type ConnectionStatus,
  type Envelope,
} from './client';

/** What the provider shares with the components below it. */
export interface AppricotContextValue {
  /** The connection's current state: idle, connecting, open, closed. */
  readonly status: ConnectionStatus;
  /**
   * The window registry. One per provider, and it survives a reconnect: the protocol
   * re-sends the whole window set on resume (wire.proto, REATTACH).
   */
  readonly registry: SurfaceRegistry;
  /** The connection, or null before it exists and after the provider's cleanup ran. */
  readonly conn: AppricotConnection | null;
  /**
   * Sends one envelope. The SDK drops sends while the connection is not open, so a host
   * re-sends focus and the like after a resume rather than the connection queueing state
   * it cannot vouch for; a host that needs to know uses `conn` directly.
   */
  readonly send: (envelope: Envelope) => void;
}

const AppricotContext = createContext<AppricotContextValue | null>(null);

/** A string token becomes its UTF-8 bytes; the wire carries `bytes`, never a string. */
const textEncoder = new TextEncoder();

export interface AppricotProviderProps {
  /** The streamer's WebSocket URL. The token never travels in it (wire.proto, AUTHENTICATION). */
  readonly url: string;
  /** The per-session stream token, sent in the Hello envelope. */
  readonly token: string | Uint8Array;
  /** Whether the client should reattach after a dropped socket (wire.proto, REATTACH). */
  readonly reconnect?: boolean;
  readonly children?: ReactNode;
}

/**
 * Makes one APPricot session available to the components below it.
 *
 * It owns the connection (connectAppricot) and the window registry, feeds every decoded
 * inbound envelope to the registry, and closes the connection when it unmounts or when
 * `url`, `token` or `reconnect` change. It renders no chrome and no markup of its own: the
 * host draws every window frame (ADR-0003 §5).
 */
export function AppricotProvider({ url, token, reconnect, children }: AppricotProviderProps) {
  const registry = useMemo(() => new SurfaceRegistry(), []);
  const [conn, setConn] = useState<AppricotConnection | null>(null);
  // 'idle': the SDK's state before any connection exists (ConnectionStatus's own start).
  const [status, setStatus] = useState<ConnectionStatus>('idle');

  useEffect(() => {
    const connection = connectAppricot(url, {
      token: typeof token === 'string' ? textEncoder.encode(token) : token,
      ...(reconnect !== undefined ? { reconnect } : {}),
    });
    const stopStatus = connection.events.on('status', setStatus);
    const stopMessages = connection.events.on('message', (envelope: Envelope) => {
      registry.apply(envelope);
    });
    setStatus(connection.status);
    setConn(connection);
    return () => {
      stopStatus();
      stopMessages();
      connection.close();
      setConn(null);
    };
  }, [url, token, reconnect, registry]);

  const send = useCallback(
    (envelope: Envelope) => {
      conn?.send(envelope);
    },
    [conn],
  );

  const value = useMemo<AppricotContextValue>(
    () => ({ status, registry, conn, send }),
    [status, registry, conn, send],
  );

  return <AppricotContext value={value}>{children}</AppricotContext>;
}

/** Reads the nearest provider's value. Throws when there is no provider above. */
export function useAppricot(): AppricotContextValue {
  const value = useContext(AppricotContext);
  if (value === null) {
    throw new Error('useAppricot() must be called inside <AppricotProvider>.');
  }
  return value;
}
