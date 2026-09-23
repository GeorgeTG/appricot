/**
 * The demo's window manager, as pure state: the set of streamed toplevels, the z-order and
 * focus, and which popups belong to which parent. It never touches the DOM and never talks to
 * the connection — `src/main.ts` feeds it registry events and mirrors the results into chrome.
 *
 * The host is the compositor (docs/architecture.md §4): it alone decides position, stacking
 * and focus, and a server request to change any of those is answered by the host's own rules,
 * not obeyed.
 */
import { cascadePosition, type Size } from './geometry';

/** One floating toplevel. `title` and `appId` are untrusted text (ADR-0003 §1). */
export interface ToplevelState {
  readonly surfaceId: number;
  /** Untrusted text. */
  appId: string;
  /** Untrusted text. */
  title: string;
  /** The logical size the host last proposed or the app last acked. */
  size: Size;
  /** Page position of the window's top-left corner, in logical pixels. */
  x: number;
  y: number;
}

/** A window the wm removed, with what focus should do next. */
export interface Removal {
  removed: ToplevelState;
  /** The popup surface ids that went with it (the wm cascades; the registry may or may not
   * have reported each one already). */
  popups: number[];
  /** The surface to focus now that the top of the stack changed, or null when no window is
   * left. Null also means "focus left every APPricot surface": the caller sends BlurRelease. */
  refocus: number | null;
}

/** A window the wm minimized, with what focus should do next. */
export interface MinimizeResult {
  minimized: ToplevelState;
  /** The popup surface ids that hide with the parent (they are positioned relative to it). */
  popups: number[];
  /**
   * The surface to focus now: the new top of the visible stack when the minimized window held
   * focus, the unchanged focus otherwise. Null means no window is left to focus — the caller
   * sends BlurRelease (a hidden window can never hold focus).
   */
  refocus: number | null;
}

export class WindowManager {
  #windows = new Map<number, ToplevelState>();
  /** Bottom-to-top stacking order; the last entry is the topmost window. A minimized window
   * leaves this order (it is not painted, focused or raised) but stays tracked in
   * `#windows`, so its state, its slot and its popups survive the round-trip. */
  #stack: number[] = [];
  /** Minimized surface ids, in minimize order (a Set iterates in insertion order). */
  #minimized = new Set<number>();
  #popupsByParent = new Map<number, Set<number>>();
  #focused: number | null = null;

  /** Every tracked toplevel, bottom-to-top. */
  order(): number[] {
    return [...this.#stack];
  }

  get(surfaceId: number): ToplevelState | undefined {
    return this.#windows.get(surfaceId);
  }

  /** The focused surface id, or null when focus is on no streamed window. */
  get focusedId(): number | null {
    return this.#focused;
  }

  /** The popup surface ids parented to `parentId`, in insertion order. */
  popupsOf(parentId: number): number[] {
    return [...(this.#popupsByParent.get(parentId) ?? [])];
  }

  /** Every minimized window, in minimize order (oldest first). */
  minimizedIds(): number[] {
    return [...this.#minimized];
  }

  /** True when the window is minimized (hidden; still tracked with its state and popups). */
  isMinimized(surfaceId: number): boolean {
    return this.#minimized.has(surfaceId);
  }

  /**
   * Adds a toplevel at the next cascade slot. A surface id the wm already tracks is ignored:
   * the registry never reuses ids, so a repeat is a replay and not a new window. Returns the
   * placed state, or undefined when it was ignored.
   */
  addToplevel(record: {
    surfaceId: number;
    appId: string;
    title: string;
    size: Size;
  }, viewport: Size): ToplevelState | undefined {
    if (this.#windows.has(record.surfaceId)) {
      return undefined;
    }
    // The cascade index counts every tracked window, minimized ones included: a window
    // arriving while others are minimized must not land on a hidden window's slot and sit
    // on top of it the moment it is restored.
    const placed = cascadePosition(this.#windows.size, record.size, viewport);
    const state: ToplevelState = { ...record, x: placed.x, y: placed.y };
    this.#windows.set(state.surfaceId, state);
    this.#stack.push(state.surfaceId);
    return state;
  }

  /** Records a popup under its parent. Unknown parents are ignored (the registry drops those
   * already; this is the wm's own spine, not a duplicate of the registry's rule). */
  addPopup(surfaceId: number, parentId: number): void {
    const parent = this.#popupsByParent.get(parentId);
    if (parent === undefined) {
      if (!this.#windows.has(parentId)) {
        return;
      }
      this.#popupsByParent.set(parentId, new Set([surfaceId]));
      return;
    }
    parent.add(surfaceId);
  }

  /** Forgets a popup. Returns true when it was tracked. */
  removePopup(surfaceId: number, parentId: number): boolean {
    return this.#popupsByParent.get(parentId)?.delete(surfaceId) ?? false;
  }

  /**
   * Removes a toplevel (and its popups) and names the surface focus moves to: the new top of
   * the stack, or null when the last window went. Removing an unknown id reports undefined.
   */
  removeToplevel(surfaceId: number): Removal | undefined {
    const removed = this.#windows.get(surfaceId);
    if (removed === undefined) {
      return undefined;
    }
    this.#windows.delete(surfaceId);
    this.#stack = this.#stack.filter((id) => id !== surfaceId);
    this.#minimized.delete(surfaceId);
    const popups = [...(this.#popupsByParent.get(surfaceId) ?? [])];
    this.#popupsByParent.delete(surfaceId);
    const refocus = this.#focused === surfaceId ? (this.top() ?? null) : this.#focused;
    if (this.#focused === surfaceId) {
      this.#focused = refocus;
    }
    return { removed, popups, refocus };
  }

  /** The topmost toplevel id, or undefined when none is tracked. */
  top(): number | undefined {
    return this.#stack.at(-1);
  }

  /**
   * Moves a window's focus to the top of the stack. Returns the new z-index the caller should
   * paint the window at, or undefined for an unknown id. Focusing the already-focused window
   * still raises it — that is ordinary desktop behaviour. A minimized window cannot be
   * focused: restoring it is the only thing that brings it back, so an attempt (a hostile
   * focus-ask, say) changes nothing instead of resurrecting a hidden window into the stack.
   */
  focus(surfaceId: number): number | undefined {
    if (!this.#windows.has(surfaceId) || this.#minimized.has(surfaceId)) {
      return undefined;
    }
    this.#stack = this.#stack.filter((id) => id !== surfaceId);
    this.#stack.push(surfaceId);
    this.#focused = surfaceId;
    return this.#stack.length;
  }

  /** Marks that focus left every streamed window (the host sends BlurRelease). */
  blur(): void {
    this.#focused = null;
  }

  /** Moves a window. Unknown ids are ignored; the caller is expected to clamp first. */
  move(surfaceId: number, x: number, y: number): void {
    const state = this.#windows.get(surfaceId);
    if (state === undefined) {
      return;
    }
    state.x = x;
    state.y = y;
  }

  /**
   * Records a window's new title and app id (untrusted text, from a SurfaceMetadata), so
   * every label drawn from the wm — the restore strip, say — shows the current one and not the
   * title the window was created with. Unknown ids are ignored.
   */
  setMetadata(surfaceId: number, metadata: { title: string; appId: string }): void {
    const state = this.#windows.get(surfaceId);
    if (state === undefined) {
      return;
    }
    state.title = metadata.title;
    state.appId = metadata.appId;
  }

  /** Resizes a window (a host proposal or a configure ack). Unknown ids are ignored. */
  resize(surfaceId: number, size: Size): void {
    const state = this.#windows.get(surfaceId);
    if (state === undefined) {
      return;
    }
    state.size = { ...size };
  }

  /**
   * Minimizes a window: it leaves the stacking order (the caller hides it; the surface, its
   * canvas and its renderer all stay exactly where they were) and can no longer be focused or
   * raised until restored. Minimizing the focused window moves focus to the top-most remaining
   * visible window, or to nothing — `refocus: null` — when none remains, which is the caller's
   * cue to send BlurRelease. Unknown or already-minimized ids report undefined.
   */
  minimize(surfaceId: number): MinimizeResult | undefined {
    const state = this.#windows.get(surfaceId);
    if (state === undefined || this.#minimized.has(surfaceId)) {
      return undefined;
    }
    this.#minimized.add(surfaceId);
    this.#stack = this.#stack.filter((id) => id !== surfaceId);
    const refocus = this.#focused === surfaceId ? (this.top() ?? null) : this.#focused;
    if (this.#focused === surfaceId) {
      this.#focused = refocus;
    }
    return { minimized: state, popups: this.popupsOf(surfaceId), refocus };
  }

  /**
   * Restores a minimized window to the top of the stack and focuses it (a restore is an
   * activation, like clicking a taskbar entry). Returns the new z-index the caller should
   * paint the window at, or undefined when the id is not a minimized window. The window keeps
   * the position, size and popups it had when it was minimized.
   */
  restore(surfaceId: number): number | undefined {
    if (!this.#minimized.delete(surfaceId)) {
      return undefined;
    }
    this.#stack.push(surfaceId);
    this.#focused = surfaceId;
    return this.#stack.length;
  }
}
