/**
 * The connection: a transport, the Hello handshake, and opt-in auto-reconnect with resume.
 *
 * Nothing here paces frames (rule 5, docs/protocol/README.md): `setTimeout` only spaces
 * reconnect attempts. Every server-to-client envelope that decodes cleanly and arrives in an
 * order v0 allows reaches the host through `events`; nothing the server sends ever becomes
 * markup or navigation (ADR-0003).
 */
import { Emitter } from './events.js';
import {
  CODEC,
  MAX_CLIPBOARD_BYTES,
  PROTOCOL_VERSION,
  ProtocolError,
  RESUME_GRACE_MS,
  decodeEnvelope,
  encodeEnvelope,
} from './protocol.js';
import type { Bye, ByeReason, Envelope, Hello, HelloReply } from './protocol.js';

const utf8Encoder = new TextEncoder();

/** The UTF-8 byte length of `text` — the unit every wire cap counts. */
function utf8ByteLength(text: string): number {
  // Encoding (rather than arithmetic on .length) is exact for every script and needs no
  // surrogate reasoning; the clipboard cap is 64 KiB, so the allocation is trivial next to
  // the message it guards.
  return utf8Encoder.encode(text).length;
}

/**
 * One byte pipe carrying encoded envelopes. `close()` guarantees the `onClose` callback
 * fires exactly once per transport, after which the transport is dead.
 */
export interface Transport {
  send(data: Uint8Array): void;
  close(): void;
  onMessage(cb: (data: Uint8Array) => void): void;
  onClose(cb: (code: number) => void): void;
  onOpen(cb: () => void): void;
}

/** A `Transport` over the browser's WebSocket. Binary frames only; text never arrives. */
export class WebSocketTransport implements Transport {
  readonly #socket: WebSocket;

  constructor(url: string) {
    this.#socket = new WebSocket(url);
    this.#socket.binaryType = 'arraybuffer';
  }

  send(data: Uint8Array): void {
    this.#socket.send(data);
  }

  close(): void {
    this.#socket.close();
  }

  onMessage(cb: (data: Uint8Array) => void): void {
    this.#socket.addEventListener('message', (event) => {
      if (event.data instanceof ArrayBuffer) {
        cb(new Uint8Array(event.data));
      }
    });
  }

  onClose(cb: (code: number) => void): void {
    this.#socket.addEventListener('close', (event) => cb(event.code));
  }

  onOpen(cb: () => void): void {
    this.#socket.addEventListener('open', () => cb());
  }
}

export type ConnectionStatus =
  | 'idle' // constructed, `connect()` not called yet
  | 'connecting' // opening the transport or waiting for the HelloReply
  | 'open' // the session is live
  | 'reconnecting' // the socket dropped and a retry is scheduled; the session may resume
  | 'closed'; // terminal: nothing more happens until the host calls `connect()` again

/** Why the connection reached 'closed'. Every string inside is untrusted server text. */
export interface CloseReason {
  /**
   * - `user`: the host called `close()`.
   * - `bye`: the server said Bye (see `bye`); no Bye is retried, whatever the close code.
   * - `refused`: this client refused what the server sent (see `error`): bytes that do not
   *   decode, a message in an order v0 forbids, or a version it did not ask for.
   * - `dropped`: the socket closed and nothing retries it (reconnect is off, or the close
   *   was normal).
   * - `grace-expired`: reconnecting gave up; the resume grace (`resume_grace_ms`) since the
   *   socket died has run out, so the session is gone.
   * - `transport-error`: the transport could not even be created (see `error`).
   */
  cause: 'user' | 'bye' | 'refused' | 'dropped' | 'grace-expired' | 'transport-error';
  /** The last WebSocket close code, when a socket closed. */
  code?: number | undefined;
  /** The server's goodbye, when one arrived. Its text is untrusted. */
  bye?: Bye | undefined;
  /** The local error behind `refused` or `transport-error`. */
  error?: unknown;
}

export interface ConnectionEvents {
  /**
   * Every server-to-client envelope that decoded cleanly and arrived in an order v0 allows,
   * in arrival order — including the hostile ones. The connection has already acted on it
   * (the HelloReply opened the session) when listeners hear it.
   */
  message: Envelope;
  /** A transport closed; the value is the WebSocket close code (1006 when abnormal). */
  close: number;
  status: ConnectionStatus;
  /** A reconnect's resume was honoured; the window set and one full frame follow. */
  resumed: void;
  /** The connection reached 'closed', and why. It follows the 'closed' status. */
  ended: CloseReason;
}

export interface ConnectionOptions {
  /** The per-session stream token from Hello; opaque bytes that must match or the server
   * closes with BYE_AUTH_FAILED. Never travels in a URL (rule 8). */
  token: Uint8Array;
  /**
   * Reconnect with resume after an abnormal close. Default false. Retries back off from
   * 500 ms to 8 s and stop once the session's resume grace, counted from the drop, is over.
   */
  reconnect?: boolean;
}

/** Backoff for reconnect attempts: 500 ms doubling to at most 8 s, reset by a HelloReply. */
const RECONNECT_BASE_MS = 500;
const RECONNECT_MAX_MS = 8000;

/** The normal WebSocket close code; anything else counts as abnormal and may be retried. */
const NORMAL_CLOSE = 1000;

/** ByeReason values this client sends when it refuses the server (wire.proto ByeReason). */
const BYE_PROTOCOL_VERSION: ByeReason = 0x100;
const BYE_PROTOCOL_VIOLATION: ByeReason = 0x103;

/** Envelope kinds only a client sends (v0 §4); one arriving here is a wrong-direction message. */
const CLIENT_TO_SERVER: ReadonlySet<Envelope['kind']> = new Set<Envelope['kind']>([
  'hello',
  'configure',
  'frameAck',
  'pointerMove',
  'pointerButton',
  'pointerAxis',
  'key',
  'focusNotify',
  'blurRelease',
  'clipboardSet',
  'closeRequest',
]);

/** What may arrive before the HelloReply (v0 §5): the answer itself, or a goodbye. */
const BEFORE_REPLY: ReadonlySet<Envelope['kind']> = new Set<Envelope['kind']>([
  'helloReply',
  'bye',
  'serverError',
]);

export class AppricotConnection {
  readonly events = new Emitter<ConnectionEvents>();
  readonly #makeTransport: () => Transport;
  readonly #token: Uint8Array;
  readonly #reconnectEnabled: boolean;

  #status: ConnectionStatus = 'idle';
  /** The live transport. Callbacks from any other transport are stale and ignored. */
  #transport: Transport | undefined;
  /** True once the live transport has carried an accepted HelloReply. */
  #handshaken = false;
  /** The Bye the live transport carried, if any. */
  #bye: Bye | undefined;
  #resumeSerial: number | undefined;
  #graceMs = RESUME_GRACE_MS;
  /** When reconnecting stops: the drop plus the resume grace. Unset while no retry runs. */
  #retryDeadline: number | undefined;
  #backoffMs = 0;
  #reconnectTimer: ReturnType<typeof setTimeout> | undefined;
  #expectingResume = false;
  #closeReason: CloseReason | undefined;

  /** Untrusted text from HelloReply, for display only. */
  sessionId: string | undefined;
  /** The frame-credit limit the server promised (MAX_FRAME_CREDITS). */
  maxFrameCredits: number | undefined;

  constructor(makeTransport: () => Transport, options: ConnectionOptions) {
    this.#makeTransport = makeTransport;
    this.#token = options.token;
    this.#reconnectEnabled = options.reconnect ?? false;
  }

  get status(): ConnectionStatus {
    return this.#status;
  }

  /** Why the connection last reached 'closed'; undefined until it has. */
  get closeReason(): CloseReason | undefined {
    return this.#closeReason;
  }

  /**
   * Opens the transport and sends Hello when it is up. Calling while connecting or open is a
   * no-op. Calling while a reconnect is scheduled cancels the wait and tries now; calling after
   * 'closed' starts over, offering the stored resume serial if there is one.
   */
  connect(): void {
    if (this.#status === 'connecting' || this.#status === 'open') {
      return;
    }
    this.#clearReconnectTimer();
    if (this.#status !== 'reconnecting') {
      this.#retryDeadline = undefined;
      this.#backoffMs = 0;
    }
    this.#closeReason = undefined;
    this.#setStatus('connecting');
    this.#openTransport();
  }

  /**
   * Sends one envelope, encoded. Sends while not open are dropped: the host re-sends focus
   * and the like after a resume rather than the connection queueing state it cannot vouch for.
   */
  send(e: Envelope): void {
    if (this.#status !== 'open') {
      return;
    }
    this.#transport?.send(encodeEnvelope(e));
  }

  /**
   * Pastes `text` into the session: the quiet, capped path a host calls from a user gesture.
   *
   * Returns true when the paste was sent. A paste whose UTF-8 byte length is over
   * `MAX_CLIPBOARD_BYTES` (or one made while not open) is silently refused — nothing reaches
   * the wire, nothing throws, the session lives on. A host that wants to tell the user their
   * paste was too large reads the false return; one that wants to see every failure as an
   * exception can use {@link send} instead, which throws the codec's `ProtocolError` at the
   * same cap.
   */
  sendClipboardText(text: string): boolean {
    // The cap counts bytes (the wire's unit), not UTF-16 code units: Greek text is two bytes
    // a character and must be refused by its byte length, not slip through a .length check.
    const byteLength = utf8ByteLength(text);
    if (this.#status !== 'open' || byteLength > MAX_CLIPBOARD_BYTES) {
      return false;
    }
    this.#transport?.send(
      encodeEnvelope({ kind: 'clipboardSet', clipboardSet: { text } }),
    );
    return true;
  }

  /**
   * Closes, at once: the status is 'closed' when this returns, no retry follows, and the
   * transport's own close event, whenever it comes, changes nothing. Sends nothing extra —
   * the transport close is the whole goodbye.
   */
  close(): void {
    if (this.#status === 'closed') {
      return;
    }
    this.#end({ cause: 'user' });
  }

  #setStatus(status: ConnectionStatus): void {
    if (this.#status === status) {
      return;
    }
    this.#status = status;
    this.events.emit('status', status);
  }

  #openTransport(): void {
    let transport: Transport;
    try {
      transport = this.#makeTransport();
    } catch (error) {
      // A factory that throws (a malformed URL, say) will throw again: no retry.
      this.#end({ cause: 'transport-error', error });
      return;
    }
    this.#transport = transport;
    this.#handshaken = false;
    this.#bye = undefined;
    // Each callback carries its transport: one from a transport that is no longer the live
    // one (closed by the host, or replaced by a newer attempt) changes nothing.
    transport.onOpen(() => {
      if (this.#transport === transport) {
        this.#handleOpen(transport);
      }
    });
    transport.onMessage((data) => {
      if (this.#transport === transport) {
        this.#handleMessage(data);
      }
    });
    transport.onClose((code) => {
      if (this.#transport === transport) {
        this.#handleClose(code);
      }
    });
  }

  #handleOpen(transport: Transport): void {
    const hello: Hello = {
      protocolVersion: PROTOCOL_VERSION,
      clientName: '@appricot/client',
      streamToken: this.#token,
      codecs: [CODEC.QOI, CODEC.RAW], // best first; RAW is the must-decode fallback
    };
    if (this.#resumeSerial !== undefined) {
      hello.resumeSerial = this.#resumeSerial;
      this.#expectingResume = true;
    }
    transport.send(encodeEnvelope({ kind: 'hello', hello }));
  }

  #handleMessage(data: Uint8Array): void {
    let envelope: Envelope;
    try {
      envelope = decodeEnvelope(data);
    } catch (error) {
      // Bytes that do not decode close the connection locally and never retry (ADR-0003 §4:
      // a violation closes; it also never throws into the host).
      this.#refuse(error, BYE_PROTOCOL_VIOLATION);
      return;
    }
    // v0 §5 order and direction: before the HelloReply only the reply or a goodbye; after it
    // no second reply; and never a message only a client sends.
    if (CLIENT_TO_SERVER.has(envelope.kind)) {
      this.#refuse(
        new ProtocolError(`${envelope.kind} is a client-to-server message`),
        BYE_PROTOCOL_VIOLATION,
      );
      return;
    }
    if (this.#handshaken ? envelope.kind === 'helloReply' : !BEFORE_REPLY.has(envelope.kind)) {
      this.#refuse(
        new ProtocolError(`${envelope.kind} is out of order`),
        BYE_PROTOCOL_VIOLATION,
      );
      return;
    }
    // The connection's own state moves first; the host hears the envelope after (events-1).
    if (envelope.kind === 'helloReply' && !this.#acceptReply(envelope.helloReply)) {
      return;
    }
    if (envelope.kind === 'bye') {
      this.#bye = envelope.bye; // the socket close that follows ends the connection
    }
    this.events.emit('message', envelope);
  }

  /** Takes a HelloReply; false when it was refused and the connection is closed. */
  #acceptReply(reply: HelloReply): boolean {
    if (reply.protocolVersion !== PROTOCOL_VERSION) {
      // The server may not guess a version and neither do we (rule 1); fail without retry.
      this.#refuse(
        new ProtocolError(`HelloReply names version ${String(reply.protocolVersion)}`),
        BYE_PROTOCOL_VERSION,
      );
      return false;
    }
    this.#handshaken = true;
    this.sessionId = reply.sessionId;
    this.maxFrameCredits = reply.maxFrameCredits;
    if (reply.resumeSerial !== undefined) {
      this.#resumeSerial = reply.resumeSerial;
    }
    this.#graceMs = reply.resumeGraceMs ?? RESUME_GRACE_MS;
    this.#backoffMs = 0; // a live session resets the backoff
    this.#retryDeadline = undefined; // and the retry window
    this.#setStatus('open');
    const resumed = this.#expectingResume && reply.resumed;
    this.#expectingResume = false;
    if (resumed) {
      this.events.emit('resumed', undefined);
    }
    return true;
  }

  /**
   * Refuses the server: a Bye naming the violation (v0 §3, §5), then a local close without
   * retry. The Bye is best effort; the close is not.
   */
  #refuse(error: unknown, reason: ByeReason): void {
    try {
      this.#transport?.send(encodeEnvelope({ kind: 'bye', bye: { reason, text: '' } }));
    } catch {
      // a transport that cannot send any more still gets closed below
    }
    this.#end({ cause: 'refused', error });
  }

  #handleClose(code: number): void {
    this.#transport = undefined;
    this.events.emit('close', code);
    // Any Bye settles it, whatever the close code: the server said why, and a retry would
    // meet the same answer (or a violation loop).
    const bye = this.#bye;
    if (bye !== undefined) {
      this.#end({ cause: 'bye', code, bye });
      return;
    }
    if (!this.#reconnectEnabled || code === NORMAL_CLOSE) {
      this.#end({ cause: 'dropped', code });
      return;
    }
    this.#scheduleReconnect(code);
  }

  /**
   * Schedules the next attempt, backing off, but never past the resume grace counted from the
   * drop (v0 §7): after it the session is gone and the streamer refuses the upgrade, so
   * retrying longer would only knock on a closed door.
   */
  #scheduleReconnect(code: number): void {
    const now = Date.now();
    this.#retryDeadline ??= now + this.#graceMs;
    const remaining = this.#retryDeadline - now;
    if (remaining <= 0) {
      this.#end({ cause: 'grace-expired', code });
      return;
    }
    const backoff = this.#backoffMs === 0 ? RECONNECT_BASE_MS : this.#backoffMs;
    this.#backoffMs = Math.min(backoff * 2, RECONNECT_MAX_MS);
    this.#setStatus('reconnecting');
    this.#reconnectTimer = setTimeout(() => {
      this.#reconnectTimer = undefined;
      this.#setStatus('connecting');
      this.#openTransport();
    }, Math.min(backoff, remaining));
  }

  /** The one way to 'closed': drops the live transport and any retry, then reports why. */
  #end(reason: CloseReason): void {
    this.#clearReconnectTimer();
    this.#expectingResume = false;
    const transport = this.#transport;
    this.#transport = undefined; // its callbacks are stale from here on
    this.#closeReason = reason;
    if (transport !== undefined) {
      transport.close();
      this.events.emit('close', NORMAL_CLOSE);
    }
    this.#setStatus('closed');
    this.events.emit('ended', reason);
  }

  #clearReconnectTimer(): void {
    if (this.#reconnectTimer !== undefined) {
      clearTimeout(this.#reconnectTimer);
      this.#reconnectTimer = undefined;
    }
  }
}

/**
 * Connects over a real WebSocket and returns the live-to-be connection. Only usable where a
 * `WebSocket` global exists; everywhere else build an `AppricotConnection` over your own
 * `Transport`.
 */
export function connectAppricot(url: string, options: ConnectionOptions): AppricotConnection {
  const WebSocketCtor = (globalThis as { WebSocket?: typeof WebSocket }).WebSocket;
  if (WebSocketCtor === undefined) {
    throw new Error('No WebSocket global; construct AppricotConnection over a Transport.');
  }
  const conn = new AppricotConnection(() => new WebSocketTransport(url), options);
  conn.connect();
  return conn;
}
