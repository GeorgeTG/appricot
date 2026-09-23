//! Keysym to keycode translation over the core protocol.
//!
//! No XKB: the backend asks `GetKeyboardMapping` once (and again on `MappingNotify`) and
//! matches a keysym against the per-keycode columns the server reports. Column 0 is the
//! unshifted level and column 1 the shifted one; anything found only in column 2 or later
//! needs AltGr or a second keyboard group, which this translation does not build, so it
//! reports [`BackendError::KeysymUnavailable`](crate::BackendError::KeysymUnavailable)
//! instead of pressing the wrong key. That is the gap the XKB spike measures.
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
//! [`candidates`] names both where this module knows the legacy name: Latin-1 in Unicode
//! form, and the plain Greek letters, whose named keysyms sit at `0x07xx` (the Greek letter
//! assignments of `keysymdef.h` in xorgproto). On the default Xvfb keymap (a US layout) Greek
//! resolves to nothing either way; it needs a Greek layout configured on the server.

use crate::BackendError;

/// `XK_Shift_L` from `keysymdef.h`: the keysym of the physical Shift key.
pub(crate) const XK_SHIFT_L: u32 = 0xffe1;

/// `XK_Caps_Lock` from `keysymdef.h`.
pub(crate) const XK_CAPS_LOCK: u32 = 0xffe5;

/// The first X11 Unicode keysym: `0x01000000 + codepoint`.
const UNICODE_KEYSYM_BASE: u32 = 0x0100_0000;

/// The last X11 Unicode keysym, for U+10FFFF.
const UNICODE_KEYSYM_LAST: u32 = 0x0110_ffff;

/// A keysym resolved against the server's keymap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Resolved {
    /// The keycode to press with XTEST.
    pub keycode: u8,
    /// The column the keysym sits in: 0 unshifted, 1 shifted, 2+ unhandled.
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
}

/// The keysyms an X keymap may list the wire keysym under, best first: at most two.
///
/// - A Unicode keysym for a printable Latin-1 character becomes the Latin-1 keysym, which is
///   the codepoint.
/// - Any other Unicode keysym is itself, then its legacy named keysym when this module knows
///   one (the plain Greek letters).
/// - Everything else is an X11 keysym and is passed through as it is.
pub(crate) fn candidates(keysym: u32) -> [Option<u32>; 2] {
    if !(UNICODE_KEYSYM_BASE..=UNICODE_KEYSYM_LAST).contains(&keysym) {
        return [Some(keysym), None];
    }
    let codepoint = keysym - UNICODE_KEYSYM_BASE;
    if is_printable_latin1(codepoint) {
        return [Some(codepoint), None];
    }
    [Some(keysym), legacy_greek(codepoint)]
}

/// True for the printable Latin-1 codepoints, U+0020-U+007E and U+00A0-U+00FF: their X11
/// keysyms carry the same numbers.
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
/// Accented Greek and the archaic letters are left out on purpose: a Greek layout types them
/// through a dead key, which no single keycode reproduces.
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

/// Turns a wire keysym into a pressable keycode, or explains why it is not.
///
/// Every [`candidates`] entry is tried and the lowest column wins, the first candidate on a
/// tie. Column 1 asks for Shift, which the caller arranges around the key; column 2 or later
/// is refused.
pub(crate) fn resolve_pressable(keymap: &Keymap, keysym: u32) -> Result<Resolved, BackendError> {
    let mut best: Option<Resolved> = None;
    for candidate in candidates(keysym).into_iter().flatten() {
        if let Some(found) = keymap.resolve(candidate)
            && best.is_none_or(|b| found.column < b.column)
        {
            best = Some(found);
        }
    }
    match best {
        Some(resolved) if resolved.column < 2 => Ok(resolved),
        _ => Err(BackendError::KeysymUnavailable(keysym)),
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
        KeyKind, Keymap, Modifier, Resolved, candidates, is_shortcut_character, key_kind,
        resolve_pressable,
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
    fn unicode_greek_names_its_legacy_keysym_as_a_second_choice() {
        assert_eq!(candidates(0x0100_03b1), [Some(0x0100_03b1), Some(0x07e1)]); // α
        assert_eq!(candidates(0x0100_03c1), [Some(0x0100_03c1), Some(0x07f1)]); // ρ
        assert_eq!(candidates(0x0100_03c2), [Some(0x0100_03c2), Some(0x07f3)]); // ς final
        assert_eq!(candidates(0x0100_03c3), [Some(0x0100_03c3), Some(0x07f2)]); // σ
        assert_eq!(candidates(0x0100_03c9), [Some(0x0100_03c9), Some(0x07f9)]); // ω
        assert_eq!(candidates(0x0100_0391), [Some(0x0100_0391), Some(0x07c1)]); // Α
        assert_eq!(candidates(0x0100_03a1), [Some(0x0100_03a1), Some(0x07d1)]); // Ρ
        assert_eq!(candidates(0x0100_03a3), [Some(0x0100_03a3), Some(0x07d2)]); // Σ
        assert_eq!(candidates(0x0100_03a9), [Some(0x0100_03a9), Some(0x07d9)]); // Ω
    }

    #[test]
    fn unicode_latin_one_is_its_own_keysym() {
        assert_eq!(candidates(0x0100_00e9), [Some(0xe9), None]); // é
        assert_eq!(candidates(0x0100_0041), [Some(0x41), None]); // A
    }

    #[test]
    fn x11_keysyms_pass_through_unchanged() {
        assert_eq!(candidates(0x61), [Some(0x61), None]);
        assert_eq!(candidates(0xffe1), [Some(0xffe1), None]); // Shift_L
        assert_eq!(candidates(0x07e1), [Some(0x07e1), None]); // Greek_alpha, legacy
        // A bare 0x03a3 is Latin-4's Rcedilla, not Σ: the bare-codepoint Greek is gone.
        assert_eq!(candidates(0x03a3), [Some(0x03a3), None]);
        assert_eq!(candidates(0x0100_1f00), [Some(0x0100_1f00), None]); // polytonic
    }

    #[test]
    fn a_greek_layout_resolves_unicode_alpha() {
        let km = default_like_map();
        assert_eq!(
            resolve_pressable(&km, 0x0100_03b1).expect("Greek alpha resolves on a Greek layout"),
            Resolved {
                keycode: 62,
                column: 0
            }
        );
        assert_eq!(
            resolve_pressable(&km, 0x0100_0391).expect("capital alpha resolves"),
            Resolved {
                keycode: 62,
                column: 1
            }
        );
    }

    #[test]
    fn a_unicode_keysym_resolves_where_the_keymap_lists_it() {
        let km = default_like_map();
        // 'ơ' (U+01A1) is on keycode 63, not on the Latin-4 key that lists XK_Aogonek.
        assert_eq!(
            resolve_pressable(&km, 0x0100_01a1).expect("U+01A1 resolves"),
            Resolved {
                keycode: 63,
                column: 0
            }
        );
        // The legacy keysym is still a keysym of its own.
        assert_eq!(
            resolve_pressable(&km, 0x01a1).expect("XK_Aogonek resolves"),
            Resolved {
                keycode: 64,
                column: 1
            }
        );
    }

    #[test]
    fn a_character_outside_the_keymap_is_refused_not_mistyped() {
        let km = default_like_map();
        // Fullwidth hyphen-minus: never XK_Return, whatever its low bits say.
        assert!(matches!(
            resolve_pressable(&km, 0x0100_ff0d),
            Err(BackendError::KeysymUnavailable(0x0100_ff0d))
        ));
    }

    #[test]
    fn altgr_columns_are_refused() {
        let km = default_like_map();
        assert!(matches!(
            resolve_pressable(&km, 0xc3),
            Err(BackendError::KeysymUnavailable(0xc3))
        ));
    }

    #[test]
    fn absent_keysyms_are_refused() {
        let km = default_like_map();
        assert!(matches!(
            resolve_pressable(&km, 0x0100_1f00),
            Err(BackendError::KeysymUnavailable(0x0100_1f00))
        ));
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
