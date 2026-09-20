/**
 * A tiny typed event emitter: `on`/`off`/`emit` over a `Map`.
 *
 * Hand-rolled on purpose (ADR-0003 §8): a dependency for this would be weight without
 * safety, and a typed `Events` map keeps every listener checkable at compile time.
 */

/** A listener for one event of an `Emitter`. */
export type Listener<Events extends object, K extends keyof Events> = (value: Events[K]) => void;

export class Emitter<Events extends object> {
  // Every listener is stored as `(value: never) => void`: `never` is the bottom type, so any
  // listener is assignable to it, and `emit` is the only place that casts back.
  readonly #byEvent = new Map<keyof Events, Set<(value: never) => void>>();

  /** Subscribes; the returned function unsubscribes (usable inline, no binding needed). */
  on<K extends keyof Events>(event: K, listener: Listener<Events, K>): () => void {
    let listeners = this.#byEvent.get(event);
    if (listeners === undefined) {
      listeners = new Set();
      this.#byEvent.set(event, listeners);
    }
    listeners.add(listener as (value: never) => void);
    return () => this.off(event, listener);
  }

  /** Removes one listener. Removing a listener that is not subscribed is a no-op. */
  off<K extends keyof Events>(event: K, listener: Listener<Events, K>): void {
    this.#byEvent.get(event)?.delete(listener as (value: never) => void);
  }

  /**
   * Emits to the listeners subscribed at the moment of the call. A listener that unsubscribes
   * itself mid-emit is still safe; a listener that subscribes mid-emit hears the next emit,
   * not this one.
   */
  emit<K extends keyof Events>(event: K, value: Events[K]): void {
    const listeners = this.#byEvent.get(event);
    if (listeners === undefined) {
      return;
    }
    for (const listener of [...listeners]) {
      listener(value as never);
    }
  }
}
