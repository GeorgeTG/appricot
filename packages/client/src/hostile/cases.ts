/**
 * The hostile-server case factories: every shape a compromised streamer can send, as typed
 * Envelope values and as hand-encoded protobuf bytes (docs/adr/0003-untrusted-server-client.md,
 * roadmap.md M2: "A hostile test server ... cannot put markup into the page, draw outside the
 * parent-plus-margin box, or take focus. This is a CI test.").
 *
 * Two paths are covered on purpose:
 *   - The typed path: codec-legal envelopes (markup strings, screen-sized popups, focus asks,
 *     frame floods) that `decodeEnvelope` accepts. The defence against these is NOT the codec;
 *     it is the registry, `setTextOnly`, `clampPopup` and the renderer, and only CI proves them.
 *   - The byte path: lengths and shapes the codec must refuse. `encodeEnvelope` validates the
 *     limits table and throws, so these bytes are built by hand with the small protobuf writer
 *     below - exactly the style of protocol.vector.test.ts, attacking `decodeEnvelope` and the
 *     connection with what a hostile server would actually put on the wire.
 *
 * Nothing here touches the DOM or the SDK modules beyond the protocol types and limits: the
 * factories are pure data, and every judgement lives in the tests.
 */
import type {
  ByeReason,
  Envelope,
  Positioner,
  Rect,
  Tile,
} from '../protocol';
import { encodeEnvelope } from '../protocol';
import {
  MAX_APP_ID_BYTES,
  MAX_BYE_TEXT_BYTES,
  MAX_CLIPBOARD_BYTES,
  MAX_CURSOR_WIDTH,
  MAX_ERROR_TEXT_BYTES,
  MAX_MESSAGE_BYTES,
  MAX_SESSION_ID_BYTES,
  MAX_SURFACE_WIDTH,
  MAX_TILE_BYTES,
  MAX_TILE_WIDTH,
  MAX_TILES_PER_FRAME,
  MAX_TITLE_BYTES,
} from '../protocol';

/** The global the markup payloads try to set; the tests assert it stays undefined. */
export const PWNED_PROPERTY = '__pwned';

/**
 * A `javascript:` URL as hostile DATA. The no-script-url lint rule exists to keep such a
 * literal out of shipping code, and it stays untouched; the fixture assembles the string so
 * the rule never sees a script-URL literal while the corpus still carries the real shape.
 */
const SCRIPT_URL = 'java' + 'script:window.__pwned=1';

/**
 * Markup that executes or hijacks layout when a browser parses it as markup instead of text.
 * Every payload sets the same canary global, so one assertion covers the corpus. The strings
 * are deliberately short enough to fit the smallest server-string cap (session id, 64 bytes)
 * unless noted; `markupUnder` filters per field.
 */
export const MARKUP_STRINGS: readonly string[] = [
  '<img src=x onerror=window.__pwned=1>',
  '<script>window.__pwned=1</script>',
  '<svg onload=window.__pwned=1></svg>',
  '<iframe srcdoc="<script>window.__pwned=1</script>"></iframe>',
  SCRIPT_URL,
  `<a href="${SCRIPT_URL}">click</a>`,
  '"><img src=x onerror=window.__pwned=1>',
  '<div style="width:100vw;height:100vh">cover</div>',
  '&lt;img src=x onerror=window.__pwned=1&gt;',
  '<img src=x onerror=window.__pwned=1> Ελληνικά με δίγραφα και τόνους',
];

// ---------------------------------------------------------------------------
// Bytes: a minimal protobuf writer, for messages the SDK's own encoder refuses to emit
// ---------------------------------------------------------------------------

const utf8Encoder = new TextEncoder();

function utf8(value: string): Uint8Array {
  return utf8Encoder.encode(value);
}

/** The UTF-8 byte length of `value`: the unit every string cap is counted in. */
export function utf8Length(value: string): number {
  return utf8(value).length;
}

/** The markup corpus that fits one field's byte cap. */
export function markupUnder(byteCap: number): readonly string[] {
  return MARKUP_STRINGS.filter((s) => utf8Length(s) <= byteCap);
}

/** One uint32 varint; lengths and enum/scalar fields only, exactly what the hostile bodies need. */
function varint(value: number): Uint8Array {
  if (!Number.isSafeInteger(value) || value < 0 || value > 0xffffffff) {
    throw new RangeError(`varint accepts uint32 only, got ${value}`);
  }
  const out: number[] = [];
  let rest = value;
  while (rest > 0x7f) {
    out.push((rest % 128) | 0x80);
    rest = Math.floor(rest / 128);
  }
  out.push(rest);
  return Uint8Array.from(out);
}

/** sint32 zigzag, as the .proto carries Rect/Point coordinates. */
function zigzag32(value: number): number {
  return ((value << 1) ^ (value >> 31)) >>> 0;
}

function concat(parts: readonly Uint8Array[]): Uint8Array {
  let total = 0;
  for (const part of parts) {
    total += part.length;
  }
  const out = new Uint8Array(total);
  let at = 0;
  for (const part of parts) {
    out.set(part, at);
    at += part.length;
  }
  return out;
}

function tag(field: number, wireType: number): Uint8Array {
  return varint((field << 3) | wireType);
}

function varintField(field: number, value: number): Uint8Array {
  return concat([tag(field, 0), varint(value)]);
}

function zigzagField(field: number, value: number): Uint8Array {
  return concat([tag(field, 0), varint(zigzag32(value))]);
}

function bytesField(field: number, payload: Uint8Array): Uint8Array {
  return concat([tag(field, 2), varint(payload.length), payload]);
}

function stringField(field: number, value: string): Uint8Array {
  return bytesField(field, utf8(value));
}

/** A length-delimited sub-message: the shape of every envelope body and nested message. */
function messageField(field: number, body: Uint8Array): Uint8Array {
  return bytesField(field, body);
}

function rectBytes(rect: Rect): Uint8Array {
  return concat([
    zigzagField(1, rect.x),
    zigzagField(2, rect.y),
    varintField(3, rect.width),
    varintField(4, rect.height),
  ]);
}

/** One full wire message: the oneof body under its Envelope field number. */
function envelope(oneofField: number, body: Uint8Array): Uint8Array {
  return messageField(oneofField, body);
}

function fillBytes(count: number, byte: number): Uint8Array {
  const out = new Uint8Array(count);
  out.fill(byte);
  return out;
}

// Envelope oneof field numbers (wire.ts encodeEnvelope; wire.proto oneof body).
const F_HELLO_REPLY = 2;
const F_BYE = 3;
const F_SERVER_ERROR = 4;
const F_SURFACE_NEW = 5;
const F_SURFACE_METADATA = 7;
const F_FRAME = 12;
const F_CURSOR_IMAGE = 14;
const F_CLIPBOARD_SET = 22;

// ---------------------------------------------------------------------------
// The markup corpus as typed envelopes: codec-legal, hostile in the DOM
// ---------------------------------------------------------------------------

/** One hostile input with a name the CI report can point at. */
export interface HostileEnvelope {
  readonly name: string;
  readonly envelope: Envelope;
}

function shortLabel(value: string): string {
  return value.length <= 28 ? value : `${value.slice(0, 28)}...`;
}

/** A benign toplevel so metadata cases have a surface to aim at. */
export function benignSurfaceNew(surfaceId: number): Envelope {
  return {
    kind: 'surfaceNew',
    surfaceNew: {
      surfaceId,
      role: 0,
      size: { width: 64, height: 48 },
      title: 'benign',
      appId: 'benign.app',
      scale120ths: 120,
    },
  };
}

/**
 * Every server string field, each loaded with every markup string that fits its cap: the
 * "markup in every string" fixture of the M2 exit criterion. Surface ids are distinct so the
 * registry keeps them all; metadata cases aim at id `metadataTargetId`.
 */
export function markupEnvelopes(): readonly HostileEnvelope[] {
  const cases: HostileEnvelope[] = [];
  const metadataTargetId = 9000;

  for (const s of markupUnder(MAX_SESSION_ID_BYTES)) {
    cases.push({
      name: `HelloReply.session_id <- ${shortLabel(s)}`,
      envelope: {
        kind: 'helloReply',
        helloReply: {
          protocolVersion: 0,
          sessionId: s,
          maxFrameCredits: 4,
          codecs: [1],
          resumed: false,
        },
      },
    });
  }
  for (const s of markupUnder(MAX_BYE_TEXT_BYTES)) {
    cases.push({
      name: `Bye.text <- ${shortLabel(s)}`,
      envelope: { kind: 'bye', bye: { reason: 0x105, text: s } },
    });
  }
  for (const s of markupUnder(MAX_ERROR_TEXT_BYTES)) {
    cases.push({
      name: `ServerError.text <- ${shortLabel(s)}`,
      envelope: { kind: 'serverError', serverError: { code: 1, text: s } },
    });
  }
  let surfaceId = 1;
  for (const s of markupUnder(MAX_TITLE_BYTES)) {
    const id = surfaceId;
    surfaceId += 1;
    cases.push({
      name: `SurfaceNew.title <- ${shortLabel(s)}`,
      envelope: {
        kind: 'surfaceNew',
        surfaceNew: {
          surfaceId: id,
          role: 0,
          size: { width: 64, height: 48 },
          title: s,
          appId: 'hostile.app',
          scale120ths: 120,
        },
      },
    });
  }
  for (const s of markupUnder(MAX_APP_ID_BYTES)) {
    const id = surfaceId;
    surfaceId += 1;
    cases.push({
      name: `SurfaceNew.app_id <- ${shortLabel(s)}`,
      envelope: {
        kind: 'surfaceNew',
        surfaceNew: {
          surfaceId: id,
          role: 0,
          size: { width: 64, height: 48 },
          title: 'benign title',
          appId: s,
          scale120ths: 120,
        },
      },
    });
  }
  cases.push({
    name: 'the benign metadata target surface',
    envelope: benignSurfaceNew(metadataTargetId),
  });
  for (const s of markupUnder(MAX_TITLE_BYTES)) {
    cases.push({
      name: `SurfaceMetadata.title <- ${shortLabel(s)}`,
      envelope: {
        kind: 'surfaceMetadata',
        surfaceMetadata: { surfaceId: metadataTargetId, title: s },
      },
    });
  }
  for (const s of markupUnder(MAX_APP_ID_BYTES)) {
    cases.push({
      name: `SurfaceMetadata.app_id <- ${shortLabel(s)}`,
      envelope: {
        kind: 'surfaceMetadata',
        surfaceMetadata: { surfaceId: metadataTargetId, appId: s },
      },
    });
  }
  return cases;
}

/**
 * The same corpus as wire bytes (encodeEnvelope accepts every one: the codec is not the
 * defence here), so the byte path - decode, registry, host DOM - is attacked too.
 */
export function markupEnvelopeBytes(): readonly { name: string; bytes: Uint8Array }[] {
  return markupEnvelopes().map((c) => ({ name: c.name, bytes: encodeEnvelope(c.envelope) }));
}

// ---------------------------------------------------------------------------
// Oversized lengths: hand-built bytes the decoder must refuse before allocating
// ---------------------------------------------------------------------------

/** One over-cap wire message, with the limits-table field the ProtocolError must name. */
export interface HostileBytes {
  readonly name: string;
  readonly bytes: Uint8Array;
  readonly field: string;
}

function tileBodyBytes(rect: Rect | undefined, codec: number, data: Uint8Array): Uint8Array {
  const parts: Uint8Array[] = [];
  if (rect !== undefined) {
    parts.push(messageField(1, rectBytes(rect)));
  }
  parts.push(varintField(2, codec));
  parts.push(bytesField(3, data));
  return concat(parts);
}

/**
 * Every "oversized length" row of the fixture: each cap at its limit plus one, hand-encoded
 * because `encodeEnvelope` refuses to emit any of them.
 */
export function oversizedBytes(): readonly HostileBytes[] {
  const letterA = (count: number): Uint8Array => fillBytes(count, 0x41);

  const surfaceNewTitle = (length: number): Uint8Array =>
    envelope(F_SURFACE_NEW, stringField(5, 'A'.repeat(length)));
  const surfaceNewSize = (width: number): Uint8Array =>
    envelope(
      F_SURFACE_NEW,
      concat([
        varintField(1, 1), // surface_id
        messageField(4, concat([varintField(1, width), varintField(2, 1)])),
      ]),
    );

  const oneTile = messageField(4, tileBodyBytes(undefined, 1, fillBytes(4, 0)));

  return [
    {
      name: 'SurfaceNew.title of 513 bytes (cap 512)',
      bytes: surfaceNewTitle(MAX_TITLE_BYTES + 1),
      field: 'SurfaceNew.title',
    },
    {
      name: 'SurfaceMetadata.title of 513 bytes (cap 512)',
      bytes: envelope(F_SURFACE_METADATA, stringField(2, 'A'.repeat(MAX_TITLE_BYTES + 1))),
      field: 'SurfaceMetadata.title',
    },
    {
      name: 'SurfaceNew.app_id of 257 bytes (cap 256)',
      bytes: envelope(F_SURFACE_NEW, stringField(6, 'A'.repeat(MAX_APP_ID_BYTES + 1))),
      field: 'SurfaceNew.app_id',
    },
    {
      name: 'HelloReply.session_id of 65 bytes (cap 64)',
      bytes: envelope(F_HELLO_REPLY, stringField(2, 'A'.repeat(MAX_SESSION_ID_BYTES + 1))),
      field: 'HelloReply.session_id',
    },
    {
      name: 'Bye.text of 257 bytes (cap 256)',
      bytes: envelope(F_BYE, stringField(2, 'A'.repeat(MAX_BYE_TEXT_BYTES + 1))),
      field: 'Bye.text',
    },
    {
      name: 'ServerError.text of 1025 bytes (cap 1024)',
      bytes: envelope(F_SERVER_ERROR, stringField(2, 'A'.repeat(MAX_ERROR_TEXT_BYTES + 1))),
      field: 'ServerError.text',
    },
    {
      name: 'ClipboardSet.text of 65537 bytes (cap 65536)',
      bytes: envelope(F_CLIPBOARD_SET, stringField(1, 'A'.repeat(MAX_CLIPBOARD_BYTES + 1))),
      field: 'ClipboardSet.text',
    },
    {
      name: 'CursorImage.width of 129 pixels (cap 128)',
      bytes: envelope(
        F_CURSOR_IMAGE,
        concat([
          varintField(2, MAX_CURSOR_WIDTH + 1),
          varintField(3, 1),
          bytesField(6, letterA((MAX_CURSOR_WIDTH + 1) * 4)),
        ]),
      ),
      field: 'CursorImage.width',
    },
    {
      name: `Frame.tiles with ${MAX_TILES_PER_FRAME + 1} tiles (cap ${MAX_TILES_PER_FRAME})`,
      bytes: envelope(
        F_FRAME,
        concat(Array.from({ length: MAX_TILES_PER_FRAME + 1 }, () => oneTile)),
      ),
      field: 'Frame.tiles',
    },
    {
      name: `Tile.rect.width of ${MAX_TILE_WIDTH + 1} pixels (cap ${MAX_TILE_WIDTH})`,
      bytes: envelope(
        F_FRAME,
        messageField(
          4,
          tileBodyBytes(
            { x: 0, y: 0, width: MAX_TILE_WIDTH + 1, height: 1 },
            1,
            fillBytes((MAX_TILE_WIDTH + 1) * 4, 0),
          ),
        ),
      ),
      field: 'Tile.rect.width',
    },
    {
      name: `Tile.data of ${MAX_TILE_BYTES + 1} bytes (cap ${MAX_TILE_BYTES})`,
      bytes: envelope(
        F_FRAME,
        messageField(
          4,
          tileBodyBytes({ x: 0, y: 0, width: 1, height: 1 }, 1, letterA(MAX_TILE_BYTES + 1)),
        ),
      ),
      field: 'Tile.data',
    },
    {
      name: 'SurfaceNew.size.width of 1921 pixels (cap 1920)',
      bytes: surfaceNewSize(MAX_SURFACE_WIDTH + 1),
      field: 'SurfaceNew.size',
    },
    {
      name: `a message of ${MAX_MESSAGE_BYTES + 1} bytes (cap ${MAX_MESSAGE_BYTES})`,
      bytes: fillBytes(MAX_MESSAGE_BYTES + 1, 0),
      field: 'envelope',
    },
  ];
}

// ---------------------------------------------------------------------------
// Hostile popups: codec-legal placement that must stay inside parent + margin
// ---------------------------------------------------------------------------

/** One hostile `Positioner`, named for the CI report. */
export interface HostilePopupCase {
  readonly name: string;
  readonly positioner: Positioner;
}

/**
 * The "popup the size of the screen" corpus: maximal sizes, far coordinates (both signs),
 * zero-size anchor tricks and int32 extremes. Every one is codec-legal; `clampPopup` is the
 * defence under test.
 */
export function hostilePopups(): readonly HostilePopupCase[] {
  return [
    {
      name: 'screen-sized popup at the parent origin',
      positioner: {
        anchorRect: { x: 0, y: 0, width: 1, height: 1 },
        anchor: 5, // TOP_LEFT
        gravity: 5,
        size: { width: 1920, height: 1200 },
      },
    },
    {
      name: 'screen-sized popup centered on a mid-parent anchor',
      positioner: {
        anchorRect: { x: 150, y: 100, width: 0, height: 0 },
        anchor: 0, // CENTER
        gravity: 0,
        size: { width: 1920, height: 1200 },
      },
    },
    {
      name: 'a far negative offset',
      positioner: {
        anchorRect: { x: 10, y: 10, width: 20, height: 5 },
        anchor: 8, // BOTTOM_RIGHT
        gravity: 5,
        offset: { x: -100000, y: -100000 },
        size: { width: 200, height: 100 },
      },
    },
    {
      name: 'a far positive offset',
      positioner: {
        anchorRect: { x: 10, y: 10, width: 20, height: 5 },
        anchor: 5,
        gravity: 8,
        offset: { x: 100000, y: 100000 },
        size: { width: 200, height: 100 },
      },
    },
    {
      name: 'a zero-size anchor rect with a screen-sized popup',
      positioner: {
        anchorRect: { x: 0, y: 0, width: 0, height: 0 },
        anchor: 0,
        gravity: 0,
        size: { width: 1920, height: 1200 },
      },
    },
    {
      name: 'inverted gravity pulling the popup away from the parent',
      positioner: {
        anchorRect: { x: 299, y: 199, width: 1, height: 1 },
        anchor: 5,
        gravity: 8, // the popup's far corner on the anchor's near corner
        size: { width: 1920, height: 1200 },
      },
    },
    {
      name: 'int32-extreme offsets',
      positioner: {
        anchorRect: { x: -2147483648, y: 2147483647, width: 1, height: 1 },
        anchor: 5,
        gravity: 5,
        offset: { x: 2147483647, y: -2147483648 },
        size: { width: 256, height: 256 },
      },
    },
    {
      name: 'a zero-size popup (must stay zero: the clamp never grows a size)',
      positioner: {
        anchorRect: { x: 5, y: 5, width: 10, height: 10 },
        anchor: 5,
        gravity: 5,
        size: { width: 0, height: 0 },
      },
    },
  ];
}

/** A byte-encoded ROLE_POPUP SurfaceNew with a screen-sized positioner, parented at `parentId`. */
export function screenSizedPopupBytes(surfaceId: number, parentId: number): Uint8Array {
  const positioner = concat([
    messageField(
      1,
      rectBytes({ x: 0, y: 0, width: 1, height: 1 }),
    ),
    varintField(2, 5), // anchor TOP_LEFT
    varintField(3, 5), // gravity TOP_LEFT
    messageField(5, concat([varintField(1, 1920), varintField(2, 1200)])),
  ]);
  return envelope(
    F_SURFACE_NEW,
    concat([
      varintField(1, surfaceId),
      varintField(2, 1), // ROLE_POPUP
      varintField(3, parentId),
      messageField(7, positioner),
      varintField(8, 120), // scale_120ths
    ]),
  );
}

// ---------------------------------------------------------------------------
// Focus, frames, surfaces, Byes: the rest of the hostile corpus (typed)
// ---------------------------------------------------------------------------

/** How many FocusAsk envelopes the flood fires at one registry. */
export const FOCUS_FLOOD_COUNT = 512;

/** A flood of FocusAsk messages across a few surface ids: the server begging for focus. */
export function focusAskFlood(count = FOCUS_FLOOD_COUNT): readonly Envelope[] {
  return Array.from({ length: count }, (_, i) => ({
    kind: 'focusAsk' as const,
    focusAsk: { surfaceId: (i % 5) + 1 },
  }));
}

/** A 2x2 cursor whose hotspot claims to be far outside its own pixels. */
export function misleadingCursorEnvelope(serial = 1): Envelope {
  return {
    kind: 'cursorImage',
    cursorImage: {
      serial,
      width: 2,
      height: 2,
      hotspotX: 127,
      hotspotY: 127,
      argbPremultiplied: fillBytes(16, 0xff),
    },
  };
}

/** A valid RAW tile: `width*height*4` bytes of a single colour, so frames can mix and match. */
export function rawTile(x: number, y: number, width: number, height: number, fill = 0x80): Tile {
  return {
    rect: { x, y, width, height },
    codec: 1,
    data: fillBytes(width * height * 4, fill),
  };
}

/** A Frame envelope; the sequence and full_redraw are the hostile knobs. */
export function frameEnvelope(
  surfaceId: number,
  sequence: number,
  fullRedraw: boolean,
  tiles: readonly Tile[],
): Envelope {
  return {
    kind: 'frame',
    frame: { surfaceId, sequence, fullRedraw, tiles: [...tiles] },
  };
}

/** One frame-abuse case: the frame, and whether the renderer may draw it. */
export interface FrameAbuseCase {
  readonly name: string;
  readonly envelope: Envelope;
  readonly draws: boolean;
}

/** Frames whose tiles claim pixels outside the surface: every one must be dropped whole. */
export function outOfBoundsFrames(
  surfaceId: number,
  surfaceSize: { width: number; height: number },
): readonly FrameAbuseCase[] {
  return [
    {
      name: 'a tile starting left of the surface',
      envelope: frameEnvelope(surfaceId, 1, false, [rawTile(-1, 0, 1, 1)]),
      draws: false,
    },
    {
      name: 'a tile starting above the surface',
      envelope: frameEnvelope(surfaceId, 1, false, [rawTile(0, -1, 1, 1)]),
      draws: false,
    },
    {
      name: 'a tile running past the right edge',
      envelope: frameEnvelope(
        surfaceId,
        1,
        false,
        [rawTile(surfaceSize.width - 1, 0, 2, 1)],
      ),
      draws: false,
    },
    {
      name: 'a tile running past the bottom edge',
      envelope: frameEnvelope(
        surfaceId,
        1,
        false,
        [rawTile(0, surfaceSize.height - 1, 1, 2)],
      ),
      draws: false,
    },
    {
      name: 'a tile bigger than the whole surface',
      envelope: frameEnvelope(
        surfaceId,
        1,
        false,
        [rawTile(0, 0, surfaceSize.width + 1, surfaceSize.height + 1)],
      ),
      draws: false,
    },
    {
      name: 'a tile with no rect at all',
      envelope: frameEnvelope(surfaceId, 1, false, [{ codec: 1, data: fillBytes(4, 0) }]),
      draws: false,
    },
    {
      name: 'a tile naming an unknown codec',
      envelope: frameEnvelope(surfaceId, 1, false, [
        { rect: { x: 0, y: 0, width: 1, height: 1 }, codec: 3, data: fillBytes(4, 0) },
      ]),
      draws: false,
    },
    {
      name: 'a RAW tile with a short payload',
      envelope: frameEnvelope(surfaceId, 1, false, [
        { rect: { x: 0, y: 0, width: 2, height: 1 }, codec: 1, data: fillBytes(4, 0) },
      ]),
      draws: false,
    },
    {
      name: 'one legal tile smuggled after a hostile one',
      envelope: frameEnvelope(surfaceId, 1, false, [
        rawTile(-50, -50, 4, 4),
        rawTile(1, 1, 1, 1),
      ]),
      draws: false,
    },
  ];
}

/**
 * Sequence abuse, in the order the tests replay it against a renderer that has already drawn
 * sequence 5: stale, repeated and wrapped sequences are dropped; only a full_redraw repairs a
 * wrapped-to-zero sequence; a huge forward jump is simply new.
 */
export function sequenceAbuseFrames(surfaceId: number): readonly FrameAbuseCase[] {
  return [
    {
      name: 'a sequence equal to the last drawn',
      envelope: frameEnvelope(surfaceId, 5, false, [rawTile(0, 0, 1, 1)]),
      draws: false,
    },
    {
      name: 'an older repeated sequence',
      envelope: frameEnvelope(surfaceId, 3, false, [rawTile(0, 0, 1, 1)]),
      draws: false,
    },
    {
      name: 'a wrap to sequence 0 without full_redraw',
      envelope: frameEnvelope(surfaceId, 0, false, [rawTile(0, 0, 1, 1)]),
      draws: false,
    },
    {
      name: 'a wrap to sequence 0 as a full_redraw',
      envelope: frameEnvelope(surfaceId, 0, true, [rawTile(2, 2, 1, 1)]),
      draws: true,
    },
    {
      name: 'a jump forward to 2^32-1',
      envelope: frameEnvelope(surfaceId, 0xffffffff, false, [rawTile(3, 3, 1, 1)]),
      draws: true,
    },
  ];
}

/** A run of back-to-back full_redraw frames, the "sync storm" shape. */
export function fullRedrawFlood(count: number, surfaceId: number): readonly Envelope[] {
  return Array.from({ length: count }, (_, i) =>
    frameEnvelope(surfaceId, i + 1, true, [rawTile(1, 1, 1, 1)]),
  );
}

/** More SurfaceNew envelopes than the session cap, all distinct ids. */
export function surfaceFlood(count: number): readonly Envelope[] {
  return Array.from({ length: count }, (_, i) => ({
    kind: 'surfaceNew' as const,
    surfaceNew: {
      surfaceId: i + 1,
      role: 0 as const,
      size: { width: 8, height: 8 },
      title: 'flood',
      appId: 'flood.app',
      scale120ths: 120,
    },
  }));
}

/** A Bye whose text is markup: untrusted display text, exactly like every other server string. */
export function byeEnvelope(reason: ByeReason): Envelope {
  return {
    kind: 'bye',
    bye: { reason, text: '<img src=x onerror=window.__pwned=1> session over' },
  };
}

/** The Bye reasons after which the client never reconnects (connection.ts FATAL_BYE_REASONS). */
export function fatalByeReasons(): readonly { name: string; reason: ByeReason }[] {
  return [
    { name: 'PROTOCOL_VERSION (0x100)', reason: 0x100 },
    { name: 'AUTH_FAILED (0x102)', reason: 0x102 },
    { name: 'SESSION_GONE (0x104)', reason: 0x104 },
  ];
}
