import { describe, expect, it, vi } from 'vitest';

import { Emitter } from './events';

interface TestEvents {
  ping: number;
  name: string;
  nothing: undefined;
}

describe('Emitter', () => {
  it('delivers each event to its listeners with the typed payload', () => {
    const emitter = new Emitter<TestEvents>();
    const pings: number[] = [];
    const names: string[] = [];
    emitter.on('ping', (n) => pings.push(n));
    emitter.on('name', (s) => names.push(s));

    emitter.emit('ping', 7);
    emitter.emit('name', 'a title');
    emitter.emit('ping', 8);

    expect(pings).toEqual([7, 8]);
    expect(names).toEqual(['a title']);
  });

  it('emitting with no subscribers is a no-op', () => {
    const emitter = new Emitter<TestEvents>();
    expect(() => emitter.emit('ping', 1)).not.toThrow();
  });

  it('off removes exactly one listener', () => {
    const emitter = new Emitter<TestEvents>();
    const first = vi.fn();
    const second = vi.fn();
    emitter.on('ping', first);
    emitter.on('ping', second);
    emitter.off('ping', first);

    emitter.emit('ping', 1);

    expect(first).not.toHaveBeenCalled();
    expect(second).toHaveBeenCalledTimes(1);
  });

  it('the function returned by on unsubscribes', () => {
    const emitter = new Emitter<TestEvents>();
    const listener = vi.fn();
    const off = emitter.on('ping', listener);
    off();
    off(); // double off is harmless

    emitter.emit('ping', 1);

    expect(listener).not.toHaveBeenCalled();
  });

  it('a listener unsubscribing itself mid-emit does not break the emit', () => {
    const emitter = new Emitter<TestEvents>();
    const seen: number[] = [];
    const first = (n: number): void => {
      seen.push(n);
      emitter.off('ping', first);
    };
    emitter.on('ping', first);
    emitter.on('ping', (n) => seen.push(n * 10));

    emitter.emit('ping', 3);

    expect(seen).toEqual([3, 30]);
  });

  it('a listener subscribing mid-emit hears the next emit only', () => {
    const emitter = new Emitter<TestEvents>();
    const late = vi.fn();
    emitter.on('ping', () => emitter.on('ping', late));

    emitter.emit('ping', 1);
    expect(late).not.toHaveBeenCalled();

    emitter.emit('ping', 2);
    expect(late).toHaveBeenCalledWith(2);
  });

  it('isolates a throwing listener: the others still hear the emit, nothing is rethrown', () => {
    const emitter = new Emitter<TestEvents>();
    const errors = vi.spyOn(console, 'error').mockImplementation(() => undefined);
    const after = vi.fn();
    emitter.on('ping', () => {
      throw new Error('a host bug');
    });
    emitter.on('ping', after);

    try {
      expect(() => emitter.emit('ping', 4)).not.toThrow();
      expect(after).toHaveBeenCalledWith(4);
      expect(errors).toHaveBeenCalledTimes(1);
    } finally {
      errors.mockRestore();
    }
  });

  it('carries an undefined payload without confusing it for a missing one', () => {
    const emitter = new Emitter<TestEvents>();
    const heard: undefined[] = [];
    emitter.on('nothing', (v) => heard.push(v));

    emitter.emit('nothing', undefined);

    expect(heard).toEqual([undefined]);
  });
});
