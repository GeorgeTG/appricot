/**
 * @appricot/react: React bindings for @appricot/client.
 *
 * A provider that owns the connection and the window registry, hooks over the registry, a
 * canvas component per surface and a text-only title component. The host renders its own
 * chrome around each window; these bindings render none (ADR-0003 §5).
 */
export { AppricotProvider, useAppricot } from './provider';
export type { AppricotContextValue, AppricotProviderProps } from './provider';
export {
  useConnectionStatus,
  useSessionEvents,
  useSurfaceMeta,
  useWindows,
} from './hooks';
export type { SessionEvent } from './hooks';
export { AppricotSurface } from './surface';
export type { AppricotSurfaceProps } from './surface';
export { AppricotTitle } from './title';
export type { AppricotTitleElement, AppricotTitleProps } from './title';
// The client types the hooks expose, re-exported so a host needs no direct
// @appricot/client import to consume them.
export type {
  ConnectionStatus,
  Envelope,
  RegistryEvents,
  SurfaceRecord,
} from './client';
