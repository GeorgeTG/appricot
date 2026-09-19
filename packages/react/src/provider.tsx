import { createContext, useContext, type ReactNode } from 'react';

/**
 * What the provider shares with the components below it.
 *
 * A stub. The connection and the window registry from @appricot/client arrive here with the
 * client MVP (docs/roadmap.md, M2).
 */
export interface AppricotContextValue {
  /** The connection's state. Always `idle` until the provider owns a connection. */
  readonly status: 'idle';
}

const AppricotContext = createContext<AppricotContextValue | null>(null);

const IDLE: AppricotContextValue = { status: 'idle' };

export interface AppricotProviderProps {
  readonly children?: ReactNode;
}

/**
 * Makes APPricot available to the components below it.
 *
 * It renders no chrome and no markup of its own: the host draws every window frame.
 */
export function AppricotProvider({ children }: AppricotProviderProps) {
  return <AppricotContext value={IDLE}>{children}</AppricotContext>;
}

/** Reads the nearest provider's value. Throws when there is no provider above. */
export function useAppricot(): AppricotContextValue {
  const value = useContext(AppricotContext);
  if (value === null) {
    throw new Error('useAppricot() must be called inside <AppricotProvider>.');
  }
  return value;
}
