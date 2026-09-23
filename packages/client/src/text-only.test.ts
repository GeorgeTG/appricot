// @vitest-environment jsdom
import { describe, expect, it } from 'vitest';

import { setTextOnly } from './text-only.js';

describe('setTextOnly', () => {
  it('shows markup as literal text and creates no element', () => {
    const target = document.createElement('span');
    const hostile = '<img src=x onerror="alert(1)"><b>bold</b>';

    setTextOnly(target, hostile);

    expect(target.textContent).toBe(hostile);
    expect(target.childElementCount).toBe(0);
    expect(target.querySelector('img')).toBeNull();
  });

  it('replaces whatever the target held before', () => {
    const target = document.createElement('div');
    target.append(document.createElement('p'), 'old text');

    setTextOnly(target, 'new title');

    expect(target.childNodes).toHaveLength(1);
    expect(target.textContent).toBe('new title');
  });
});
