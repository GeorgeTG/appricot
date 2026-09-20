/**
 * Where the demo's stream token comes from: the toolbar's input field, and nowhere else.
 *
 * The token is a per-session credential (ADR-0003, protocol rule 8: it never travels in a
 * URL). This module's ONLY input is the form field's value. The page's URL is deliberately
 * not a parameter to anything here, and a test greps the demo's sources to keep that true:
 * no query-string reader, no location reads, no cookies.
 */

/** The connect arguments the page builds from the toolbar. */
export interface ConnectToken {
  /** The token bytes Hello carries; opaque, exactly what the input field held. */
  token: Uint8Array;
}

export type TokenFromForm = ConnectToken | { error: 'empty-token' };

/**
 * Reads the token from an input-like field. The value is NOT trimmed or normalized — it is
 * opaque bytes, so the user's paste is encoded exactly as it arrived (TextEncoder, the same
 * encoding the wire wants). An empty field is an error, never a silent empty token.
 */
export function tokenFromForm(field: { value: string }): TokenFromForm {
  if (field.value.length === 0) {
    return { error: 'empty-token' };
  }
  return { token: new TextEncoder().encode(field.value) };
}

/**
 * Builds the page state that connects. The `pageSearch` parameter exists so a test can prove,
 * by construction, that a URL query such as `?token=evil` changes nothing: it is accepted and
 * deliberately ignored — the token comes from the form alone.
 */
export function connectStateFromForm(
  field: { value: string },
  pageSearch: string,
): TokenFromForm {
  void pageSearch; // read by nothing on purpose; the form field is the token's only source
  return tokenFromForm(field);
}
