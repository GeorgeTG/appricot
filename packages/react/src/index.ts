/**
 * @appricot/react: React bindings for @appricot/client.
 *
 * A provider, hooks over the window registry, and (later) a window canvas component. The host
 * renders its own chrome around each window; these bindings render none.
 *
 * At bootstrap it exports a stub provider and its hook.
 */
export { AppricotProvider, useAppricot } from './provider';
export type { AppricotContextValue, AppricotProviderProps } from './provider';
