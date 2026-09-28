/*
 * A focused input probe (dev-only): the same driver reduced to the input criterion, so its
 * evidence fits in one file. Expects the stand-in app to be running on the demo's display.
 *
 *   APPRICOT_DEMO_TOKEN=<fresh token> node input-probe.mjs
 *
 * It types with the same key events the driver sends, holds each key 400 ms (a keysym the
 * keymap cannot reach is delivered by rebinding a spare keycode, restored on release, so the
 * observer reads the rebound keysym only while the key is down), then idles so the host can
 * read the stand-in app's own key log out of the container.
 */
import { chromium } from 'playwright';

const BASE = process.env['APPRICOT_BASE'] ?? 'http://127.0.0.1:8390/';
const TOKEN = process.env['APPRICOT_DEMO_TOKEN'] ?? '';
const KINDS = { 1: 'hello', 2: 'hello_reply', 12: 'frame', 13: 'frame_ack', 16: 'pointer_move', 19: 'key', 20: 'focus_notify', 22: 'clipboard_set' };

const browser = await chromium.launch({ chromiumSandbox: false });
const context = await browser.newContext({ viewport: { width: 1280, height: 800 } });
const page = await context.newPage();
await page.addInitScript((kinds) => {
  const wire = { sent: [] };
  window.__wire = wire;
  const proto = window.WebSocket.prototype;
  const originalSend = proto.send;
  proto.send = function send(data) {
    const bytes = data instanceof ArrayBuffer ? new Uint8Array(data) : (ArrayBuffer.isView(data) ? new Uint8Array(data.buffer, data.byteOffset, data.byteLength) : null);
    if (bytes !== null) {
      let value = 0; let shift = 0;
      for (let i = 0; i < 4 && i < bytes.length; i += 1) { const b = bytes[i]; value |= (b & 0x7f) << shift; if ((b & 0x80) === 0) break; shift += 7; }
      wire.sent.push(kinds[value >> 3] ?? `field-${value >> 3}`);
    }
    return originalSend.call(this, data);
  };
}, KINDS);

await page.goto(BASE, { waitUntil: 'domcontentloaded' });
await page.fill('#token', TOKEN);
await page.click('#connect');
await page.waitForFunction(() => document.querySelector('#status')?.textContent === 'open', null, { timeout: 15000 });
await page.waitForTimeout(1500);

const target = await page.evaluate(() => {
  const w = [...document.querySelectorAll('#windows .window')].find((el) => el.querySelector('.title')?.textContent === 'Fake app: table');
  return w === undefined ? null : w.dataset.surfaceId;
});
if (target === null) { throw new Error('the stand-in app window is not in the session'); }
await page.locator(`#windows .window[data-surface-id="${target}"] .titlebar`).click({ position: { x: 60, y: 12 } });
await page.waitForTimeout(400);

const cdp = await context.newCDPSession(page);
const send = async (spec) => {
  await cdp.send('Input.dispatchKeyEvent', { type: 'keyDown', ...spec });
  await page.waitForTimeout(400);
  await cdp.send('Input.dispatchKeyEvent', { type: 'keyUp', ...spec, text: undefined, unmodifiedText: undefined });
  await page.waitForTimeout(150);
};

// US: a, A (Shift), 1, Enter, ArrowLeft. Greek: α on the physical KeyA. AltGr: € with Ctrl+Alt.
// Dead key: the dead key itself, then the composed character ά.
await send({ key: 'a', code: 'KeyA', text: 'a', unmodifiedText: 'a', windowsVirtualKeyCode: 65 });
await send({ key: 'A', code: 'KeyA', text: 'A', unmodifiedText: 'A', modifiers: 8, windowsVirtualKeyCode: 65 });
await send({ key: '1', code: 'Digit1', text: '1', unmodifiedText: '1', windowsVirtualKeyCode: 49 });
await send({ key: 'Enter', code: 'Enter', windowsVirtualKeyCode: 13 });
await send({ key: 'ArrowLeft', code: 'ArrowLeft', windowsVirtualKeyCode: 37 });
await send({ key: 'α', code: 'KeyA', text: 'α', unmodifiedText: 'α', windowsVirtualKeyCode: 65 });
await send({ key: '€', code: 'KeyE', text: '€', unmodifiedText: '€', modifiers: 3, windowsVirtualKeyCode: 69 });
await send({ key: 'Dead', code: 'Semicolon', windowsVirtualKeyCode: 186 });
await send({ key: 'ά', code: 'KeyA', text: 'ά', unmodifiedText: 'ά', windowsVirtualKeyCode: 65 });

const sent = (await page.evaluate(() => window.__wire.sent)).filter((k) => k === 'key');
console.log(`key envelopes sent by the client: ${sent.length}`);
console.log('idling 120 s so the host can read the stand-in app\'s key log out of the container');
await page.waitForTimeout(120000);
await browser.close();
