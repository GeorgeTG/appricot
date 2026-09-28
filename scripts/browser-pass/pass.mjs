/*
 * The M2 browser pass driver (dev-only: `just check` does not run it, and it ships nowhere).
 *
 * Playwright's container image, with the version that matches the browsers inside it:
 * https://playwright.dev/docs/docker (checked 2026-09-28).
 *
 * One run = one browser = one session, because the streamer serves one session per process
 * (docs/protocol/v0.md §7): when the browser closes, the streamer exits its resume grace and
 * the demo container stops. So this drives everything in one pass.
 *
 * The browser runs in a container that shares the demo container's network namespace, so the
 * page is reached at http://127.0.0.1:8390 with a loopback Host and Origin — the two things
 * the demo's session endpoint requires (packages/demo/src/server.ts:83, :364).
 *
 * Hand-off points print `=== HANDOFF: ... ===` and wait for the host to do something.
 *
 * Usage: APPRICOT_DEMO_TOKEN=<fresh token> BROWSER=chromium node driver.mjs
 */
import { chromium, firefox } from 'playwright';
import { writeFileSync, mkdirSync, existsSync, readFileSync, readdirSync, unlinkSync } from 'node:fs';

const BASE = process.env['APPRICOT_BASE'] ?? 'http://127.0.0.1:8390/';
const TOKEN = process.env['APPRICOT_DEMO_TOKEN'] ?? '';
// Fixed, never from the environment: a Git Bash host mangles `-e OUT=/pass/out` into a
// Windows path (MSYS path conversion), and the driver then writes into the container's own
// filesystem instead of the bind mount.
const OUT = '/pass/out';
const WHICH = process.env['BROWSER'] ?? 'chromium';
const STEPS = `${OUT}/steps`;

mkdirSync(STEPS, { recursive: true });
// A marker left by an earlier run must never be read as this run's hand-off: a stale
// host-to-app-read.txt made one pass report a clipboard read it had not taken.
for (const name of readdirSync(STEPS)) {
  unlinkSync(`${STEPS}/${name}`);
}

const records = [];
const consoleLog = [];
const pageErrors = [];
const timeline = [];
let stepNo = 0;

const iso = () => new Date().toISOString();
const say = (line) => {
  console.log(`${iso()} ${line}`);
  timeline.push(`${iso()} ${line}`);
};

function record(criterion, exercised, status, evidence, detail) {
  stepNo += 1;
  const r = { n: stepNo, criterion, exercised, status, evidence, detail };
  records.push(r);
  console.log(`[${status.toUpperCase()}] ${criterion} :: ${exercised}\n    evidence: ${evidence}` +
    (detail ? `\n    detail: ${detail}` : ''));
}

const shot = async (page, name) => {
  const path = `${OUT}/${WHICH}-${name}.png`;
  await page.screenshot({ path });
  say(`screenshot ${path}`);
  return path;
};

/** Waits for a file the host writes into the bind-mounted steps directory. A file that exists
 *  but is empty counts as not yet written: the host writes it with `mv` into place, and this
 *  is the belt to that pair of braces. */
async function waitForFile(name, timeoutMs) {
  const path = `${STEPS}/${name}`;
  const start = Date.now();
  while (Date.now() - start < timeoutMs) {
    if (existsSync(path)) {
      const text = readFileSync(path, 'utf8');
      if (text.length > 0) {
        return text;
      }
    }
    await new Promise((r) => setTimeout(r, 500));
  }
  return null;
}

/** Kinds by envelope field number, from crates/appricot-proto/proto/appricot/v0/wire.proto. */
const KINDS = {
  1: 'hello', 2: 'hello_reply', 3: 'bye', 4: 'server_error', 5: 'surface_new',
  6: 'surface_gone', 7: 'surface_metadata', 8: 'focus_ask', 9: 'resize_ask',
  10: 'configure', 11: 'configure_ack', 12: 'frame', 13: 'frame_ack', 14: 'cursor_image',
  15: 'cursor_gone', 16: 'pointer_move', 17: 'pointer_button', 18: 'pointer_axis',
  19: 'key', 20: 'focus_notify', 21: 'blur_release', 22: 'clipboard_set',
  23: 'clipboard_ask', 24: 'close_request', 25: 'clipboard_text',
};

const browserType = { chromium, firefox }[WHICH];
const browser = await browserType.launch(WHICH === 'chromium' ? { chromiumSandbox: false } : {});
const version = browser.version();
const context = await browser.newContext({
  viewport: { width: 1280, height: 800 },
  permissions: WHICH === 'chromium' ? ['clipboard-read', 'clipboard-write'] : [],
});
const page = await context.newPage();
page.on('console', (msg) => consoleLog.push({ t: iso(), type: msg.type(), text: msg.text() }));
page.on('pageerror', (err) => pageErrors.push({ t: iso(), text: String(err) }));

await page.addInitScript((kinds) => {
  // Observe the wire without changing it.
  const wire = { sent: [], received: [] };
  window.__wire = wire;
  const kindOf = (bytes) => {
    if (bytes.length === 0) return 'empty';
    let value = 0;
    let shift = 0;
    for (let i = 0; i < bytes.length && i < 4; i += 1) {
      const b = bytes[i];
      value |= (b & 0x7f) << shift;
      if ((b & 0x80) === 0) break;
      shift += 7;
    }
    return kinds[value >> 3] ?? `field-${value >> 3}`;
  };
  const decode = (data) => {
    if (data instanceof ArrayBuffer) return new Uint8Array(data);
    if (ArrayBuffer.isView(data)) return new Uint8Array(data.buffer, data.byteOffset, data.byteLength);
    return null;
  };
  const proto = window.WebSocket.prototype;
  const originalSend = proto.send;
  proto.send = function send(data) {
    const bytes = decode(data);
    wire.sent.push({ t: Math.round(performance.now()), kind: bytes ? kindOf(bytes) : typeof data, bytes: bytes ? bytes.length : 0 });
    return originalSend.call(this, data);
  };
  const originalAdd = proto.addEventListener;
  proto.addEventListener = function addEventListener(type, listener, options) {
    if (type !== 'message') {
      return originalAdd.call(this, type, listener, options);
    }
    const wrapped = (event) => {
      const bytes = decode(event.data);
      wire.received.push({ t: Math.round(performance.now()), kind: bytes ? kindOf(bytes) : typeof event.data, bytes: bytes ? bytes.length : 0 });
      return listener.call(this, event);
    };
    return originalAdd.call(this, type, wrapped, options);
  };
  // Observe every key the page is given, capture phase, without touching it.
  window.__keys = [];
  const note = (e) => window.__keys.push({
    t: Math.round(performance.now()), type: e.type, key: e.key, code: e.code,
    shift: e.shiftKey, ctrl: e.ctrlKey, alt: e.altKey, meta: e.metaKey,
    altGraph: e.getModifierState('AltGraph'), repeat: e.repeat, isTrusted: e.isTrusted,
  });
  document.addEventListener('keydown', note, true);
  document.addEventListener('keyup', note, true);
}, KINDS);

const snapshot = () => page.evaluate(() => ({
  status: document.querySelector('#status').textContent,
  sessionId: document.querySelector('#session-id').textContent,
  windowCount: document.querySelector('#windows-count').textContent,
  copyButton: { disabled: document.querySelector('#copy-app-text').disabled, text: document.querySelector('#copy-app-text').textContent },
  pasteToggle: document.querySelector('#paste').checked,
  minimized: [...document.querySelectorAll('#minimized .restore')].map((b) => b.textContent),
  activeElement: document.activeElement?.className ?? document.activeElement?.tagName,
  windows: [...document.querySelectorAll('#windows .window')].map((w) => {
    const box = w.getBoundingClientRect();
    const title = w.querySelector('.title');
    const canvas = w.querySelector('canvas.surface');
    const rect = canvas.getBoundingClientRect();
    const overlay = w.querySelector('canvas.cursor-overlay');
    return {
      surfaceId: w.dataset.surfaceId,
      app: w.querySelector('.app')?.textContent,
      title: title?.textContent,
      titleChildElements: title ? title.children.length : null,
      className: w.className,
      left: Math.round(box.left), top: Math.round(box.top),
      width: Math.round(box.width), height: Math.round(box.height),
      canvas: { width: canvas.width, height: canvas.height, cssWidth: Math.round(rect.width), cssHeight: Math.round(rect.height) },
      cursorOverlay: { width: overlay.width, height: overlay.height, left: overlay.style.left, top: overlay.style.top, visible: w.classList.contains('has-cursor') },
      popups: [...w.querySelectorAll('.popup')].map((p) => {
        const pb = p.getBoundingClientRect();
        const parentContent = w.querySelector('.content').getBoundingClientRect();
        return {
          left: Math.round(pb.left), top: Math.round(pb.top), width: Math.round(pb.width), height: Math.round(pb.height),
          parentContent: { left: Math.round(parentContent.left), top: Math.round(parentContent.top), width: Math.round(parentContent.width), height: Math.round(parentContent.height) },
          clipsAtParent: p.parentElement ? getComputedStyle(p.parentElement).overflow : null,
        };
      }),
    };
  }),
}));

const wire = () => page.evaluate(() => ({ sent: window.__wire.sent, received: window.__wire.received }));
const keys = () => page.evaluate(() => window.__keys);

const windowsByTitle = (snap) => Object.fromEntries(snap.windows.map((w) => [w.title, w]));

// --- 1. load and CSP ---------------------------------------------------------------------------
try {
say(`BROWSER ${WHICH} ${version}`);
const response = await page.goto(BASE, { waitUntil: 'domcontentloaded' });
const csp = response.headers()['content-security-policy'] ?? '(none)';
record('CSP', `GET ${BASE}; read the response headers`, 'run',
  `content-security-policy: ${csp}`, `HTTP ${response.status()}`);
await page.waitForSelector('#connect');
record('page', 'the demo page rendered its toolbar', 'run', `HTTP ${response.status()}, #connect present`);
await shot(page, '01-loaded');

// An inline script and an inline style, injected the way markup injection would arrive: the
// policy must refuse both, and refuse loudly. `require-trusted-types-for 'script'` and
// `trusted-types 'none'` mean the sink itself throws before CSP's script-src is even reached.
const cspProbe = await page.evaluate(() => {
  const out = { scriptError: null, scriptRan: null, styleApplied: null, statusColour: null };
  try {
    const script = document.createElement('script');
    script.textContent = 'window.__inlineScriptRan = 1';
    document.head.append(script);
  } catch (err) {
    out.scriptError = String(err);
  }
  out.scriptRan = window.__inlineScriptRan === 1;
  const style = document.createElement('style');
  style.textContent = '#status { color: rgb(255, 0, 0) }';
  document.head.append(style);
  out.statusColour = getComputedStyle(document.querySelector('#status')).color;
  out.styleApplied = out.statusColour === 'rgb(255, 0, 0)';
  return out;
});
await page.waitForTimeout(400);
record('CSP refuses inline script and style', 'injected a <script> and a <style> the way a markup injection would', 'run',
  `script.textContent threw ${JSON.stringify(cspProbe.scriptError)}; inline script ran = ${cspProbe.scriptRan}; ` +
  `injected <style> applied = ${cspProbe.styleApplied} (#status colour = ${cspProbe.statusColour})`,
  'the CSP violations are in the console log');

// --- 2. token auth, negative -------------------------------------------------------------------
await page.fill('#token', 'definitely-not-the-token');
await page.click('#connect');
await page.waitForTimeout(1500);
const wrong = await snapshot();
record('token auth: a wrong token is refused', 'typed a wrong token and clicked Connect', 'run',
  `#status = ${JSON.stringify(wrong.status)}, #session-id = ${JSON.stringify(wrong.sessionId)}`);
await shot(page, '02-wrong-token');
await page.waitForTimeout(1000);

// --- 3. token auth, positive; the session and its windows --------------------------------------
await page.fill('#token', TOKEN);
await page.click('#connect');
await page.waitForFunction(() => document.querySelector('#status')?.textContent === 'open', null, { timeout: 15000 });
await page.waitForTimeout(2000);
const opened = await snapshot();
record('token auth: the token from the recipe opens a session',
  'typed the token the recipe printed and clicked Connect', 'run',
  `#status = ${JSON.stringify(opened.status)}, #session-id = ${JSON.stringify(opened.sessionId)}, ` +
  `#windows-count = ${JSON.stringify(opened.windowCount)}`,
  JSON.stringify(opened.windows.map((w) => ({ id: w.surfaceId, title: w.title, canvas: w.canvas }))));
await shot(page, '03-open');

// --- 4. one host window per toplevel, titles as text -------------------------------------------
record('one host window per streamed toplevel',
  'counted the host windows against the X toplevels the observer saw', 'run',
  `${opened.windows.length} host window(s): ${opened.windows.map((w) => `${w.title} (${w.canvas.width}x${w.canvas.height})`).join(', ')}`,
  'the X side of this count is in the X observer log');
const titleCheck = opened.windows.map((w) => ({ title: w.title, childElements: w.titleChildElements, app: w.app }));
record('titles as text (no markup)', 'read each window title element and its child elements', 'run',
  JSON.stringify(titleCheck), 'a marked-up title would have child elements; these are text-only');

// --- 5. idle: nothing arrives while nothing changes --------------------------------------------
const idleStart = await wire();
await page.waitForTimeout(5000);
const idleEnd = await wire();
record('an idle window sends nothing', 'held the session open for 5 s with no pointer or key input', 'run',
  `received ${idleEnd.received.length - idleStart.received.length} message(s) in 5 s; ` +
  `sent ${idleEnd.sent.length - idleStart.sent.length}`);
record('ack-based flow control', 'counted the acks the client sent for the frames it drew', 'run',
  `client sent: ${JSON.stringify(idleEnd.sent.map((m) => m.kind))}`,
  `streamer sent: ${JSON.stringify(idleEnd.received.map((m) => m.kind))}`);

// --- 6. focus, cursor, drag, resize, minimise, restore -----------------------------------------
const byTitle = windowsByTitle(opened);
const logo = byTitle['XLogo'];
const probe = byTitle['XInputProbe'];
if (logo === undefined || probe === undefined) {
  record('window chrome interactions', 'looked for the two expected windows', 'failed',
    `titles seen: ${JSON.stringify(Object.keys(byTitle))}`);
} else {
  const logoRoot = page.locator(`#windows .window[data-surface-id="${logo.surfaceId}"]`);

  // focus: click the XLogo title bar
  await logoRoot.locator('.titlebar').click({ position: { x: 60, y: 12 } });
  await page.waitForTimeout(600);
  const focused = await snapshot();
  record('focus follows a click', 'clicked the XLogo title bar', 'run',
    `class = ${JSON.stringify(focused.windows.find((w) => w.title === 'XLogo')?.className)}; ` +
    `document.activeElement = ${JSON.stringify(focused.activeElement)}`,
    'the X-side input focus is in the observer log');

  // cursor: move the pointer over the window. Whether an image arrives is the next record's
  // business (the demo's display has to change its cursor for the streamer to send one).
  const box = await logoRoot.boundingBox();
  await page.mouse.move(box.x + 30, box.y + 90);
  await page.mouse.move(box.x + 40, box.y + 100, { steps: 5 });
  await page.waitForTimeout(600);
  await shot(page, '04-pointer');

  // drag: title bar, +140 x, +110 y
  const beforeDrag = await logoRoot.boundingBox();
  await page.mouse.move(beforeDrag.x + 60, beforeDrag.y + 12);
  await page.mouse.down();
  await page.mouse.move(beforeDrag.x + 60 + 140, beforeDrag.y + 12 + 110, { steps: 12 });
  await page.mouse.up();
  await page.waitForTimeout(800);
  const afterDrag = await logoRoot.boundingBox();
  record('drag moves the window', 'dragged the XLogo title bar by +140 x, +110 y', 'run',
    `host box ${JSON.stringify({ x: Math.round(beforeDrag.x), y: Math.round(beforeDrag.y) })} -> ` +
    `${JSON.stringify({ x: Math.round(afterDrag.x), y: Math.round(afterDrag.y) })}`,
    'the X-side move is in the observer log at the same timestamp');
  await shot(page, '05-dragged');

  // resize: the grip at the bottom-right corner
  const grip = logoRoot.locator('.resize-grip');
  const gripBox = await grip.boundingBox();
  const beforeResize = await snapshot();
  await page.mouse.move(gripBox.x + gripBox.width / 2, gripBox.y + gripBox.height / 2);
  await page.mouse.down();
  await page.mouse.move(gripBox.x + gripBox.width / 2 + 90, gripBox.y + gripBox.height / 2 + 70, { steps: 12 });
  await page.mouse.up();
  await page.waitForTimeout(1200);
  const afterResize = await snapshot();
  record('resize', 'dragged the resize grip by +90 x, +70 y and released', 'run',
    `XLogo canvas ${JSON.stringify(beforeResize.windows.find((w) => w.title === 'XLogo')?.canvas)} -> ` +
    `${JSON.stringify(afterResize.windows.find((w) => w.title === 'XLogo')?.canvas)}`,
    'the X-side size is in the observer log at the same timestamp');
  await shot(page, '06-resized');

  // minimise and restore
  await logoRoot.locator('.minimize').click();
  await page.waitForTimeout(500);
  const minimized = await snapshot();
  record('minimise', 'clicked the XLogo minimise button', 'run',
    `XLogo class = ${JSON.stringify(minimized.windows.find((w) => w.title === 'XLogo')?.className)}; ` +
    `restore strip = ${JSON.stringify(minimized.minimized)}`);
  await shot(page, '07-minimized');
  if (minimized.minimized.length > 0) {
    await page.locator('#minimized .restore').first().click();
    await page.waitForTimeout(600);
    const restored = await snapshot();
    record('restore', 'clicked the XLogo restore button in the toolbar strip', 'run',
      `XLogo class = ${JSON.stringify(restored.windows.find((w) => w.title === 'XLogo')?.className)}; restore strip = ${JSON.stringify(restored.minimized)}`);
  }
}

// --- 11. close -----------------------------------------------------------------------------
// Before the stand-in app is started: that one maps a window which never lists
// WM_DELETE_WINDOW, and a window under another is a click the host cannot make. The XLogo and
// XInputProbe clients both handle the close protocol.
{
  const beforeClose = await snapshot();
  const closable = beforeClose.windows.find((w) => ['XLogo', 'XInputProbe'].includes(w.title) && w.className.includes('focused'))
    ?? beforeClose.windows.find((w) => w.title === 'XLogo' || w.title === 'XInputProbe');
  if (closable !== undefined) {
    const root = page.locator(`#windows .window[data-surface-id="${closable.surfaceId}"]`);
    await root.locator('.titlebar').click({ position: { x: 60, y: 12 }, timeout: 5000 }).catch(() => {});
    await page.waitForTimeout(300);
    await root.locator('.close').click({ timeout: 5000 }).catch((err) => say(`close click: ${String(err).split(String.fromCharCode(10))[0]}`));
    await page.waitForTimeout(2500);
    const afterClose = await snapshot();
    record('close', `clicked the close button of ${closable.title} (a CloseRequest)`,
      afterClose.windows.length < beforeClose.windows.length ? 'run' : 'failed',
      `windows: ${beforeClose.windows.map((w) => w.title).join(', ')} -> ${afterClose.windows.map((w) => w.title).join(', ')}; ` +
      `#windows-count = ${JSON.stringify(afterClose.windowCount)}`,
      'the X client exits on WM_DELETE_WINDOW; the X observer log says whether it did');
    await shot(page, '12-closed');
  } else {
    record('close', 'looked for a closable window', 'not run', 'neither X client window was in the session');
  }
}

// --- 7. popups (hand-off: start the stand-in app) ----------------------------------------------
say('=== HANDOFF: start the stand-in app now (popup phase) ===');
let popupSeen = [];
try {
  await page.waitForSelector('#windows .popup', { timeout: 90000 });
  const measure = async (label) => {
    const during = await snapshot();
    for (const w of during.windows) {
      for (const p of w.popups) {
        popupSeen.push({ at: label, parentTitle: w.title, parentContent: p.parentContent, popup: { left: p.left, top: p.top, width: p.width, height: p.height }, clipsAtParent: p.clipsAtParent });
      }
    }
  };
  await measure('the first popup mapped');
  await shot(page, '08-popup');
  // The host moves the popup window itself out to a corner that is outside its parent's box,
  // so the clamp has something to hold.
  await page.waitForTimeout(300);
  await measure('after the host moved the popup window');
  await shot(page, '08a-popup-moved');
  // The stand-in app maps an override-redirect popup for one second and a transient dialog
  // later; the dialog is the one that stays, so measure again after its moment.
  await page.waitForTimeout(3200);
  await measure('3.5 s later (the transient dialog)');
  await shot(page, '08b-popup-dialog');
  record('popups placed and clamped to their parent',
    'measured every popup box against its parent content box, twice', 'run',
    JSON.stringify(popupSeen), 'margin: the layer reaches 32 px past the content box and clips there');
} catch {
  record('popups placed and clamped to their parent',
    'watched for a popup surface for 90 s', 'not run',
    'no .popup element appeared; the stand-in app was not started (or its popup was unmapped before it could be measured)');
}

// --- 8. input ----------------------------------------------------------------------------------
// The observer for what the app received is the stand-in app itself: its key reports recompute
// each keysym from the server's keymap, refetched on every MappingNotify, so a keycode the
// streamer rebinds for one press reads as the rebound keysym — the same reading the backend's
// own tests take. A second observer (the X client started as XInputProbe) is read too.
say('=== HANDOFF: start the stand-in app now (input observer; the popup is the NEXT hand-off) ===');
const standIn = await waitForFile('stand-in-started.txt', 90000);
const probeWindow = (await snapshot()).windows.find((w) => w.title === 'Fake app: table')
  ?? (await snapshot()).windows.find((w) => w.title === 'XInputProbe');
let cdp = null;
if (WHICH === 'chromium') {
  cdp = await context.newCDPSession(page);
}

const typeKeys = async (label, events) => {
  const before = (await wire()).sent.length;
  let refused = 0;
  if (WHICH === 'chromium') {
    for (const e of events) {
      await cdp.send('Input.dispatchKeyEvent', { type: 'keyDown', ...e.down });
      // Held this long on purpose: the streamer rebinds a spare keycode for a keysym the map
      // cannot reach and restores it on release, so an observer reads the rebound keysym only
      // while the key is still down.
      await page.waitForTimeout(400);
      await cdp.send('Input.dispatchKeyEvent', { type: 'keyUp', ...e.up });
      await page.waitForTimeout(120);
    }
  } else {
    for (const e of events) {
      const chord = e.chord ?? (e.playwright !== undefined ? [e.playwright] : (e.char !== undefined ? [e.char] : undefined));
      if (chord === undefined) { continue; }
      try {
        for (const name of chord) { await page.keyboard.down(name); }
        await page.waitForTimeout(400);
        for (const name of [...chord].reverse()) { await page.keyboard.up(name); }
        await page.waitForTimeout(120);
      } catch (err) {
        refused += 1;
        say(`the keyboard refused ${JSON.stringify(chord)}: ${String(err).split(String.fromCharCode(10))[0]}`);
      }
    }
  }
  await page.waitForTimeout(400);
  const after = await wire();
  const sentKeys = after.sent.slice(before).filter((m) => m.kind === 'key');
  return { envelopes: sentKeys.length, refused };
};

if (probeWindow !== undefined) {
  // Focus the XInputProbe window so the keys go to it.
  await page.locator(`#windows .window[data-surface-id="${probeWindow.surfaceId}"] .titlebar`).click({ position: { x: 60, y: 12 } });
  await page.waitForTimeout(500);

  const us = await typeKeys('US', WHICH === 'chromium' ? [
    { down: { key: 'a', code: 'KeyA', text: 'a', unmodifiedText: 'a', windowsVirtualKeyCode: 65 }, up: { key: 'a', code: 'KeyA', windowsVirtualKeyCode: 65 } },
    { down: { key: 'A', code: 'KeyA', text: 'A', unmodifiedText: 'A', modifiers: 8, windowsVirtualKeyCode: 65 }, up: { key: 'A', code: 'KeyA', modifiers: 8, windowsVirtualKeyCode: 65 } },
    { down: { key: '1', code: 'Digit1', text: '1', unmodifiedText: '1', windowsVirtualKeyCode: 49 }, up: { key: '1', code: 'Digit1', windowsVirtualKeyCode: 49 } },
    { down: { key: 'Enter', code: 'Enter', windowsVirtualKeyCode: 13 }, up: { key: 'Enter', code: 'Enter', windowsVirtualKeyCode: 13 } },
    { down: { key: 'ArrowLeft', code: 'ArrowLeft', windowsVirtualKeyCode: 37 }, up: { key: 'ArrowLeft', code: 'ArrowLeft', windowsVirtualKeyCode: 37 } },
  ] : [
    { playwright: 'a' }, { chord: ['Shift', 'a'] }, { playwright: '1' }, { playwright: 'Enter' }, { playwright: 'ArrowLeft' },
  ]);
  record('input, US layout', `typed a, A, 1, Enter, ArrowLeft into ${probeWindow.title} (${WHICH})`, 'run',
    `${us.envelopes} key envelopes on the wire; the X side is in the record below`);

  const greek = await typeKeys('Greek', WHICH === 'chromium' ? [
    { down: { key: 'α', code: 'KeyA', text: 'α', unmodifiedText: 'α', windowsVirtualKeyCode: 65 }, up: { key: 'α', code: 'KeyA', windowsVirtualKeyCode: 65 } },
  ] : [
    { char: 'α' },
  ]);
  record('input, Greek layout', `sent the character α with the physical KeyA code (${WHICH})`, greek.envelopes > 0 ? 'run' : 'failed',
    `${greek.envelopes} key envelope(s) (${greek.refused} refused by the driver) on the wire; the X side is in the record below`);

  const altgr = await typeKeys('AltGr', WHICH === 'chromium' ? [
    { down: { key: '€', code: 'KeyE', text: '€', unmodifiedText: '€', modifiers: 3, windowsVirtualKeyCode: 69 }, up: { key: '€', code: 'KeyE', modifiers: 3, windowsVirtualKeyCode: 69 } },
  ] : [
    { char: '€' },
  ]);
  record('input, AltGr', `sent the AltGr character € with Ctrl+Alt held (${WHICH})`, altgr.envelopes > 0 ? 'run' : 'failed',
    `${altgr.envelopes} key envelope(s) (${altgr.refused} refused by the driver) on the wire; the X side is in the record below`);

  const dead = await typeKeys('dead key', WHICH === 'chromium' ? [
    { down: { key: 'Dead', code: 'Semicolon', windowsVirtualKeyCode: 186 }, up: { key: 'Dead', code: 'Semicolon', windowsVirtualKeyCode: 186 } },
    { down: { key: 'ά', code: 'KeyA', text: 'ά', unmodifiedText: 'ά', windowsVirtualKeyCode: 65 }, up: { key: 'ά', code: 'KeyA', windowsVirtualKeyCode: 65 } },
  ] : [
    { char: 'ά' },
  ]);
  const keyLog = await keys();
  record('input, dead keys', `sent the dead-key keydown then the composed character ά (${WHICH})`, 'run',
    `${dead.envelopes} key envelope(s) for two keydowns (${dead.refused} refused by the driver) (the dead key itself carries no keysym and is not sent); ` +
    `the X-side keysym is in the record below`,
    `page key log: ${JSON.stringify(keyLog.slice(-6))}`);
  await shot(page, '09-input');

  say('=== HANDOFF: read the X side of the input (stand-in app and X client key logs) into steps/xev-read.txt ===');
  const xev = await waitForFile('xev-read.txt', 90000);
  record('input, what the X client received', 'read the X client\'s own key log after the typing', xev === null ? 'not run' : 'run',
    xev === null ? 'the host did not provide steps/xev-read.txt' : xev.replace(/\n/g, ' | ').slice(0, 1200),
    'xev recomputes each keysym from the server keymap, so this is what the app was actually given');
} else {
  record('input', 'looked for the XInputProbe window', 'not run', 'the window was not in the session inventory');
}

// --- 9. clipboard, app -> host -----------------------------------------------------------------
say('=== HANDOFF: set the X clipboard from another X client now (xclip -selection clipboard) ===');
let copyReady = false;
try {
  await page.waitForFunction(() => document.querySelector('#copy-app-text')?.disabled === false, null, { timeout: 90000 });
  copyReady = true;
} catch {
  copyReady = false;
}
if (copyReady) {
  await shot(page, '10-app-copy-held');
  // A sentinel goes on the user's clipboard first, so the write can be attributed to the click.
  await page.evaluate(() => navigator.clipboard.writeText('sentinel-before-the-click'));
  const beforeClick = await page.evaluate(() => navigator.clipboard.readText());
  await page.locator('#copy-app-text').click();
  await page.waitForTimeout(700);
  const afterClick = await page.evaluate(() => navigator.clipboard.readText());
  const held = await page.evaluate(() => document.querySelector('#status').textContent);
  record('clipboard, app -> host, written only inside a user gesture',
    'let another X client take the CLIPBOARD selection, then clicked "Copy app text"', 'run',
    `clipboard before the click = ${JSON.stringify(beforeClick)}; after the click = ${JSON.stringify(afterClick)}; #status = ${JSON.stringify(held)}`,
    'the button was enabled by the clipboard_text envelope; nothing was written until the click');
  await shot(page, '11-app-copy-written');
} else {
  record('clipboard, app -> host', 'waited 90 s for the "Copy app text" button to enable', 'not run',
    'the app never took the CLIPBOARD selection (no X client with text ran, or the streamer did not fetch it)');
}

// --- 10. clipboard, host -> app -----------------------------------------------------------------
if (probeWindow !== undefined) {
  await page.locator('#paste').check();
  await page.locator(`#windows .window[data-surface-id="${probeWindow.surfaceId}"] .titlebar`).click({ position: { x: 60, y: 12 } });
  await page.waitForTimeout(300);
  await page.evaluate(() => navigator.clipboard.writeText('host-side paste text'));
  const sentBefore = (await wire()).sent.length;
  await page.keyboard.press('Control+v');
  await page.waitForTimeout(800);
  const sentAfter = await wire();
  const sentKinds = sentAfter.sent.slice(sentBefore).map((m) => m.kind);
  record('clipboard, host -> app', 'checked the paste toggle, put text on the host clipboard and pressed Ctrl+V', 'run',
    `wire kinds after the paste: ${JSON.stringify(sentKinds)}`,
    'the client sends clipboard_set only inside the paste event');
  say('=== HANDOFF: read the X clipboard now (xclip -selection clipboard -o) and write the text to steps/host-to-app-read.txt ===');
  const read = await waitForFile('host-to-app-read.txt', 90000);
  record('clipboard, host -> app, observed on the X side', 'read the CLIPBOARD selection from another X client after the paste', read === null ? 'not run' : 'run',
    read === null ? 'the host did not provide steps/host-to-app-read.txt' : `xclip read back: ${JSON.stringify(read.trim())}`);
} else {
  record('clipboard, host -> app', 'looked for the XInputProbe window', 'not run', 'the window was not in the session inventory');
}

// --- 13. the cursor, deliberately last ----------------------------------------------------------
// The change that makes the streamer send a cursor image has ended the session in this rig (see
// the record's defect section), so it runs here: a death on this step cannot take the rest of the
// pass with it.
say('=== HANDOFF: change the X cursor now (xsetroot -cursor_name crosshair, as another X client) ===');
const cursorMarker = await waitForFile('cursor-changed.txt', 90000);
if (cursorMarker !== null) {
  await page.waitForTimeout(900);
  let cursorSnap = null;
  try {
    const target = (await snapshot()).windows[0];
    if (target !== undefined) {
      const root = page.locator(`#windows .window[data-surface-id="${target.surfaceId}"]`);
      await root.locator('.titlebar').click({ position: { x: 60, y: 12 }, timeout: 5000 }).catch(() => {});
      const box = await root.boundingBox();
      if (box !== null) {
        await page.mouse.move(box.x + 40, box.y + 90, { steps: 4 });
      }
      await page.waitForTimeout(700);
    }
    cursorSnap = await snapshot();
  } catch (err) {
    say(`the cursor phase could not drive the page: ${String(err).split('\n')[0]}`);
  }
  const cursorWire = await wire();
  const drawn = (cursorSnap?.windows ?? []).filter((w) => w.cursorOverlay.visible)
    .map((w) => ({ title: w.title, overlay: w.cursorOverlay }));
  const cursorKinds = cursorWire.received.filter((m) => m.kind.startsWith('cursor')).map((m) => m.kind);
  record('the cursor', 'let another X client change the cursor while the session was open, then moved the pointer over a window',
    drawn.length > 0 ? 'run' : 'failed',
    `#status = ${JSON.stringify(cursorSnap?.status)}, cursor messages on the wire: ${JSON.stringify(cursorKinds)}, ` +
    `overlays drawn: ${JSON.stringify(drawn)}`,
    'the overlay is the streamed app\'s cursor image; if the session ended here, that is in this record');
  await shot(page, '14-cursor');
} else {
  record('the cursor', 'waited for the host to change the X cursor', 'not run',
    'the host did not provide steps/cursor-changed.txt');
}

// --- 14. wrap up --------------------------------------------------------------------------------
const final = await snapshot();
const wireEnd = await wire();
const keyLog = await keys();
record('console cleanliness', 'collected every console message and page error for the whole run', pageErrors.length === 0 ? 'run' : 'failed',
  `${consoleLog.length} console message(s), ${pageErrors.length} page error(s)`,
  consoleLog.length > 0 ? JSON.stringify(consoleLog) : 'none');
await shot(page, '13-final');
} catch (err) {
  record('the pass itself', 'ran the driver to the end', 'failed', String(err).split('\n')[0],
    'the phases before this point kept their records; the ones after it were not run');
  say(String(err));
} finally {
  const report = {
    browser: WHICH,
    version,
    userAgent: await page.evaluate(() => navigator.userAgent).catch(() => '(unknown)'),
    base: BASE,
    when: iso(),
    records,
    timeline,
    console: consoleLog,
    pageErrors,
    wire: await wire().catch(() => null),
    keyLog: await keys().catch(() => null),
  };
  writeFileSync(`${OUT}/${WHICH}-pass.json`, JSON.stringify(report, null, 2));
  console.log(`\n${WHICH} ${version}: ${records.filter((r) => r.status === 'run').length} run, ` +
    `${records.filter((r) => r.status === 'failed').length} failed, ` +
    `${records.filter((r) => r.status === 'not run').length} not run`);
  await context.close();
  await browser.close();
}
