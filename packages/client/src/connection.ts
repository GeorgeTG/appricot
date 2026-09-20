/**
 * The connection: a transport, the Hello handshake, and opt-in auto-reconnect with resume.
 *
 * Nothing here paces frames (rule 5, docs/protocol/README.md): `setTimeout` only spaces
 * reconnect attempts. Every envelope that decodes cleanly reaches the host through
 * `events`; nothing the server sends ever becomes markup or navigation (ADR-0003).
 */
import { Emitter } from './events';
import {
  CODEC,
  MAX_CLIPBOARD_BYTES,
  PROTOCOL_VERSION,
  decodeEnvelope,
  encodeEnvelope,
} from './protocol';
import type { Bye, Envelope, Hello, HelloReply } from './protocol';

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
  | 'closed'; // terminal: by the user, by the peer, or after a local failure

export interface ConnectionEvents {
  /** Every envelope that decoded cleanly, in arrival order — including the hostile ones. */
  message: Envelope;
  /** The transport closed; the value is the WebSocket close code (1006 when abnormal). */
  close: number;
  status: ConnectionStatus;
  /** A reconnect's resume was honoured; the window set and one full frame follow. */
  resumed: void;
}

export interface ConnectionOptions {
  /** The per-session stream token from Hello; opaque bytes that must match or the server
   * closes with BYE_AUTH_FAILED. Never travels in a URL (rule 8). */
  token: Uint8Array;
  /** Reconnect with resume after an abnormal close. Default false. */
  reconnect?: boolean;
}

/** Backoff for reconnect attempts: 500 ms doubling to at most 8 s, reset by a HelloReply. */
const RECONNECT_BASE_MS = 500;
const RECONNECT_MAX_MS = 8000;

/** Bye reasons after which this client never reconnects (wire.proto ByeReason values). */
const FATAL_BYE_REASONS = new Set([0x100, 0x102, 0x104]); // VERSION, AUTH_FAILED, SESSION_GONE

/** The normal WebSocket close code; anything else counts as abnormal and may be retried. */
const NORMAL_CLOSE = 1000;

export class AppricotConnection {
  readonly events = new Emitter<ConnectionEvents>();
  readonly #makeTransport: () => Transport;
  readonly #token: Uint8Array;
  readonly #reconnectEnabled: boolean;

  #status: ConnectionStatus = 'idle';
  #transport: Transport | undefined;
  #resumeSerial: number | undefined;
  #backoffMs = 0;
  #reconnectTimer: ReturnType<typeof setTimeout> | undefined;
  #userClosed = false;
  #noReconnect = false;
  #expectingResume = false;

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

  /** Opens the transport and sends Hello when it is up. Calling while live is a no-op. */
  connect(): void {
    if (this.#status === 'connecting' || this.#status === 'open') {
      return;
    }
    this.#userClosed = false;
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

  /** Closes. Sends nothing extra — the transport close is the whole goodbye. */
  close(): void {
    this.#userClosed = true;
    this.#clearReconnectTimer();
    const transport = this.#transport;
    this.#transport = undefined;
    if (transport === undefined) {
      this.#setStatus('closed');
      return;
    }
    transport.close(); // the close callback finishes the transition and emits 'close'
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
    } catch {
      // The factory itself failed; treat it as an abnormal close and let the backoff decide.
      this.#handleClose(1006);
      return;
    }
    this.#transport = transport;
    transport.onOpen(() => this.#handleOpen());
    transport.onMessage((data) => this.#handleMessage(data));
    transport.onClose((code) => this.#handleClose(code));
  }

  #handleOpen(): void {
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
    this.#transport?.send(encodeEnvelope({ kind: 'hello', hello }));
  }

  #handleMessage(data: Uint8Array): void {
    let envelope: Envelope;
    try {
      envelope = decodeEnvelope(data);
    } catch {
      // Bytes that do not decode close the connection locally and never retry (ADR-0003 §4:
      // a violation closes; it also never throws into the host).
      this.#noReconnect = true;
      this.close();
      return;
    }
    this.events.emit('message', envelope);
    if (envelope.kind === 'helloReply') {
      this.#handleReply(envelope.helloReply);
    } else if (envelope.kind === 'bye') {
      this.#handleBye(envelope.bye);
    }
  }

  #handleReply(reply: HelloReply): void {
    if (reply.protocolVersion !== PROTOCOL_VERSION) {
      // The server may not guess a version and neither do we (rule 1); fail without retry.
      this.#noReconnect = true;
      this.close();
      return;
    }
    this.sessionId = reply.sessionId;
    this.maxFrameCredits = reply.maxFrameCredits;
    if (reply.resumeSerial !== undefined) {
      this.#resumeSerial = reply.resumeSerial;
    }
    this.#backoffMs = 0; // a live session resets the backoff
    this.#setStatus('open');
    if (this.#expectingResume && reply.resumed) {
      this.#expectingResume = false;
      this.events.emit('resumed', undefined);
    }
    if (!reply.resumed) {
      this.#expectingResume = false;
    }
  }

  #handleBye(bye: Bye): void {
    if (FATAL_BYE_REASONS.has(bye.reason)) {
      // AUTH_FAILED / SESSION_GONE / PROTOCOL_VERSION: retrying cannot change the answer.
      this.#noReconnect = true;
    }
    // The socket close that follows drives the rest; the host saw the Bye via 'message'.
  }

  #handleClose(code: number): void {
    this.#transport = undefined;
    if (this.#status === 'closed') {
      return; // already terminal; the close event was emitted for the first one
    }
    this.events.emit('close', code);
    this.#setStatus('closed');
    if (
      this.#reconnectEnabled &&
      !this.#userClosed &&
      !this.#noReconnect &&
      code !== NORMAL_CLOSE
    ) {
      this.#scheduleReconnect();
    }
  }

  #scheduleReconnect(): void {
    const delay = this.#backoffMs === 0 ? RECONNECT_BASE_MS : this.#backoffMs;
    this.#backoffMs = Math.min(delay * 2, RECONNECT_MAX_MS);
    this.#reconnectTimer = setTimeout(() => {
      this.#reconnectTimer = undefined;
      if (this.#userClosed || this.#noReconnect) {
        return;
      }
      this.#setStatus('connecting');
      this.#openTransport();
    }, delay);
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
