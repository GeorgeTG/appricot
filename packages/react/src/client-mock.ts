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
 *
 * Every connectAppricot() call hands out a FRESH connection, with its own emitter and its
 * own mocks, and every `new SurfaceRegistry()` gets its own emitter: a provider that closes
 * the wrong connection, leaks a listener on an old one, or keeps feeding an old registry is
 * visible to the tests instead of hiding behind one shared object.
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

  /** Every listener on every event name. */
  totalListeners(): number {
    let total = 0;
    for (const set of this.#listeners.values()) total += set.size;
    return total;
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
  /** Marks the connection closed (status 'closed'); emits nothing, like a close whose
   * transport callback has not run yet. */
  readonly close: Mock;
}

/** One `new SurfaceRegistry()` the code under test made. */
export interface FakeRegistry {
  readonly events: FakeEmitter;
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
  /** Every connection connectAppricot handed out, oldest first: one distinct object per call. */
  readonly connections: () => readonly FakeConnection[];
  /** The newest connection, or undefined before the first connect. */
  readonly latest: () => FakeConnection | undefined;
  /** Every registry the code under test constructed, oldest first. */
  readonly registries: () => readonly FakeRegistry[];
  /** SurfaceRegistry.apply — what the provider feeds inbound envelopes into (every registry). */
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
  /** Backs `get` for every id. */
  setMeta(meta: FakeSurfaceRecord | undefined): void;
  /** Backs `get` per id; overrides setMeta for the ids it names. */
  setMetaFor(id: number, meta: FakeSurfaceRecord | undefined): void;
  setSurfaces(surfaces: readonly FakeSurfaceRecord[]): void;
  /** Sets the NEWEST connection's status and emits it, as the real connection would. */
  setStatus(status: FakeConnectionStatus): void;
  /** Emits one registry event on the NEWEST registry. */
  emitRegistry(event: string, value?: unknown): void;
  /** Emits one inbound envelope on the NEWEST connection. */
  deliverEnvelope(envelope: unknown): void;
  emitResumed(): void;
  /** How many listeners the newest registry's emitter holds for one event name. */
  registryListenerCount(event: string): number;
  /** How many listeners one connection's emitter holds, over every event name. */
  connectionListenerCount(connection: FakeConnection): number;
  reset(): void;
}

function createMock(): ClientMock {
  const rendererCalls: Array<{ canvas: unknown; surfaceId: number; deps: unknown }> = [];
  const inputCalls: Array<{ element: unknown; surfaceId: number; deps: unknown }> = [];
  const connections: FakeConnection[] = [];
  const registries: FakeRegistry[] = [];

  let meta: FakeSurfaceRecord | undefined;
  const metaById = new Map<number, FakeSurfaceRecord | undefined>();
  let surfaces: readonly FakeSurfaceRecord[] = [];

  const connect = vi.fn((): FakeConnection => {
    const connection: FakeConnection = {
      status: 'connecting',
      events: new FakeEmitter(),
      connect: vi.fn(),
      send: vi.fn(),
      close: vi.fn((): void => {
        connection.status = 'closed';
      }),
    };
    connections.push(connection);
    return connection;
  });

  const apply = vi.fn();
  const get = vi.fn((id: number): FakeSurfaceRecord | undefined =>
    metaById.has(id) ? metaById.get(id) : meta,
  );
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
      readonly events = new FakeEmitter();
      readonly apply = apply;
      readonly get = get;
      readonly list = list;
      constructor() {
        registries.push(this);
      }
    },
    SurfaceRenderer: { attach: rendererAttach },
    attachInput,
  };

  const mocks = [
    connect,
    apply,
    get,
    list,
    rendererAttach,
    rendererDetach,
    attachInput,
    detachInput,
  ];

  const latest = (): FakeConnection | undefined => connections.at(-1);
  const latestRegistry = (): FakeRegistry => {
    const registry = registries.at(-1);
    if (registry === undefined) {
      throw new Error('no SurfaceRegistry was constructed yet');
    }
    return registry;
  };
  const latestConnection = (): FakeConnection => {
    const connection = latest();
    if (connection === undefined) {
      throw new Error('connectAppricot was not called yet');
    }
    return connection;
  };

  return {
    overrides,
    connect,
    connections: () => connections,
    latest,
    registries: () => registries,
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
    setMetaFor(id: number, next: FakeSurfaceRecord | undefined): void {
      metaById.set(id, next);
    },
    setSurfaces(next: readonly FakeSurfaceRecord[]): void {
      surfaces = next;
    },
    setStatus(status: FakeConnectionStatus): void {
      const connection = latestConnection();
      connection.status = status;
      connection.events.emit('status', status);
    },
    emitRegistry(event: string, value?: unknown): void {
      latestRegistry().events.emit(event, value);
    },
    deliverEnvelope(envelope: unknown): void {
      latestConnection().events.emit('message', envelope);
    },
    emitResumed(): void {
      latestConnection().events.emit('resumed', undefined);
    },
    registryListenerCount(event: string): number {
      return latestRegistry().events.listenerCount(event);
    },
    connectionListenerCount(connection: FakeConnection): number {
      return connection.events.totalListeners();
    },
    reset(): void {
      rendererCalls.length = 0;
      inputCalls.length = 0;
      connections.length = 0;
      registries.length = 0;
      meta = undefined;
      metaById.clear();
      surfaces = [];
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
