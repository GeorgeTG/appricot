/**
 * Shows an untrusted string as plain text in `target`, replacing what was there.
 *
 * Every string the server sends (window titles, app ids, error messages) is hostile input
 * (docs/adr/0003-untrusted-server-client.md §1). This is how the client puts one into the
 * DOM: it sets `textContent`, which the browser never parses as markup. The lint rules in
 * eslint.config.js forbid the markup sinks, so there is no second way.
 */
export function setTextOnly(target: Node, text: string): void {
  target.textContent = text;
}
