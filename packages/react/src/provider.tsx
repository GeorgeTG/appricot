import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
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
   * The window registry. One per connection: it survives the SDK's own reconnect, since the
   * protocol re-sends the whole window set on resume (wire.proto, REATTACH), but a new `url`,
   * `token` or `reconnect` opens a new session and gets a new, empty registry. Surface ids
   * are per session, so the old session's windows must not linger or swallow the new ones.
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

/**
 * A key that is equal exactly when two tokens carry the same bytes, so the connection effect
 * compares tokens by content. A Uint8Array compares by identity in a dependency list, and an
 * inline `new TextEncoder().encode(ticket)` would otherwise reconnect on every render. A
 * string and a byte array with the same UTF-8 bytes send the same Hello, so they share a key.
 */
function tokenContentKey(token: string | Uint8Array): string {
  const bytes = typeof token === 'string' ? textEncoder.encode(token) : token;
  let key = '';
  for (const byte of bytes) {
    key += byte.toString(16).padStart(2, '0');
  }
  return key;
}

export interface AppricotProviderProps {
  /** The streamer's WebSocket URL. The token never travels in it (wire.proto, AUTHENTICATION). */
  readonly url: string;
  /**
   * The per-session stream token, sent in the Hello envelope. Compared by content, not by
   * identity: an equal token in a new Uint8Array (an inline `encode(...)`, say) does not
   * reconnect.
   */
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
 * `url`, `token` (by content) or `reconnect` change. Each connection gets a fresh registry.
 * It renders no chrome and no markup of its own: the host draws every window frame
 * (ADR-0003 §5).
 */
export function AppricotProvider({ url, token, reconnect, children }: AppricotProviderProps) {
  // useState, not useMemo: the registry must persist, and useMemo is only a cache. The
  // effect below replaces it with a fresh one for every connection it opens.
  const [registry, setRegistry] = useState(() => new SurfaceRegistry());
  const [conn, setConn] = useState<AppricotConnection | null>(null);
  // 'idle': the SDK's state before any connection exists (ConnectionStatus's own start).
  const [status, setStatus] = useState<ConnectionStatus>('idle');

  // The effect keys on the token's content; the bytes themselves travel through a ref that
  // is refreshed before the connection effect runs (effects run in declaration order).
  const tokenKey = tokenContentKey(token);
  const tokenRef = useRef(token);
  useEffect(() => {
    tokenRef.current = token;
  });

  useEffect(() => {
    const current = tokenRef.current;
    const sessionRegistry = new SurfaceRegistry();
    const connection = connectAppricot(url, {
      token: typeof current === 'string' ? textEncoder.encode(current) : current,
      ...(reconnect !== undefined ? { reconnect } : {}),
    });
    const stopStatus = connection.events.on('status', setStatus);
    const stopMessages = connection.events.on('message', (envelope: Envelope) => {
      sessionRegistry.apply(envelope);
    });
    setRegistry(sessionRegistry);
    setStatus(connection.status);
    setConn(connection);
    return () => {
      stopStatus();
      stopMessages();
      connection.close();
      setConn(null);
    };
  }, [url, tokenKey, reconnect]);

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
