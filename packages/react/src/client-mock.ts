/**
 * Test infrastructure: a controllable fake of @appricot/client's public surface, shared by
 * every test file in this package. Not shipped (excluded from tsconfig.build.json).
 *
 * Usage at the top of a test file (the factory must reach this module with a dynamic
 * import, because vi.mock is hoisted above static imports):
 *
 *   vi.mock('@appricot/client', async (importOriginal) => ({
 *     ...(await importOriginal<Record<string, unknown>>()),
 *     ...mockClientModule(),
 *   }));
 *
 * and then `clientMock()` in the test body. The importOriginal spread keeps the REAL
 * setTextOnly, so the hostile-title test exercises the real text-only path. This module
 * must not import anything that (transitively) imports '@appricot/client' — the vi.mock
 * factory runs while that module is being resolved.
 */
import { vi, type Mock } from 'vitest';

/** The Emitter's shape, hand-rolled here so this module stays free of '@appricot/client'. */
class FakeEmitter {
  readonly #listeners = new Map<string, Set<(value: unknown) => void>>();

  on(event: string, listener: (value: unknown) => void): () => void {
    let set = this.#listeners.get(event);
    if (set === undefined) {
      set = new Set();
      this.#listeners.set(event, set);
    }
    set.add(listener);
    return () => {
      set.delete(listener);
    };
  }

  emit(event: string, value?: unknown): void {
    const set = this.#listeners.get(event);
    if (set === undefined) {
      return;
    }
    for (const listener of [...set]) listener(value);
  }

  listenerCount(event: string): number {
    return this.#listeners.get(event)?.size ?? 0;
  }

  clear(): void {
    this.#listeners.clear();
  }
}

export type FakeConnectionStatus = 'idle' | 'connecting' | 'open' | 'closed';

export interface FakeConnection {
  status: FakeConnectionStatus;
  readonly events: FakeEmitter;
  readonly connect: Mock;
  readonly send: Mock;
  readonly close: Mock;
}

/**
 * A surface as the fake registry hands out. Field names mirror the SDK's SurfaceRecord;
 * `parent` is always present and undefined for a plain toplevel, which keeps
 * exactOptionalPropertyTypes out of the builders' way.
 */
export interface FakeSurfaceRecord {
  readonly id: number;
  readonly role: 0 | 1;
  readonly parent: number | undefined;
  readonly title: string;
  readonly appId: string;
  /** Device pixels per logical pixel in 120ths, as the wire carries it. */
  readonly scale: number;
  readonly size: { readonly width: number; readonly height: number };
  readonly positioner: unknown;
  readonly lastSequence: number;
}

export type FakeSurfaceRecordOverrides = Partial<Omit<FakeSurfaceRecord, 'id' | 'parent'>> & {
  parent?: number;
};

export function fakeSurfaceRecord(
  id: number,
  overrides: FakeSurfaceRecordOverrides = {},
): FakeSurfaceRecord {
  return {
    id,
    role: overrides.role ?? 0,
    parent: overrides.parent ?? undefined,
    title: overrides.title ?? 'A window',
    appId: overrides.appId ?? 'org.appricot.Example',
    scale: overrides.scale ?? 120,
    size: overrides.size ?? { width: 640, height: 480 },
    positioner: overrides.positioner ?? undefined,
    lastSequence: overrides.lastSequence ?? 0,
  };
}

export interface ClientMock {
  /** The '@appricot/client' overrides a vi.mock factory spreads over the real module. */
  readonly overrides: Record<string, unknown>;
  /** connectAppricot; one call per provider connection. */
  readonly connect: Mock;
  /** Every connection connectAppricot handed out (the same object, once per call). */
  readonly connections: () => readonly FakeConnection[];
  /** SurfaceRegistry.apply — what the provider feeds inbound envelopes into. */
  readonly apply: Mock;
  /** SurfaceRegistry.get — back it with setMeta(). */
  readonly get: Mock;
  /** SurfaceRegistry.list — back it with setSurfaces(). */
  readonly list: Mock;
  readonly attachInput: Mock;
  /** The detach function attachInput hands back. */
  readonly detachInput: Mock;
  /** The detach SurfaceRenderer.attach hands back. */
  readonly rendererDetach: Mock;
  /** One entry per SurfaceRenderer.attach(canvas, surfaceId, deps). */
  readonly rendererCalls: () => ReadonlyArray<{
    readonly canvas: unknown;
    readonly surfaceId: number;
    readonly deps: unknown;
  }>;
  /** One entry per attachInput(element, surfaceId, deps). */
  readonly inputCalls: () => ReadonlyArray<{
    readonly element: unknown;
    readonly surfaceId: number;
    readonly deps: unknown;
  }>;
  setMeta(meta: FakeSurfaceRecord | undefined): void;
  setSurfaces(surfaces: readonly FakeSurfaceRecord[]): void;
  /** Sets the connection status and emits it, as the real connection would. */
  setStatus(status: FakeConnectionStatus): void;
  emitRegistry(event: string, value?: unknown): void;
  deliverEnvelope(envelope: unknown): void;
  emitResumed(): void;
  /** How many listeners the fake registry emitter holds for one event name. */
  registryListenerCount(event: string): number;
  reset(): void;
}

function createMock(): ClientMock {
  const connEvents = new FakeEmitter();
  const registryEvents = new FakeEmitter();
  const rendererCalls: Array<{ canvas: unknown; surfaceId: number; deps: unknown }> = [];
  const inputCalls: Array<{ element: unknown; surfaceId: number; deps: unknown }> = [];
  const connections: FakeConnection[] = [];

  let meta: FakeSurfaceRecord | undefined;
  let surfaces: readonly FakeSurfaceRecord[] = [];

  const connection: FakeConnection = {
    status: 'connecting',
    events: connEvents,
    connect: vi.fn(),
    send: vi.fn(),
    close: vi.fn(),
  };
  const connect = vi.fn((): FakeConnection => {
    connections.push(connection);
    return connection;
  });

  const apply = vi.fn();
  const get = vi.fn((): FakeSurfaceRecord | undefined => meta);
  const list = vi.fn((): FakeSurfaceRecord[] => [...surfaces]);

  const rendererDetach = vi.fn();
  const rendererAttach = vi.fn(
    (canvas: unknown, surfaceId: number, deps: unknown): { detach: Mock } => {
      rendererCalls.push({ canvas, surfaceId, deps });
      return { detach: rendererDetach };
    },
  );

  const detachInput = vi.fn();
  const attachInput = vi.fn((element: unknown, surfaceId: number, deps: unknown): Mock => {
    inputCalls.push({ element, surfaceId, deps });
    return detachInput;
  });

  const overrides: Record<string, unknown> = {
    connectAppricot: connect,
    SurfaceRegistry: class {
      readonly events = registryEvents;
      readonly apply = apply;
      readonly get = get;
      readonly list = list;
    },
    SurfaceRenderer: { attach: rendererAttach },
    attachInput,
  };

  const mocks = [
    connect,
    connection.connect,
    connection.send,
    connection.close,
    apply,
    get,
    list,
    rendererAttach,
    rendererDetach,
    attachInput,
    detachInput,
  ];

  return {
    overrides,
    connect,
    connections: () => connections,
    apply,
    get,
    list,
    attachInput,
    detachInput,
    rendererDetach,
    rendererCalls: () => rendererCalls,
    inputCalls: () => inputCalls,
    setMeta(next: FakeSurfaceRecord | undefined): void {
      meta = next;
    },
    setSurfaces(next: readonly FakeSurfaceRecord[]): void {
      surfaces = next;
    },
    setStatus(status: FakeConnectionStatus): void {
      connection.status = status;
      connEvents.emit('status', status);
    },
    emitRegistry(event: string, value?: unknown): void {
      registryEvents.emit(event, value);
    },
    deliverEnvelope(envelope: unknown): void {
      connEvents.emit('message', envelope);
    },
    emitResumed(): void {
      connEvents.emit('resumed', undefined);
    },
    registryListenerCount(event: string): number {
      return registryEvents.listenerCount(event);
    },
    reset(): void {
      connEvents.clear();
      registryEvents.clear();
      rendererCalls.length = 0;
      inputCalls.length = 0;
      connections.length = 0;
      meta = undefined;
      surfaces = [];
      connection.status = 'connecting';
      for (const mock of mocks) mock.mockClear();
    },
  };
}

let current: ClientMock | null = null;

/** vi.mock factory entry: returns the '@appricot/client' overrides. */
export function mockClientModule(): Record<string, unknown> {
  current = createMock();
  return current.overrides;
}

/** The mock this file's mockClientModule() created for the importing test file. */
export function clientMock(): ClientMock {
  if (current === null) {
    throw new Error('clientMock() called before mockClientModule() ran.');
  }
  return current;
}
