import { readFileSync } from 'node:fs';

import { describe, expect, it } from 'vitest';

import { PROTOCOL_VERSION } from './protocol.js';

describe('PROTOCOL_VERSION', () => {
  it('equals the Rust constant in appricot-proto', () => {
    const lib = new URL('../../../crates/appricot-proto/src/lib.rs', import.meta.url);
    const rust = readFileSync(lib, 'utf8');
    const match = /^pub const PROTOCOL_VERSION: u16 = (\d+);$/m.exec(rust);
    expect(match, 'PROTOCOL_VERSION not found in appricot-proto').not.toBeNull();
    expect(PROTOCOL_VERSION).toBe(Number(match?.[1]));
  });
});
