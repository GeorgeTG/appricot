// @vitest-environment jsdom
import { describe, expect, it } from 'vitest';

import { connectStateFromForm, tokenFromForm } from './token';

describe('tokenFromForm', () => {
  it('encodes the input value exactly, with no trimming or normalization', () => {
    const state = tokenFromForm({ value: 'demo-token' });
    expect('error' in state).toBe(false);
    if (!('error' in state)) {
      expect(Array.from(state.token)).toEqual(Array.from(new TextEncoder().encode('demo-token')));
    }
  });

  it('keeps surrounding space: the token is opaque bytes, not prose', () => {
    const state = tokenFromForm({ value: ' padded ' });
    if (!('error' in state)) {
      expect(new TextDecoder().decode(state.token)).toBe(' padded ');
    } else {
      expect.unreachable('a padded token is still a token');
    }
  });

  it('refuses an empty field instead of sending an empty token', () => {
    expect(tokenFromForm({ value: '' })).toEqual({ error: 'empty-token' });
  });
});

describe('the page state never takes the token from the URL', () => {
  it('builds connect state from the form even when the query string carries a token', () => {
    // A hostile or careless link such as .../?token=evil must contribute nothing. The
    // connect state is built from the form field alone; the search string is accepted and
    // ignored (see connectStateFromForm's signature).
    const state = connectStateFromForm({ value: 'the-real-token' }, '?token=evil&token2=x');
    if ('error' in state) {
      expect.unreachable('the real token must win');
    } else {
      expect(new TextDecoder().decode(state.token)).toBe('the-real-token');
    }
  });

  it('still refuses an empty form field whatever the query says', () => {
    expect(connectStateFromForm({ value: '' }, '?token=evil')).toEqual({ error: 'empty-token' });
  });
});
