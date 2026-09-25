//! What the backend holds down, and the XTEST strokes one key press takes.
//!
//! XTEST injects raw keycodes, and the X server applies whatever modifiers are down to them.
//! The wire's keysym already carries the user's Shift, Caps Lock and AltGr (the browser
//! applied them to `KeyboardEvent.key`), so the modifiers must count once, not twice. The
//! model, keysym first:
//!
//! - Caps Lock, Shift Lock and the AltGr level shifts are consumed, never forwarded.
//! - Shift, Control, Alt, Meta and Super keys are forwarded and remembered as held.
//! - A character is text by default. For its press alone, the Shift keys come up or one goes
//!   down so that X's Shift matches the column the keysym sits in, and are put back right
//!   after.
//! - Under Alt, a character that is not a shortcut character (see
//!   [`is_shortcut_character`]) is what the browser made of AltGr: Control and Alt come up
//!   around its press too. A browser on Windows reports AltGr as Control plus `AltRight`.
//! - A shortcut character under Control, Alt, Meta or Super, and any named key, keeps the
//!   user's modifiers as they are; Shift is added only when the key's column needs it and
//!   none is held.
//!
//! Keys are remembered by their physical code, so a release comes off the keycode its press
//! went down on, even when the keysym changed in between (German Shift+7 presses `/` and may
//! be released as `7`). [`HeldInput::release_all`] is what
//! [`InputSink::blur`](appricot_core::InputSink::blur) sends to leave nothing stuck down.

use std::collections::BTreeMap;

use crate::keymap::{KeyKind, Modifier, is_shortcut_character};

/// One XTEST key event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stroke {
    /// The keycode goes down.
    Down(u8),
    /// The keycode comes up.
    Up(u8),
}

/// The physical key a press came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum KeyId {
    /// The browser's `KeyboardEvent.code`.
    Code(String),
    /// The keycode the keysym resolved to, when the client named no usable code.
    Keycode(u8),
}

impl KeyId {
    /// The code the client sent, unless it says nothing: `Unidentified` names no key.
    pub(crate) fn usable_code(code: Option<&str>) -> Option<&str> {
        code.filter(|c| !c.is_empty() && *c != "Unidentified")
    }
}

/// A key the backend pressed and has not released.
#[derive(Debug, Clone, PartialEq, Eq)]
struct HeldKey {
    id: KeyId,
    keycode: u8,
    /// The modifier class, for a modifier key.
    modifier: Option<Modifier>,
}

/// One press of a character or named key, resolved against the keymap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct KeyPress<'a> {
    /// The wire keysym.
    pub keysym: u32,
    /// The physical code, when usable.
    pub code: Option<&'a str>,
    /// [`KeyKind::Character`] or [`KeyKind::Named`].
    pub kind: KeyKind,
    /// The keycode to press.
    pub keycode: u8,
    /// True when the keysym sits in a shifted column and needs Shift.
    pub shifted: bool,
    /// The level-3 shift keycode to hold around the press, when the keysym sits in a
    /// column the first two do not name (an AltGr level or a further group).
    pub level3: Option<u8>,
}

/// A press needs Shift and the keymap has no Shift key to press.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NoShiftKey;

/// Outstanding presses: keys in press order, and buttons by X button number.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct HeldInput {
    keys: Vec<HeldKey>,
    /// Buttons (X button numbers) to presses still held.
    buttons: BTreeMap<u8, u32>,
}

impl HeldInput {
    /// An empty tally.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// True while a key of this modifier class is held.
    pub(crate) fn holds(&self, modifier: Modifier) -> bool {
        self.keys.iter().any(|k| k.modifier == Some(modifier))
    }

    /// The keycodes held for a modifier class, each once, in press order.
    fn modifier_keycodes(&self, modifiers: &[Modifier]) -> Vec<u8> {
        let mut out = Vec::new();
        for key in &self.keys {
            if key.modifier.is_some_and(|m| modifiers.contains(&m)) && !out.contains(&key.keycode) {
                out.push(key.keycode);
            }
        }
        out
    }

    /// True while the physical key `id` is held.
    fn is_held(&self, id: &KeyId) -> bool {
        self.keys.iter().any(|k| k.id == *id)
    }

    /// True while some held key is down on `keycode`: two keys can share one, and a
    /// keycode nobody holds is where a rebind may end.
    pub(crate) fn holds_keycode(&self, keycode: u8) -> bool {
        self.keys.iter().any(|k| k.keycode == keycode)
    }

    /// Records a modifier key press and returns what to send: the keycode down, or nothing
    /// when that physical key is already down.
    pub(crate) fn press_modifier(
        &mut self,
        id: KeyId,
        keycode: u8,
        modifier: Modifier,
    ) -> Vec<Stroke> {
        if self.is_held(&id) {
            return Vec::new();
        }
        self.keys.push(HeldKey {
            id,
            keycode,
            modifier: Some(modifier),
        });
        vec![Stroke::Down(keycode)]
    }

    /// Records a character or named key press and returns the strokes that type it, with the
    /// modifier state set for this press alone (see the module docs). A physical key that is
    /// already down sends nothing.
    ///
    /// `shift_keycode` is the keycode to press when the key needs a Shift nobody holds.
    pub(crate) fn press_key(
        &mut self,
        id: KeyId,
        press: KeyPress<'_>,
        shift_keycode: Option<u8>,
    ) -> Result<Vec<Stroke>, NoShiftKey> {
        if self.is_held(&id) {
            return Ok(Vec::new());
        }
        let shift_held = self.holds(Modifier::Shift);
        let under_modifier = self.holds(Modifier::Control)
            || self.holds(Modifier::Alt)
            || self.holds(Modifier::Other);
        let text = press.kind == KeyKind::Character
            && !(under_modifier && is_shortcut_character(press.keysym, press.code));

        let mut lift = Vec::new();
        if text && self.holds(Modifier::Alt) {
            // AltGr already sits in the keysym.
            lift = self.modifier_keycodes(&[Modifier::Control, Modifier::Alt]);
        }
        let mut add = None;
        if press.shifted && !shift_held {
            add = Some(shift_keycode.ok_or(NoShiftKey)?);
        } else if text && !press.shifted && shift_held {
            lift.extend(self.modifier_keycodes(&[Modifier::Shift]));
        }

        // The level-3 key brackets the press like the added Shift does: down before,
        // up right after. It is never one of the wire's held keys (the level shifts are
        // consumed), so the bracket never collides with the tally.
        let level3 = press.level3;
        let mut strokes: Vec<Stroke> = lift.iter().map(|k| Stroke::Up(*k)).collect();
        strokes.extend(level3.map(Stroke::Down));
        strokes.extend(add.map(Stroke::Down));
        strokes.push(Stroke::Down(press.keycode));
        strokes.extend(add.map(Stroke::Up));
        strokes.extend(level3.map(Stroke::Up));
        strokes.extend(lift.iter().rev().map(|k| Stroke::Down(*k)));
        self.keys.push(HeldKey {
            id,
            keycode: press.keycode,
            modifier: None,
        });
        Ok(strokes)
    }

    /// Records the release of physical key `id` and returns what to send: its keycode up,
    /// unless another held key still holds that keycode down. A release with no press
    /// releases nothing: the host may replay a release it never paired.
    pub(crate) fn release_key(&mut self, id: &KeyId) -> Vec<Stroke> {
        let Some(index) = self.keys.iter().position(|k| k.id == *id) else {
            return Vec::new();
        };
        let keycode = self.keys.remove(index).keycode;
        if self.keys.iter().any(|k| k.keycode == keycode) {
            return Vec::new();
        }
        vec![Stroke::Up(keycode)]
    }

    /// Records a button press (a wheel step is a press followed at once by a release; it
    /// leaves nothing held).
    pub(crate) fn button_press(&mut self, button: u8) {
        *self.buttons.entry(button).or_insert(0) += 1;
    }

    /// Records a button release; unpaired releases are ignored.
    pub(crate) fn button_release(&mut self, button: u8) {
        if let Some(count) = self.buttons.get_mut(&button).filter(|c| **c > 0) {
            *count -= 1;
        }
    }

    /// Everything to release on blur, and forgets it: every held keycode once, the other
    /// keys before the modifiers and each group newest first, then every held button once.
    pub(crate) fn release_all(&mut self) -> (Vec<u8>, Vec<u8>) {
        let mut keys = Vec::new();
        let plain = self.keys.iter().rev().filter(|k| k.modifier.is_none());
        let modifiers = self.keys.iter().rev().filter(|k| k.modifier.is_some());
        for key in plain.chain(modifiers) {
            if !keys.contains(&key.keycode) {
                keys.push(key.keycode);
            }
        }
        let buttons = self
            .buttons
            .iter()
            .filter(|(_, count)| **count > 0)
            .map(|(button, _)| *button)
            .collect();
        self.keys.clear();
        self.buttons.clear();
        (keys, buttons)
    }

    /// True when nothing is held.
    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.keys.is_empty() && self.buttons.values().all(|c| *c == 0)
    }
}

#[cfg(test)]
mod tests {
    use super::{HeldInput, KeyId, KeyPress, NoShiftKey, Stroke};
    use crate::keymap::{KeyKind, Modifier};

    // Keycodes of the evdev US layout Xvfb starts with.
    const KEY_A: u8 = 38;
    const KEY_2: u8 = 11;
    const KEY_SLASH: u8 = 61;
    const KEY_END: u8 = 115;
    const SHIFT_L: u8 = 50;
    const SHIFT_R: u8 = 62;
    const CONTROL_L: u8 = 37;
    const ALT_R: u8 = 108;

    fn code(name: &str) -> KeyId {
        KeyId::Code(name.to_owned())
    }

    fn character(keysym: u32, code: &str, keycode: u8, shifted: bool) -> KeyPress<'_> {
        KeyPress {
            keysym,
            code: Some(code),
            kind: KeyKind::Character,
            keycode,
            shifted,
            level3: None,
        }
    }

    fn press(held: &mut HeldInput, id: &str, key: KeyPress<'_>) -> Vec<Stroke> {
        held.press_key(code(id), key, Some(SHIFT_L))
            .expect("a Shift key exists")
    }

    #[test]
    fn a_plain_press_is_just_the_key() {
        let mut held = HeldInput::new();
        assert_eq!(
            press(&mut held, "KeyA", character(0x61, "KeyA", KEY_A, false)),
            [Stroke::Down(KEY_A)]
        );
        assert!(!held.is_empty());
        assert_eq!(held.release_key(&code("KeyA")), [Stroke::Up(KEY_A)]);
        assert!(held.is_empty());
    }

    #[test]
    fn a_shifted_character_wraps_its_press_in_shift() {
        let mut held = HeldInput::new();
        assert_eq!(
            press(&mut held, "KeyA", character(0x41, "KeyA", KEY_A, true)),
            [
                Stroke::Down(SHIFT_L),
                Stroke::Down(KEY_A),
                Stroke::Up(SHIFT_L)
            ]
        );
        // Shift is already up: the release is the key alone.
        assert_eq!(held.release_key(&code("KeyA")), [Stroke::Up(KEY_A)]);
    }

    #[test]
    fn a_shifted_character_under_a_held_shift_adds_nothing_and_leaves_it_down() {
        let mut held = HeldInput::new();
        held.press_modifier(code("ShiftLeft"), SHIFT_L, Modifier::Shift);
        assert_eq!(
            press(&mut held, "KeyA", character(0x41, "KeyA", KEY_A, true)),
            [Stroke::Down(KEY_A)]
        );
        assert_eq!(held.release_key(&code("KeyA")), [Stroke::Up(KEY_A)]);
        // Shift+End still selects: nothing released the user's Shift.
        assert!(held.holds(Modifier::Shift));
        let end = KeyPress {
            keysym: 0xff57,
            code: Some("End"),
            kind: KeyKind::Named,
            keycode: KEY_END,
            shifted: false,
            level3: None,
        };
        assert_eq!(press(&mut held, "End", end), [Stroke::Down(KEY_END)]);
    }

    #[test]
    fn a_held_shift_is_lifted_for_an_unshifted_character() {
        // German Shift+7 is '/', which the US keymap has unshifted.
        let mut held = HeldInput::new();
        held.press_modifier(code("ShiftLeft"), SHIFT_L, Modifier::Shift);
        held.press_modifier(code("ShiftRight"), SHIFT_R, Modifier::Shift);
        assert_eq!(
            press(
                &mut held,
                "Digit7",
                character(0x2f, "Digit7", KEY_SLASH, false)
            ),
            [
                Stroke::Up(SHIFT_L),
                Stroke::Up(SHIFT_R),
                Stroke::Down(KEY_SLASH),
                Stroke::Down(SHIFT_R),
                Stroke::Down(SHIFT_L),
            ]
        );
        // The release comes off the keycode the press went down on, whatever keysym the
        // browser reports on the way up.
        assert_eq!(held.release_key(&code("Digit7")), [Stroke::Up(KEY_SLASH)]);
    }

    #[test]
    fn caps_lock_text_arrives_cased_and_is_typed_as_it_arrives() {
        // Caps Lock is consumed, so X's Lock stays off: 'A' is Shift+a, and Shift+'a' (the
        // browser's answer to Shift under Caps Lock) is a with Shift lifted.
        let mut held = HeldInput::new();
        assert_eq!(
            press(&mut held, "KeyA", character(0x41, "KeyA", KEY_A, true)),
            [
                Stroke::Down(SHIFT_L),
                Stroke::Down(KEY_A),
                Stroke::Up(SHIFT_L)
            ]
        );
        held.release_key(&code("KeyA"));
        held.press_modifier(code("ShiftLeft"), SHIFT_L, Modifier::Shift);
        assert_eq!(
            press(&mut held, "KeyA", character(0x61, "KeyA", KEY_A, false)),
            [
                Stroke::Up(SHIFT_L),
                Stroke::Down(KEY_A),
                Stroke::Down(SHIFT_L)
            ]
        );
    }

    #[test]
    fn windows_altgr_lifts_control_and_alt_around_the_character() {
        // AltGr+Q on a German layout: ControlLeft, AltRight, then '@' from KeyQ.
        let mut held = HeldInput::new();
        held.press_modifier(code("ControlLeft"), CONTROL_L, Modifier::Control);
        held.press_modifier(code("AltRight"), ALT_R, Modifier::Alt);
        assert_eq!(
            press(&mut held, "KeyQ", character(0x40, "KeyQ", KEY_2, true)),
            [
                Stroke::Up(CONTROL_L),
                Stroke::Up(ALT_R),
                Stroke::Down(SHIFT_L),
                Stroke::Down(KEY_2),
                Stroke::Up(SHIFT_L),
                Stroke::Down(ALT_R),
                Stroke::Down(CONTROL_L),
            ]
        );
    }

    #[test]
    fn a_shortcut_keeps_every_modifier_as_held() {
        let mut held = HeldInput::new();
        held.press_modifier(code("ControlLeft"), CONTROL_L, Modifier::Control);
        held.press_modifier(code("ShiftLeft"), SHIFT_L, Modifier::Shift);
        // Ctrl+Shift+A under Caps Lock arrives as 'a': still Ctrl+Shift+A.
        assert_eq!(
            press(&mut held, "KeyA", character(0x61, "KeyA", KEY_A, false)),
            [Stroke::Down(KEY_A)]
        );
        held.release_key(&code("KeyA"));
        held.press_modifier(code("AltRight"), ALT_R, Modifier::Alt);
        // Ctrl+Alt+Shift+2 on a US layout is a shortcut too: '@' is KeyQ's AltGr product
        // only when it comes from KeyQ.
        assert_eq!(
            press(&mut held, "Digit2", character(0x40, "Digit2", KEY_2, true)),
            [Stroke::Down(KEY_2)]
        );
    }

    #[test]
    fn an_altgr_column_holds_the_level_three_key_around_the_press() {
        // The Euro sign on a Greek layout: level 3 of the E key, reached by holding the
        // map's ISO_Level3_Shift key (an AltGr) around the press.
        let mut held = HeldInput::new();
        assert_eq!(
            press(
                &mut held,
                "KeyE",
                KeyPress {
                    keysym: 0x0100_20ac,
                    code: Some("KeyE"),
                    kind: KeyKind::Character,
                    keycode: KEY_2,
                    shifted: false,
                    level3: Some(ALT_R),
                }
            ),
            [Stroke::Down(ALT_R), Stroke::Down(KEY_2), Stroke::Up(ALT_R)]
        );
        assert_eq!(held.release_key(&code("KeyE")), [Stroke::Up(KEY_2)]);
    }

    #[test]
    fn a_missing_shift_key_refuses_the_press_and_records_nothing() {
        let mut held = HeldInput::new();
        assert_eq!(
            held.press_key(code("KeyA"), character(0x41, "KeyA", KEY_A, true), None),
            Err(NoShiftKey)
        );
        assert!(held.is_empty());
    }

    #[test]
    fn a_repeated_press_of_a_held_key_sends_nothing() {
        let mut held = HeldInput::new();
        press(&mut held, "KeyA", character(0x61, "KeyA", KEY_A, false));
        assert!(press(&mut held, "KeyA", character(0x61, "KeyA", KEY_A, false)).is_empty());
        assert_eq!(
            held.press_modifier(code("ShiftLeft"), SHIFT_L, Modifier::Shift),
            [Stroke::Down(SHIFT_L)]
        );
        assert!(
            held.press_modifier(code("ShiftLeft"), SHIFT_L, Modifier::Shift)
                .is_empty()
        );
    }

    #[test]
    fn a_keycode_two_keys_hold_comes_up_with_the_last() {
        // Digit1 and Numpad1 both type '1' on keycode 10.
        let mut held = HeldInput::new();
        press(&mut held, "Digit1", character(0x31, "Digit1", 10, false));
        press(&mut held, "Numpad1", character(0x31, "Numpad1", 10, false));
        assert!(held.release_key(&code("Digit1")).is_empty());
        assert_eq!(held.release_key(&code("Numpad1")), [Stroke::Up(10)]);
    }

    #[test]
    fn an_unpaired_release_releases_nothing() {
        let mut held = HeldInput::new();
        assert!(held.release_key(&code("KeyA")).is_empty());
        held.button_press(1);
        held.button_release(3);
        held.button_release(1);
        assert!(held.is_empty());
    }

    #[test]
    fn blur_releases_every_held_key_and_button_once() {
        let mut held = HeldInput::new();
        held.press_modifier(code("ShiftLeft"), SHIFT_L, Modifier::Shift);
        press(&mut held, "KeyA", character(0x41, "KeyA", KEY_A, true));
        held.press_modifier(code("ControlLeft"), CONTROL_L, Modifier::Control);
        press(&mut held, "Digit2", character(0x32, "Digit2", KEY_2, false));
        held.button_press(1);
        held.button_press(3);
        held.button_press(3);
        let (keys, buttons) = held.release_all();
        // The other keys newest first, then the modifiers newest first.
        assert_eq!(keys, vec![KEY_2, KEY_A, CONTROL_L, SHIFT_L]);
        assert_eq!(buttons, vec![1, 3]);
        assert!(held.is_empty());
    }
}
