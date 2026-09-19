// @vitest-environment jsdom
import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';

import { AppricotProvider, useAppricot } from './provider';

afterEach(() => {
  cleanup();
});

function Status() {
  const { status } = useAppricot();
  return <output data-testid="status">{status}</output>;
}

describe('AppricotProvider', () => {
  it('gives the components below it the context', () => {
    render(
      <AppricotProvider>
        <Status />
      </AppricotProvider>,
    );
    expect(screen.getByTestId('status').textContent).toBe('idle');
  });
});
