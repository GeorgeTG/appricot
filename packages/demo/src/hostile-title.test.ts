// @vitest-environment jsdom
import { setTextOnly } from '@appricot/client';
import { describe, expect, it } from 'vitest';

/**
 * A hostile window title, exactly the shape a compromised streamer would send, rendered the
 * way the demo renders every server string: through `setTextOnly` (ADR-0003 §1). What lands
 * in the DOM must be one text node — no elements, no attributes, no script — and reading it
 * back must yield the original bytes. (`innerHTML` is a forbidden property name in this
 * repository, so the assertions work from the child list instead.)
 */
const HOSTILE_TITLE =
  '<img src=x onerror="fetch(`/leak?c=${document.cookie}`)"><script>alert(1)</script>' +
  '<svg onload=alert(2)></svg><iframe srcdoc="<script>alert(3)</script>">"onmouseover="alert(4)';

describe('a hostile window title rendered through setTextOnly', () => {
  it('leaves the element element-free: one text child, no markup', () => {
    const title = document.createElement('span');
    setTextOnly(title, HOSTILE_TITLE);
    expect(title.childElementCount).toBe(0);
    expect(title.childNodes.length).toBe(1);
    expect(title.firstChild?.nodeType).toBe(Node.TEXT_NODE);
  });

  it('keeps the string intact as text (nothing stripped, nothing parsed)', () => {
    const title = document.createElement('span');
    setTextOnly(title, HOSTILE_TITLE);
    expect(title.textContent).toBe(HOSTILE_TITLE);
  });

  it('replaces what was there before, so a stale title cannot linger', () => {
    const title = document.createElement('span');
    title.append('old title', document.createElement('b'));
    setTextOnly(title, HOSTILE_TITLE);
    expect(title.childElementCount).toBe(0);
    expect(title.textContent).toBe(HOSTILE_TITLE);
  });
});
