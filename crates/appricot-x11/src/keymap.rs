//! Keysym to keycode translation over the core protocol.
//!
//! No XKB: the backend asks `GetKeyboardMapping` once (and again on `MappingNotify`) and
//! matches a keysym against the per-keycode columns the server reports. Column 0 is the
//! unshifted level and column 1 the shifted one; anything found only in column 2 or later
//! needs AltGr or a second keyboard group, which this translation does not build, so it
//! reports [`BackendError::KeysymUnavailable`](crate::BackendError::KeysymUnavailable)
//! instead of pressing the wrong key. That is the gap the XKB spike measures.
//!
//! # Two keysym conventions
//!
//! The wire protocol carries printable characters as Unicode codepoints and everything else
//! as X11 keysyms. The two agree everywhere below 0x100 (Latin-1). They disagree on Greek:
//! the wire sends Unicode (Greek alpha 0x03b1) while the X server's keymap lists the X11
//! Greek keysyms at 0x07xx. [`to_x11_keysym`] bridges exactly that block, nothing more; the
//! numbers are the Greek letter assignments of `keysymdef.h` in xorgproto. On the default
//! Xvfb keymap (a US layout) Greek resolves to nothing either way — it needs a Greek layout
//! configured on the server; the bridge makes such a layout work.

use crate::BackendError;

/// `XK_Shift_L` from `keysymdef.h`: the keysym of the physical Shift key.
pub(crate) const XK_SHIFT_L: u32 = 0xffe1;

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
    /// `keysym` must already be an X11 keysym: callers pass
    /// [`to_x11_keysym`](self::to_x11_keysym) output. A zero keysym (`NoSymbol`) resolves to
    /// nothing.
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

    /// The keycode of the left Shift key, which [`HeldInput`](crate) presses when a keysym
    /// needs the shifted column.
    pub(crate) fn shift_keycode(&self) -> Option<u8> {
        self.resolve(XK_SHIFT_L).map(|r| r.keycode)
    }
}

/// Maps a wire keysym (Unicode for printable characters, X11 for the rest) to the keysym the
/// X server's keymap lists.
///
/// Everything but the Unicode Greek and Coptic block is passed through: Latin-1 codepoints
/// are their own X11 keysyms, and a client that already sends X11 keysyms (0x07xx Greek
/// included) is understood as-is. The Greek bridge covers the plain letters of
/// `keysymdef.h`:
///
/// - Unicode 0x0391-0x03a1 (Α..Ρ) map to X11 0x07c1-0x07d1,
/// - Unicode 0x03a3-0x03a9 (Σ..Ω) map to X11 0x07d2, 0x07d4-0x07d9,
/// - Unicode 0x03b1-0x03c1 (α..ρ) map to X11 0x07e1-0x07f1,
/// - Unicode 0x03c2 (ς) maps to X11 0x07f3, Unicode 0x03c3 (σ) to X11 0x07f2, and
///   0x03c4-0x03c9 (τ..ω) map to X11 0x07f4-0x07f9.
///
/// Accented Greek and the archaic letters are left alone on purpose: the mapping stays
/// small, and the spike decides whether they are worth it.
pub(crate) fn to_x11_keysym(keysym: u32) -> u32 {
    // Uppercase Α..Ρ, no gap.
    if (0x0391..=0x03a1).contains(&keysym) {
        return 0x07c1 + (keysym - 0x0391);
    }
    // Uppercase Σ..Ω; Σ is 0x07d2, Τ..Ω are 0x07d4..0x07d9 (0x07d3 is not assigned).
    if (0x03a3..=0x03a9).contains(&keysym) {
        if keysym == 0x03a3 {
            return 0x07d2;
        }
        return 0x07d4 + (keysym - 0x03a4);
    }
    // Lowercase α..ρ, no gap.
    if (0x03b1..=0x03c1).contains(&keysym) {
        return 0x07e1 + (keysym - 0x03b1);
    }
    // Lowercase σ, ς, τ..ω: σ is 0x07f2, ς is 0x07f3, τ..ω are 0x07f4..0x07f9.
    if (0x03c2..=0x03c9).contains(&keysym) {
        if keysym == 0x03c2 {
            return 0x07f3;
        }
        if keysym == 0x03c3 {
            return 0x07f2;
        }
        return 0x07f4 + (keysym - 0x03c4);
    }
    keysym
}

/// Turns a wire keysym into a pressable keycode, or explains why it is not.
///
/// Column 1 asks for Shift, which the caller presses around the key
/// (see [`HeldInput`](crate)); column 2 or later is refused.
pub(crate) fn resolve_pressable(keymap: &Keymap, keysym: u32) -> Result<Resolved, BackendError> {
    let x11 = to_x11_keysym(keysym);
    let resolved = keymap
        .resolve(x11)
        .ok_or(BackendError::KeysymUnavailable(keysym))?;
    if resolved.column >= 2 {
        return Err(BackendError::KeysymUnavailable(keysym));
    }
    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::{Keymap, Resolved, resolve_pressable, to_x11_keysym};
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
    fn greek_unicode_bridges_to_the_x11_keysym() {
        assert_eq!(to_x11_keysym(0x03b1), 0x07e1); // α
        assert_eq!(to_x11_keysym(0x03c1), 0x07f1); // ρ
        assert_eq!(to_x11_keysym(0x03c2), 0x07f3); // ς final
        assert_eq!(to_x11_keysym(0x03c3), 0x07f2); // σ
        assert_eq!(to_x11_keysym(0x03c9), 0x07f9); // ω
        assert_eq!(to_x11_keysym(0x0391), 0x07c1); // Α
        assert_eq!(to_x11_keysym(0x03a1), 0x07d1); // Ρ
        assert_eq!(to_x11_keysym(0x03a3), 0x07d2); // Σ
        assert_eq!(to_x11_keysym(0x03a9), 0x07d9); // Ω
    }

    #[test]
    fn non_greek_keysyms_pass_through_unchanged() {
        assert_eq!(to_x11_keysym(0x61), 0x61);
        assert_eq!(to_x11_keysym(0x0386), 0x0386); // accented Greek: deliberately unmapped
        assert_eq!(to_x11_keysym(0x1f00), 0x1f00); // polytonic: unmapped
        assert_eq!(to_x11_keysym(0xffe1), 0xffe1); // already an X11 keysym
        assert_eq!(to_x11_keysym(0x07e1), 0x07e1); // already an X11 Greek keysym
    }

    #[test]
    fn a_greek_layout_resolves_unicode_alpha() {
        let km = default_like_map();
        assert_eq!(
            resolve_pressable(&km, 0x03b1).expect("Greek alpha resolves on a Greek layout"),
            Resolved {
                keycode: 62,
                column: 0
            }
        );
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
            resolve_pressable(&km, 0x1f00),
            Err(BackendError::KeysymUnavailable(0x1f00))
        ));
    }
}
