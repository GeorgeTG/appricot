import { useCallback } from 'react';

import { setTextOnly } from './client.js';
import { useSurfaceMeta } from './hooks.js';

/** The text-only elements AppricotTitle can render. The title never becomes markup. */
export type AppricotTitleElement = 'div' | 'h1' | 'h2' | 'h3' | 'h4' | 'h5' | 'h6' | 'p' | 'span';

export interface AppricotTitleProps {
  /** The surface whose title is shown. */
  readonly id: number;
  /** The element to render; a span by default. */
  readonly as?: AppricotTitleElement;
}

/**
 * Shows one surface's title as text — the proof-of-pattern for hosts (ADR-0003 §1).
 *
 * The title is server data and therefore hostile. React never sees it as children: the
 * component renders an empty element and setTextOnly writes it as a text node, which the
 * browser never parses as markup. Truncation is the host's CSS, not ours.
 */
export function AppricotTitle({ id, as = 'span' }: AppricotTitleProps) {
  const meta = useSurfaceMeta(id);
  const title = meta?.title ?? '';
  // A callback ref instead of children: React re-invokes it whenever the title changes, and
  // the supertype parameter satisfies every element in AppricotTitleElement without a cast.
  const attach = useCallback((node: HTMLElement | null) => {
    if (node !== null) setTextOnly(node, title);
  }, [title]);
  const Tag = as;
  return <Tag ref={attach} />;
}
