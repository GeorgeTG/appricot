/**
 * @app-ricot/react: React bindings for @app-ricot/client.
 *
 * A provider that owns the connection and the window registry, hooks over the registry, a
 * canvas component per surface and a text-only title component. The host renders its own
 * chrome around each window; these bindings render none (ADR-0003 §5).
 */
export { AppricotProvider, useAppricot } from './provider.js';
export type { AppricotContextValue, AppricotProviderProps } from './provider.js';
export {
  useConnectionStatus,
  useSessionEvents,
  useSurfaceMeta,
  useWindows,
} from './hooks.js';
export type { SessionEvent } from './hooks.js';
export { AppricotSurface } from './surface.js';
export type { AppricotResizeAsk, AppricotSurfaceProps } from './surface.js';
export { AppricotTitle } from './title.js';
export type { AppricotTitleElement, AppricotTitleProps } from './title.js';
// The client types the hooks expose, re-exported so a host needs no direct
// @app-ricot/client import to consume them.
export type {
  ConnectionStatus,
  Envelope,
  PastePolicy,
  RegistryEvents,
  SurfaceRecord,
} from './client.js';
