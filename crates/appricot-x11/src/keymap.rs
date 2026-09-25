//! Keysym to keycode translation over the core protocol.
//!
//! No XKB requests: the backend asks `GetKeyboardMapping` once (and again on `MappingNotify`)
//! and matches a keysym against every column the server reports per keycode. A column's
//! parity is its Shift state, and a column past the first two needs the level-3 or
//! second-group shift — the key whose keysym is `ISO_Level3_Shift` or `Mode_switch` — held
//! around the press, exactly as [`crate`]'s input module already synthesises exact Shift.
//! A keysym the modifiers cannot reach (a second group the server keeps locked out, or a
//! column past what any held modifier selects) is typed by rebinding an unused keycode for
//! the press, the technique VNC servers use; a keysym no column of the keymap produces at
//! all is not this layout's to type and is refused with
//! [`BackendError::KeysymUnavailable`](crate::BackendError::KeysymUnavailable) instead of
//! pressed as the wrong key.
//!
//! # The wire's keysyms and the keymap's
//!
//! The wire (docs/protocol/v0.md §8) sends a printable Latin-1 character as its codepoint,
//! which is also its X11 keysym, and every other character as X11's Unicode keysym,
//! `0x01000000 + codepoint`. Keys that produce no character arrive as X11's named keysyms.
//! Any other number below `0x01000000` is an X11 keysym as `keysymdef.h` defines it, legacy
//! blocks included: `0x03a3` is `XK_Rcedilla` (Latin-4), never a Greek capital sigma.
//!
//! An X keymap may list a character under its Unicode keysym or under a legacy named one.
//! [`candidates`] names the spellings this module knows:
//!
//! - Latin-1 printable characters carry their codepoint as their keysym;
//! - the plain Greek letters' named keysyms sit at `0x07xx` (the Greek letter assignments
//!   of `keysymdef.h` in xorgproto);
//! - a character of U+2000 to U+20FF may carry its codepoint as its keysym number — the
//!   Euro sign's `EuroSign` is `0x20ac`, the number of U+20AC — because that block of
//!   `keysymdef.h` assigns its Unicode codepoints directly.
//!
//! # What the columns mean
//!
//! The server flattens its groups into the core map: on a `setxkbmap us,gr` map the first
//! two columns are the US group's levels, the next pair the Greek group's first two
//! levels, and the columns after those the Greek group's AltGr levels (measured on the
//! dev container's Xvfb, 2026-09-24; a single-group map duplicates its first two columns
//! and then lists its AltGr levels). The server processes a keycode event against its own
//! group state, so a column is reachable only when some modifier state selects it — which
//! is why [`Plan`] carries a rebind step for the columns no modifier reaches.

use crate::BackendError;

/// `XK_Shift_L` from `keysymdef.h`: the keysym of the physical Shift key.
pub(crate) const XK_SHIFT_L: u32 = 0xffe1;

/// `XK_Caps_Lock` from `keysymdef.h`.
pub(crate) const XK_CAPS_LOCK: u32 = 0xffe5;

/// `XK_ISO_Level3_Shift` (`0xfe03`) from `keysymdef.h`: holding a key that carries it
/// selects the AltGr columns of a key whose type has a third level. Verified against the
/// xkbcommon keysym header (checked 2026-09-24).
const XK_ISO_LEVEL3_SHIFT: u32 = 0xfe03;

/// `XK_Mode_switch` (`0xff7e`), the pre-XKB spelling of the second-group shift.
const XK_MODE_SWITCH: u32 = 0xff7e;

/// `XK_dead_acute` (`0xfe51`): the dead acute accent a Greek layout types tonos with.
const XK_DEAD_ACUTE: u32 = 0xfe51;

/// `XK_dead_diaeresis` (`0xfe57`): the dead diaeresis for the Greek dialytika.
const XK_DEAD_DIAERESIS: u32 = 0xfe57;

/// The first X11 Unicode keysym: `0x01000000 + codepoint`.
const UNICODE_KEYSYM_BASE: u32 = 0x0100_0000;

/// The last X11 Unicode keysym, for U+10FFFF.
const UNICODE_KEYSYM_LAST: u32 = 0x0110_ffff;

/// A keysym resolved against the server's keymap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Resolved {
    /// The keycode to press with XTEST.
    pub keycode: u8,
    /// The column the keysym sits in: 0 and 1 are the first group's unshifted and shifted
    /// levels; 2 and later belong to AltGr levels or further groups.
    pub column: usize,
}

/// The server's core-protocol keyboard map, rebuilt on `MappingNotify`.
#[derive(Debug, Clone)]
pub(crate) struct Keymap {
    min_keycode: u8,
    /// Keysyms per keycode, from column 0 up. Index 0 is keycode `min_keycode`.
    per_keycode: Vec<Vec<u32>>,
}

impl Keymap {
    /// Builds the map from a `GetKeyboardMapping` reply's numbers.
    ///
    /// `flat` is the reply's `keysyms` list: `keysyms_per_keycode` entries per keycode in
    /// order, which is how the core protocol packs it (the last keycodes may carry a
    /// shorter, zero-padded tail).
    pub(crate) fn from_flat(min_keycode: u8, keysyms_per_keycode: u8, flat: &[u32]) -> Self {
        let width = usize::from(keysyms_per_keycode).max(1);
        let per_keycode = flat
            .chunks(width)
            .map(|chunk| {
                let mut syms = chunk.to_vec();
                syms.resize(width, 0);
                syms
            })
            .collect();
        Self {
            min_keycode,
            per_keycode,
        }
    }

    /// Finds the keycode and column that produce `keysym`, preferring the lowest keycode
    /// and the lowest column.
    ///
    /// `keysym` must be a keysym the keymap could list: callers pass the output of
    /// [`candidates`]. A zero keysym (`NoSymbol`) resolves to nothing.
    pub(crate) fn resolve(&self, keysym: u32) -> Option<Resolved> {
        if keysym == 0 {
            return None;
        }
        let mut best: Option<Resolved> = None;
        for (index, syms) in self.per_keycode.iter().enumerate() {
            for (column, &sym) in syms.iter().enumerate() {
                if sym != keysym {
                    continue;
                }
                let found = Resolved {
                    keycode: self.min_keycode + u8::try_from(index).unwrap_or(u8::MAX),
                    column,
                };
                if best.is_none_or(|b| found.column < b.column) {
                    best = Some(found);
                }
                if best.is_some_and(|b| b.column == 0) {
                    return best;
                }
            }
        }
        best
    }

    /// The keycode of the left Shift key, which the backend presses when a keysym needs
    /// the shifted column and the user holds no Shift.
    pub(crate) fn shift_keycode(&self) -> Option<u8> {
        self.resolve(XK_SHIFT_L).map(|r| r.keycode)
    }

    /// The keycode of the level-3 or second-group shift: a key whose keysym is
    /// `ISO_Level3_Shift`, else one carrying `Mode_switch`. Held around a press whose
    /// keysym sits in a column past the first two.
    pub(crate) fn level3_keycode(&self) -> Option<u8> {
        self.resolve(XK_ISO_LEVEL3_SHIFT)
            .or_else(|| self.resolve(XK_MODE_SWITCH))
            .map(|r| r.keycode)
    }

    /// True when modifier choreography reaches `found`'s column on this keymap.
    ///
    /// Columns 0 and 1 are the active group's Shift pair. A column past them is reachable
    /// only when the keycode's own third and fourth columns repeat its first two: the
    /// server exports a key's single group repeated across the core map's group columns
    /// and its AltGr levels after them, so those later columns still belong to the active
    /// group and the level-3 shift selects them. When the third and fourth columns hold
    /// something else, the key carries a second group there — the server keeps the first
    /// group active and no held modifier reaches it (measured on the dev container's
    /// Xvfb, 2026-09-24: `setxkbmap us,gr` lists Greek in those columns and delivers the
    /// US keysym however the modifiers sit).
    pub(crate) fn reachable(&self, found: Resolved) -> bool {
        if found.column < 2 {
            return true;
        }
        if self.level3_keycode().is_none() {
            return false;
        }
        self.row(found.keycode).is_some_and(|row| {
            row.len() >= 4 && row.get(2) == row.first() && row.get(3) == row.get(1)
        })
    }

    /// One keycode's row of keysyms, from column 0 up.
    fn row(&self, keycode: u8) -> Option<&[u32]> {
        keycode
            .checked_sub(self.min_keycode)
            .and_then(|index| self.per_keycode.get(usize::from(index)))
            .map(Vec::as_slice)
    }

    /// The keysym columns the server reports per keycode; the width a rebind writes.
    /// The keysym columns the server reports per keycode, as the width a rebind writes:
    /// the same `u8` the server's reply named, so it needs no cast back.
    pub(crate) fn width(&self) -> u8 {
        u8::try_from(self.per_keycode.first().map_or(0, Vec::len)).unwrap_or(0)
    }

    /// A keycode whose every column is empty, to rebind a keysym onto; the highest such
    /// keycode, skipping any `exclude`d one (a rebind still held). `None` when the keymap
    /// has no free keycode at all.
    pub(crate) fn spare_keycode(&self, exclude: &[u8]) -> Option<u8> {
        self.per_keycode
            .iter()
            .enumerate()
            .rev()
            .filter(|(_, syms)| syms.iter().all(|s| *s == 0))
            .map(|(index, _)| self.min_keycode + u8::try_from(index).unwrap_or(u8::MAX))
            .find(|keycode| !exclude.contains(keycode))
    }
}

/// The keysyms an X keymap may list the wire keysym under, best first: at most three.
///
/// - A Unicode keysym for a printable Latin-1 character becomes the Latin-1 keysym, which
///   is the codepoint.
/// - Any other Unicode keysym is itself, then its legacy named keysym when this module
///   knows one: the plain Greek letters, and the codepoint's own number for the characters
///   of U+0100 to U+20FF whose legacy keysym carries it (the Euro sign).
/// - Everything else is an X11 keysym and is passed through as it is.
pub(crate) fn candidates(keysym: u32) -> [Option<u32>; 3] {
    if !(UNICODE_KEYSYM_BASE..=UNICODE_KEYSYM_LAST).contains(&keysym) {
        return [Some(keysym), None, None];
    }
    let codepoint = keysym - UNICODE_KEYSYM_BASE;
    if is_printable_latin1(codepoint) {
        return [Some(codepoint), None, None];
    }
    [
        Some(keysym),
        legacy_greek(codepoint).or_else(|| legacy_named(codepoint)),
        None,
    ]
}

/// True for the printable Latin-1 codepoints, U+0020-U+007E and U+00A0-U+00FF: their X11
/// keysyms carry the same numbers as the characters.
fn is_printable_latin1(codepoint: u32) -> bool {
    (0x20..=0x7e).contains(&codepoint) || (0xa0..=0xff).contains(&codepoint)
}

/// The named X11 keysym of a plain Greek letter, from `keysymdef.h`:
///
/// - U+0391-U+03A1 (Α..Ρ) are 0x07c1-0x07d1,
/// - U+03A3 (Σ) is 0x07d2, and U+03A4-U+03A9 (Τ..Ω) are 0x07d4-0x07d9,
/// - U+03B1-U+03C1 (α..ρ) are 0x07e1-0x07f1,
/// - U+03C2 (ς) is 0x07f3, U+03C3 (σ) is 0x07f2, and U+03C4-U+03C9 (τ..ω) are
///   0x07f4-0x07f9.
///
/// Accented Greek and the archaic letters are left out on purpose: a Greek layout types
/// them through a dead key, which no single keycode reproduces — [`greek_dead_split`]
/// names the sequences instead.
fn legacy_greek(codepoint: u32) -> Option<u32> {
    match codepoint {
        // Uppercase Α..Ρ, no gap.
        0x0391..=0x03a1 => Some(0x07c1 + (codepoint - 0x0391)),
        // Σ is 0x07d2; Τ..Ω are 0x07d4..0x07d9 (0x07d3 is not assigned).
        0x03a3 => Some(0x07d2),
        0x03a4..=0x03a9 => Some(0x07d4 + (codepoint - 0x03a4)),
        // Lowercase α..ρ, no gap.
        0x03b1..=0x03c1 => Some(0x07e1 + (codepoint - 0x03b1)),
        // ς is 0x07f3 and σ is 0x07f2; τ..ω are 0x07f4..0x07f9.
        0x03c2 => Some(0x07f3),
        0x03c3 => Some(0x07f2),
        0x03c4..=0x03c9 => Some(0x07f4 + (codepoint - 0x03c4)),
        _ => None,
    }
}

/// The legacy named keysym that carries the character's own codepoint as its number, for
/// the characters of U+2000 to U+20FF: `keysymdef.h`'s punctuation and currency block
/// assigns its Unicode codepoints as keysym numbers — `XK_EuroSign` is 0x20ac, the number
/// of U+20AC, which is how the dev container's Greek keymap lists the Euro sign (measured
/// 2026-09-24). xkbcommon's converter reaches legacy keysyms through a curated table over
/// U+0100-U+20FF rather than any blanket rule
/// ([keysym docs](https://xkbcommon.org/doc/current/group__keysyms.html), checked
/// 2026-09-24); only this block is systematic, so only it is offered. A number that is no
/// real keysym simply matches no column of any keymap, so offering one is safe anyway.
fn legacy_named(codepoint: u32) -> Option<u32> {
    (0x2000..=0x20ff).contains(&codepoint).then_some(codepoint)
}

/// One key event's worth of delivery, as [`plan`] computed it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Step {
    /// The keycode is reached by modifier choreography alone: X's Shift set to `shift`,
    /// and when the keysym sits past the first two columns the keymap's level-3 key held
    /// around the press.
    Direct {
        /// The keysym as the keymap lists it (what a rebind of it would write).
        keysym: u32,
        /// The keycode to press.
        keycode: u8,
        /// The column's Shift parity.
        shift: bool,
        /// True when the column needs the level-3 or second-group shift held.
        level3: bool,
    },
    /// The keycode is rebound to `keysym` for this one event and restored after it.
    Rebind { keysym: u32 },
}

/// How one wire keysym is typed against the keymap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Plan {
    /// One key event.
    Key(Step),
    /// The layout's standard dead-key sequence: each dead key of `taps` is typed and
    /// released, then the base key is pressed and held.
    Dead { taps: Vec<Step>, base: Step },
}

/// Turns a wire keysym into the key events that type it, or refuses it.
///
/// The order of preference:
///
/// 1. a candidate that some modifier state reaches, at the lowest column ([`Step::Direct`]);
/// 2. for a precomposed accented Greek vowel, the layout's dead-key sequence
///    ([`greek_dead_split`]), when the map holds every part of it;
/// 3. a candidate the keymap lists but no modifier state reaches, typed by rebinding an
///    unused keycode ([`Step::Rebind`]) — never a keysym the keymap does not hold at all;
/// 4. anything else is [`BackendError::KeysymUnavailable`], never pressed as another key.
pub(crate) fn plan(keymap: &Keymap, keysym: u32) -> Result<Plan, BackendError> {
    let step_of = |candidate: u32, found: Resolved| -> Step {
        if keymap.reachable(found) {
            Step::Direct {
                keysym: candidate,
                keycode: found.keycode,
                shift: found.column % 2 == 1,
                level3: found.column >= 2,
            }
        } else {
            Step::Rebind { keysym: candidate }
        }
    };
    let best = best_candidate(keymap, keysym);
    if let Some((candidate, found)) = best.filter(|(_, found)| keymap.reachable(*found)) {
        return Ok(Plan::Key(step_of(candidate, found)));
    }
    if (UNICODE_KEYSYM_BASE..=UNICODE_KEYSYM_LAST).contains(&keysym)
        && let Some((base, deads)) = greek_dead_split(keysym - UNICODE_KEYSYM_BASE)
    {
        let mut steps = Vec::with_capacity(deads.len() + 1);
        let mut all = true;
        for part in deads.iter().copied().chain([UNICODE_KEYSYM_BASE + base]) {
            if let Some((candidate, found)) = best_candidate(keymap, part) {
                steps.push(step_of(candidate, found));
            } else {
                all = false;
                break;
            }
        }
        if all && let Some(base_step) = steps.pop() {
            return Ok(Plan::Dead {
                taps: steps,
                base: base_step,
            });
        }
    }
    match best {
        // In the keymap but out of every modifier's reach: type it by rebind.
        Some((candidate, _)) => Ok(Plan::Key(Step::Rebind { keysym: candidate })),
        None => Err(BackendError::KeysymUnavailable(keysym)),
    }
}

/// The lowest-column resolution of `keysym` over all its [`candidates`], with the
/// candidate that matched (the keysym a rebind of it must write).
fn best_candidate(keymap: &Keymap, keysym: u32) -> Option<(u32, Resolved)> {
    let mut best: Option<(u32, Resolved)> = None;
    for candidate in candidates(keysym).into_iter().flatten() {
        if let Some(found) = keymap.resolve(candidate)
            && best.is_none_or(|(_, b)| found.column < b.column)
        {
            best = Some((candidate, found));
        }
    }
    best
}

/// The keycode a release of `keysym` should come off when the wire named no code: the
/// lowest-column candidate's keycode, or the spare a live rebind bound it to.
pub(crate) fn release_keycode(keymap: &Keymap, keysym: u32, rebound: &[(u8, u32)]) -> Option<u8> {
    if let Some((_, found)) = best_candidate(keymap, keysym) {
        return Some(found.keycode);
    }
    rebound
        .iter()
        .find(|(_, bound)| candidates(keysym).contains(&Some(*bound)))
        .map(|(keycode, _)| *keycode)
}

/// The dead keys and the base vowel a Greek layout types a precomposed Greek vowel with:
/// ά is a dead acute tap then α. The splits are the characters' canonical decompositions
/// (U+03AC is U+03B1 plus U+0301; U+0390 is U+03B9 plus U+0308 plus U+0301), their marks
/// mapped to the dead keysyms of `keysymdef.h`: the tonos is a combining acute, and the
/// dialytika a combining diaeresis. Composition into a glyph stays the toolkit's job;
/// this names the keysyms to deliver (checked against the Unicode data, 2026-09-24).
fn greek_dead_split(codepoint: u32) -> Option<(u32, &'static [u32])> {
    const ACUTE: &[u32] = &[XK_DEAD_ACUTE];
    const DIAERESIS: &[u32] = &[XK_DEAD_DIAERESIS];
    const DIAERESIS_ACUTE: &[u32] = &[XK_DEAD_DIAERESIS, XK_DEAD_ACUTE];
    match codepoint {
        0x0386 => Some((0x0391, ACUTE)),           // Ά
        0x0388 => Some((0x0395, ACUTE)),           // Έ
        0x0389 => Some((0x0397, ACUTE)),           // Ή
        0x038a => Some((0x0399, ACUTE)),           // Ί
        0x038c => Some((0x039f, ACUTE)),           // Ό
        0x038e => Some((0x03a5, ACUTE)),           // Ύ
        0x038f => Some((0x03a9, ACUTE)),           // Ώ
        0x03aa => Some((0x0399, DIAERESIS)),       // Ϊ
        0x03ab => Some((0x03a5, DIAERESIS)),       // Ϋ
        0x0390 => Some((0x03b9, DIAERESIS_ACUTE)), // ΐ
        0x03ac => Some((0x03b1, ACUTE)),           // ά
        0x03ad => Some((0x03b5, ACUTE)),           // έ
        0x03ae => Some((0x03b7, ACUTE)),           // ή
        0x03af => Some((0x03b9, ACUTE)),           // ί
        0x03cc => Some((0x03bf, ACUTE)),           // ό
        0x03cd => Some((0x03c5, ACUTE)),           // ύ
        0x03ce => Some((0x03c9, ACUTE)),           // ώ
        0x03ca => Some((0x03b9, DIAERESIS)),       // ϊ
        0x03cb => Some((0x03c5, DIAERESIS)),       // ϋ
        _ => None,
    }
}

/// What a wire keysym is to the backend's modifier handling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KeyKind {
    /// A modifier the backend forwards and remembers as held.
    Modifier(Modifier),
    /// A key whose effect the browser already applied to the character keysyms: Caps Lock,
    /// Shift Lock, and the AltGr level shifts. It is not forwarded.
    Consumed,
    /// A key that produces a character. Its keysym already carries Shift, Caps Lock and
    /// AltGr, so the backend sets the X modifier state to match it.
    Character,
    /// Any other key: Enter, the arrows, the function keys. The held modifiers apply as
    /// they are.
    Named,
}

/// A modifier class, as the backend tracks it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Modifier {
    /// `Shift_L`, `Shift_R`.
    Shift,
    /// `Control_L`, `Control_R`.
    Control,
    /// `Alt_L`, `Alt_R`. A browser on Windows reports AltGr as Control plus `AltRight`.
    Alt,
    /// `Meta_L`, `Meta_R`, `Super_L`, `Super_R`, `Hyper_L`, `Hyper_R`.
    Other,
}

/// Classifies a wire keysym.
pub(crate) fn key_kind(keysym: u32) -> KeyKind {
    match keysym {
        0xffe1 | 0xffe2 => KeyKind::Modifier(Modifier::Shift),
        0xffe3 | 0xffe4 => KeyKind::Modifier(Modifier::Control),
        0xffe9 | 0xffea => KeyKind::Modifier(Modifier::Alt),
        0xffe7 | 0xffe8 | 0xffeb..=0xffee => KeyKind::Modifier(Modifier::Other),
        // Caps_Lock, Shift_Lock; ISO_Level3_Shift, ISO_Level5_Shift, Mode_switch.
        0xffe5 | 0xffe6 | 0xfe03 | 0xfe11 | 0xff7e => KeyKind::Consumed,
        // The character blocks of keysymdef.h end below 0xfe00; 0xfe00-0xffff are the
        // keyboard's function, modifier, dead and keypad keysyms.
        0x20..=0xfdff | UNICODE_KEYSYM_BASE..=UNICODE_KEYSYM_LAST => KeyKind::Character,
        _ => KeyKind::Named,
    }
}

/// The characters a US layout puts on each physical key that is not a letter: unshifted,
/// then shifted. `KeyboardEvent.code` names the key by its US position.
const US_SYMBOLS: [(&str, [char; 2]); 22] = [
    ("Backquote", ['`', '~']),
    ("Digit1", ['1', '!']),
    ("Digit2", ['2', '@']),
    ("Digit3", ['3', '#']),
    ("Digit4", ['4', '$']),
    ("Digit5", ['5', '%']),
    ("Digit6", ['6', '^']),
    ("Digit7", ['7', '&']),
    ("Digit8", ['8', '*']),
    ("Digit9", ['9', '(']),
    ("Digit0", ['0', ')']),
    ("Minus", ['-', '_']),
    ("Equal", ['=', '+']),
    ("BracketLeft", ['[', '{']),
    ("BracketRight", [']', '}']),
    ("Backslash", ['\\', '|']),
    ("Semicolon", [';', ':']),
    ("Quote", ['\'', '"']),
    ("Comma", [',', '<']),
    ("Period", ['.', '>']),
    ("Slash", ['/', '?']),
    ("IntlBackslash", ['<', '>']),
];

/// True when a character typed under Control, Alt or another non-Shift modifier reads as a
/// shortcut rather than as text: an ASCII letter or digit, or a character the US layout puts
/// on the physical key `code` names.
///
/// Anything else under Alt is what the browser made of AltGr (or of the Option key), which
/// already sits in the keysym: German AltGr+Q arrives as `@` from `KeyQ`, and must reach the
/// app as `@` with neither Control nor Alt down.
pub(crate) fn is_shortcut_character(keysym: u32, code: Option<&str>) -> bool {
    // Only a Latin-1 keysym can be one of them: its number is its character.
    let Some(c) = candidates(keysym)[0]
        .filter(|sym| *sym <= 0xff)
        .and_then(char::from_u32)
    else {
        return false;
    };
    if c.is_ascii_alphanumeric() {
        return true;
    }
    code.and_then(|code| US_SYMBOLS.iter().find(|(name, _)| *name == code))
        .is_some_and(|(_, symbols)| symbols.contains(&c))
}

#[cfg(test)]
mod tests {
    use super::{
        KeyKind, Keymap, Modifier, Plan, Resolved, Step, candidates, is_shortcut_character,
        key_kind, plan, release_keycode,
    };
    use crate::BackendError;

    // Four columns (base, shift, AltGr, shift+AltGr) for a handful of keycodes from 8 up:
    // this is the shape of Xvfb's default map around the letters.
    fn default_like_map() -> Keymap {
        let mut flat = vec![];
        for _ in 0..42 {
            flat.extend_from_slice(&[0, 0, 0, 0]);
        }
        // Keycode 50 (index 42): Shift_L.
        flat.extend_from_slice(&[0xffe1, 0xffe1, 0, 0]);
        for _ in 0..9 {
            flat.extend_from_slice(&[0, 0, 0, 0]);
        }
        // Keycode 60 (index 52): 'a' / 'A'.
        flat.extend_from_slice(&[0x61, 0x41, 0, 0]);
        // Keycode 61 (index 53): a keysym that only AltGr reaches ('æ' in column 2).
        flat.extend_from_slice(&[0, 0, 0xc3, 0]);
        // Keycode 62 (index 54): Greek alpha on a Greek layout, cols 0 and 1 (α, Α).
        flat.extend_from_slice(&[0x07e1, 0x07c1, 0, 0]);
        // Keycode 63 (index 55): a layout that lists 'ơ' by its Unicode keysym.
        flat.extend_from_slice(&[0x0100_01a1, 0x0100_01a0, 0, 0]);
        // Keycode 64 (index 56): Latin-4's legacy Aogonek, the keysym a bare U+01A1 names.
        flat.extend_from_slice(&[0x01b1, 0x01a1, 0, 0]);
        Keymap::from_flat(8, 4, &flat)
    }

    /// The map above plus a level-3 key on keycode 70: ISO_Level3_Shift in column 0. The
    /// 'æ' row stays a "second group" row (its third and fourth columns hold something
    /// else), so even a present level-3 key must not reach it.
    fn map_with_level3_key() -> Keymap {
        let mut flat = vec![];
        for _ in 0..42 {
            flat.extend_from_slice(&[0, 0, 0, 0]);
        }
        // Keycode 50 (index 42): Shift_L.
        flat.extend_from_slice(&[0xffe1, 0xffe1, 0, 0]);
        for _ in 0..9 {
            flat.extend_from_slice(&[0, 0, 0, 0]);
        }
        // Keycodes 60 to 64: as in the map above.
        flat.extend_from_slice(&[0x61, 0x41, 0, 0]);
        flat.extend_from_slice(&[0, 0, 0xc3, 0]);
        flat.extend_from_slice(&[0x07e1, 0x07c1, 0, 0]);
        flat.extend_from_slice(&[0x0100_01a1, 0x0100_01a0, 0, 0]);
        flat.extend_from_slice(&[0x01b1, 0x01a1, 0, 0]);
        for _ in 0..5 {
            flat.extend_from_slice(&[0, 0, 0, 0]);
        }
        // Keycode 70 (index 62): ISO_Level3_Shift.
        flat.extend_from_slice(&[0xfe03, 0xfe03, 0, 0]);
        Keymap::from_flat(8, 4, &flat)
    }

    /// A single-group map with AltGr levels, the shape of `setxkbmap gr` (width 6): every
    /// letter key repeats its first two columns, its AltGr levels follow, and a level-3
    /// key sits on keycode 70. Keycode 61 carries 'æ' at column 4 and 'Æ' at column 5.
    fn single_group_altgr_map() -> Keymap {
        let mut flat = vec![];
        for _ in 0..42 {
            flat.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
        }
        // Keycode 50: Shift_L.
        flat.extend_from_slice(&[0xffe1, 0xffe1, 0, 0, 0, 0]);
        for _ in 0..9 {
            flat.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
        }
        // Keycode 60: 'a'/'A' repeated, nothing at the AltGr levels.
        flat.extend_from_slice(&[0x61, 0x41, 0x61, 0x41, 0, 0]);
        // Keycode 61: 'b'/'B' repeated, 'æ' at level 3 and 'Æ' at level 4.
        flat.extend_from_slice(&[0x62, 0x42, 0x62, 0x42, 0xc3, 0xc5]);
        // Keycode 62: Greek alpha.
        flat.extend_from_slice(&[0x07e1, 0x07c1, 0x07e1, 0x07c1, 0, 0]);
        for _ in 0..7 {
            flat.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
        }
        // Keycode 70: ISO_Level3_Shift.
        flat.extend_from_slice(&[0xfe03, 0xfe03, 0, 0, 0, 0]);
        Keymap::from_flat(8, 6, &flat)
    }

    #[test]
    fn latin_one_keysyms_resolve_in_column_zero() {
        let km = default_like_map();
        assert_eq!(
            km.resolve(0x61),
            Some(Resolved {
                keycode: 60,
                column: 0
            })
        );
    }

    #[test]
    fn shifted_symbols_sit_in_column_one() {
        let km = default_like_map();
        assert_eq!(
            km.resolve(0x41),
            Some(Resolved {
                keycode: 60,
                column: 1
            })
        );
    }

    #[test]
    fn shift_lives_on_its_own_key() {
        let km = default_like_map();
        assert_eq!(km.shift_keycode(), Some(50));
    }

    #[test]
    fn the_level3_key_is_found_by_either_keysym() {
        assert_eq!(default_like_map().level3_keycode(), None);
        assert_eq!(map_with_level3_key().level3_keycode(), Some(70));
    }

    #[test]
    fn the_width_and_spare_keycodes_come_from_the_map() {
        let km = default_like_map();
        assert_eq!(km.width(), 4);
        // The highest all-empty keycode, and a different one once it is taken.
        let first = km.spare_keycode(&[]).expect("empty keycodes exist");
        let second = km.spare_keycode(&[first]).expect("another empty keycode");
        assert_ne!(first, second);
    }

    #[test]
    fn unicode_greek_names_its_legacy_keysym_as_a_second_choice() {
        assert_eq!(
            candidates(0x0100_03b1),
            [Some(0x0100_03b1), Some(0x07e1), None]
        ); // α
        assert_eq!(
            candidates(0x0100_03c1),
            [Some(0x0100_03c1), Some(0x07f1), None]
        ); // ρ
        assert_eq!(
            candidates(0x0100_03c2),
            [Some(0x0100_03c2), Some(0x07f3), None]
        ); // ς final
        assert_eq!(
            candidates(0x0100_03c3),
            [Some(0x0100_03c3), Some(0x07f2), None]
        ); // σ
        assert_eq!(
            candidates(0x0100_03c9),
            [Some(0x0100_03c9), Some(0x07f9), None]
        ); // ω
        assert_eq!(
            candidates(0x0100_0391),
            [Some(0x0100_0391), Some(0x07c1), None]
        ); // Α
        assert_eq!(
            candidates(0x0100_03a1),
            [Some(0x0100_03a1), Some(0x07d1), None]
        ); // Ρ
        assert_eq!(
            candidates(0x0100_03a3),
            [Some(0x0100_03a3), Some(0x07d2), None]
        ); // Σ
        assert_eq!(
            candidates(0x0100_03a9),
            [Some(0x0100_03a9), Some(0x07d9), None]
        ); // Ω
    }

    #[test]
    fn unicode_latin_one_is_its_own_keysym() {
        assert_eq!(candidates(0x0100_00e9), [Some(0xe9), None, None]); // é
        assert_eq!(candidates(0x0100_0041), [Some(0x41), None, None]); // A
    }

    #[test]
    fn the_euro_sign_names_its_named_keysym_which_carries_the_codepoint() {
        // EuroSign is 0x20ac, the number of U+20AC: the keymap holds that spelling.
        assert_eq!(
            candidates(0x0100_20ac),
            [Some(0x0100_20ac), Some(0x20ac), None]
        );
    }

    #[test]
    fn x11_keysyms_pass_through_unchanged() {
        assert_eq!(candidates(0x61), [Some(0x61), None, None]);
        assert_eq!(candidates(0xffe1), [Some(0xffe1), None, None]); // Shift_L
        assert_eq!(candidates(0x07e1), [Some(0x07e1), None, None]); // Greek_alpha, legacy
        // A bare 0x03a3 is Latin-4's Rcedilla, not Σ: the bare-codepoint Greek is gone.
        assert_eq!(candidates(0x03a3), [Some(0x03a3), None, None]);
        assert_eq!(candidates(0x0100_1f00), [Some(0x0100_1f00), None, None]); // polytonic
    }

    #[test]
    fn a_greek_layout_resolves_unicode_alpha() {
        let km = default_like_map();
        assert_eq!(
            plan(&km, 0x0100_03b1).expect("Greek alpha resolves on a Greek layout"),
            Plan::Key(Step::Direct {
                keysym: 0x07e1,
                keycode: 62,
                shift: false,
                level3: false,
            })
        );
        assert_eq!(
            plan(&km, 0x0100_0391).expect("capital alpha resolves"),
            Plan::Key(Step::Direct {
                keysym: 0x07c1,
                keycode: 62,
                shift: true,
                level3: false,
            })
        );
    }

    #[test]
    fn a_unicode_keysym_resolves_where_the_keymap_lists_it() {
        let km = default_like_map();
        // 'ơ' (U+01A1) is on keycode 63, not on the Latin-4 key that lists XK_Aogonek.
        assert_eq!(
            plan(&km, 0x0100_01a1).expect("U+01A1 resolves"),
            Plan::Key(Step::Direct {
                keysym: 0x0100_01a1,
                keycode: 63,
                shift: false,
                level3: false,
            })
        );
        // The legacy keysym is still a keysym of its own.
        assert_eq!(
            plan(&km, 0x01a1).expect("XK_Aogonek resolves"),
            Plan::Key(Step::Direct {
                keysym: 0x01a1,
                keycode: 64,
                shift: true,
                level3: false,
            })
        );
    }

    #[test]
    fn an_altgr_column_of_a_single_group_key_is_held_when_a_level3_key_exists() {
        // The map's own group repeated in the group columns: its AltGr columns belong to
        // the active group, and the level-3 key reaches them.
        let km = single_group_altgr_map();
        assert_eq!(
            plan(&km, 0xc3).expect("AltGr column 4 with a level3 key"),
            Plan::Key(Step::Direct {
                keysym: 0xc3,
                keycode: 61,
                shift: false,
                level3: true,
            })
        );
        assert_eq!(
            plan(&km, 0xc5).expect("AltGr column 5 with a level3 key"),
            Plan::Key(Step::Direct {
                keysym: 0xc5,
                keycode: 61,
                shift: true,
                level3: true,
            })
        );
    }

    #[test]
    fn a_second_groups_columns_are_rebound_even_with_a_level3_key() {
        // 'æ' sits in a second group's column: no held modifier reaches it while the first
        // group is active, so pressing the key would type another character.
        let km = map_with_level3_key();
        assert_eq!(
            plan(&km, 0xc3).expect("the second group is typed by rebind"),
            Plan::Key(Step::Rebind { keysym: 0xc3 })
        );
    }

    #[test]
    fn an_altgr_column_without_a_level3_key_is_rebound() {
        let km = default_like_map();
        assert_eq!(
            plan(&km, 0xc3).expect("AltGr column without a level3 key"),
            Plan::Key(Step::Rebind { keysym: 0xc3 })
        );
    }

    #[test]
    fn accented_greek_plans_the_layouts_dead_key_sequence() {
        // ά has no column of its own; the map holds dead_acute only at an unreachable
        // column (so the tap rebinds) and alpha at column 0 (so the base is direct).
        let mut flat = vec![];
        for _ in 0..53 {
            flat.extend_from_slice(&[0, 0, 0, 0]);
        }
        // Keycode 61: dead_acute only in column 2 (unreachable); keycode 62: alpha.
        flat.extend_from_slice(&[0, 0, 0xfe51, 0]);
        flat.extend_from_slice(&[0x07e1, 0x07c1, 0, 0]);
        let km = Keymap::from_flat(8, 4, &flat);
        assert_eq!(
            plan(&km, 0x0100_03ac).expect("ά plans a dead-key sequence"),
            Plan::Dead {
                taps: vec![Step::Rebind { keysym: 0xfe51 }],
                base: Step::Direct {
                    keysym: 0x07e1,
                    keycode: 62,
                    shift: false,
                    level3: false,
                }
            }
        );
    }

    #[test]
    fn a_keysym_only_an_unreachable_column_holds_is_rebound() {
        // ά itself sits at an unreachable column and the map has no dead_acute: the rebind
        // types it, rather than the refusal the missing dead keys would otherwise give.
        let mut flat = vec![];
        for _ in 0..53 {
            flat.extend_from_slice(&[0, 0, 0, 0]);
        }
        flat.extend_from_slice(&[0, 0, 0x0100_03ac, 0]); // ά, column 2 only
        flat.extend_from_slice(&[0, 0, 0, 0]);
        let km = Keymap::from_flat(8, 4, &flat);
        assert_eq!(
            plan(&km, 0x0100_03ac).expect("the unreachable column is typed by rebind"),
            Plan::Key(Step::Rebind {
                keysym: 0x0100_03ac
            })
        );
    }

    #[test]
    fn a_character_outside_the_keymap_is_refused_not_mistyped() {
        let km = default_like_map();
        // Fullwidth hyphen-minus: never XK_Return, whatever its low bits say.
        assert!(matches!(
            plan(&km, 0x0100_ff0d),
            Err(BackendError::KeysymUnavailable(0x0100_ff0d))
        ));
        // Accented Greek without its dead key is refused even though the base vowel is
        // on the map: nothing may type a different character for it.
        assert!(matches!(
            plan(&km, 0x0100_03ac),
            Err(BackendError::KeysymUnavailable(0x0100_03ac))
        ));
    }

    #[test]
    fn a_release_finds_the_keycode_or_the_live_rebind() {
        let km = default_like_map();
        assert_eq!(release_keycode(&km, 0x61, &[]), Some(60));
        // The rebind answers when the map cannot: beta is on no key here, but a held
        // rebind carries its legacy keysym on the spare keycode 100.
        assert_eq!(
            release_keycode(&km, 0x0100_03b2, &[(100, 0x07e2)]),
            Some(100)
        );
        assert_eq!(release_keycode(&km, 0x0100_1f00, &[(100, 0x07e2)]), None);
    }

    #[test]
    fn keysyms_are_classified_for_the_modifier_model() {
        assert_eq!(key_kind(0xffe1), KeyKind::Modifier(Modifier::Shift));
        assert_eq!(key_kind(0xffe4), KeyKind::Modifier(Modifier::Control));
        assert_eq!(key_kind(0xffea), KeyKind::Modifier(Modifier::Alt));
        assert_eq!(key_kind(0xffe7), KeyKind::Modifier(Modifier::Other));
        assert_eq!(key_kind(0xffe5), KeyKind::Consumed); // Caps_Lock
        assert_eq!(key_kind(0xfe03), KeyKind::Consumed); // ISO_Level3_Shift
        assert_eq!(key_kind(0x61), KeyKind::Character);
        assert_eq!(key_kind(0x07e1), KeyKind::Character);
        assert_eq!(key_kind(0x0100_20ac), KeyKind::Character);
        assert_eq!(key_kind(0xff0d), KeyKind::Named); // Return
        assert_eq!(key_kind(0xffbe), KeyKind::Named); // F1
    }

    #[test]
    fn shortcut_characters_are_ascii_alphanumerics_and_the_keys_us_symbols() {
        assert!(is_shortcut_character(0x74, Some("KeyT"))); // Ctrl+Alt+T
        assert!(is_shortcut_character(0x5a, Some("KeyY"))); // QWERTZ Z
        assert!(is_shortcut_character(0x2d, Some("Minus"))); // Ctrl+Alt+-
        assert!(is_shortcut_character(0x0100_0031, Some("Digit1")));
        // AltGr products: German AltGr+Q, AltGr+7, AltGr+E; Polish AltGr+A.
        assert!(!is_shortcut_character(0x40, Some("KeyQ")));
        assert!(!is_shortcut_character(0x7b, Some("Digit7")));
        assert!(!is_shortcut_character(0x0100_20ac, Some("KeyE")));
        assert!(!is_shortcut_character(0x0100_0105, Some("KeyA")));
        assert!(!is_shortcut_character(0x40, None));
    }
}
