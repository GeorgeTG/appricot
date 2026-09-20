//! What the backend holds down: keys, their Shift companions, and pointer buttons.
//!
//! XTEST injects raw keycodes, so a "press keysym A" becomes "press Shift, press keycode
//! 38" and the release must come off in the mirror order. This module is only the
//! bookkeeping — which presses are outstanding and what to send to undo them — so that
//! [`InputSink::blur`](appricot_core::InputSink::blur) can leave nothing stuck down even
//! if the host lost count.

use std::collections::BTreeMap;

/// Outstanding presses, keyed by what XTEST sent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct HeldInput {
    /// Keycode to presses still held.
    keys: BTreeMap<u8, u32>,
    /// How many of the held keys needed Shift.
    shift_users: u32,
    /// Buttons (X button numbers) to presses still held.
    buttons: BTreeMap<u8, u32>,
}

/// What to send to XTEST for one logical action: one or two keycode actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct VecPresses {
    /// The first keycode, if any.
    pub first: Option<u8>,
    /// The second keycode, when Shift wraps a key press.
    pub second: Option<u8>,
}

impl VecPresses {
    /// The keycodes in order.
    pub(crate) fn as_slice(self) -> [Option<u8>; 2] {
        [self.first, self.second]
    }

    fn from_one(keycode: u8) -> Self {
        Self {
            first: Some(keycode),
            second: None,
        }
    }

    fn from_pair(shift: u8, keycode: u8) -> Self {
        Self {
            first: Some(shift),
            second: Some(keycode),
        }
    }
}

impl HeldInput {
    /// An empty tally.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Records a key press and returns the keycodes to press: Shift first when this press
    /// is the first that needs it.
    pub(crate) fn key_press(
        &mut self,
        keycode: u8,
        needs_shift: bool,
        shift_keycode: u8,
    ) -> VecPresses {
        *self.keys.entry(keycode).or_insert(0) += 1;
        if needs_shift {
            self.shift_users += 1;
            if self.shift_users == 1 {
                *self.keys.entry(shift_keycode).or_insert(0) += 1;
                return VecPresses::from_pair(shift_keycode, keycode);
            }
        }
        VecPresses::from_one(keycode)
    }

    /// Records a key release and returns the keycodes to release: the key first, Shift
    /// after it when this was the last press that needed it.
    ///
    /// A release with no outstanding press releases nothing: the host may replay a release
    /// it never paired.
    pub(crate) fn key_release(
        &mut self,
        keycode: u8,
        needs_shift: bool,
        shift_keycode: u8,
    ) -> VecPresses {
        let count = self.keys.get_mut(&keycode).filter(|c| **c > 0);
        if let Some(count) = count {
            *count -= 1;
        } else {
            return VecPresses::default();
        }
        if needs_shift && self.shift_users > 0 {
            self.shift_users -= 1;
            if self.shift_users == 0 {
                if let Some(count) = self.keys.get_mut(&shift_keycode).filter(|c| **c > 0) {
                    *count -= 1;
                }
                return VecPresses::from_pair(keycode, shift_keycode);
            }
        }
        VecPresses::from_one(keycode)
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

    /// Everything to release on blur: one release per outstanding press, keys before
    /// buttons. The keycodes come out in numeric order, which mirrors how they went down
    /// (Shift pressed before the letter is released after it, since Shift's keycode sits
    /// above the letters').
    pub(crate) fn release_all(&mut self) -> (Vec<u8>, Vec<u8>) {
        let mut keys = Vec::new();
        for (&keycode, &count) in &self.keys {
            for _ in 0..count {
                keys.push(keycode);
            }
        }
        keys.sort_unstable();
        let mut buttons = Vec::new();
        for (&button, &count) in &self.buttons {
            for _ in 0..count {
                buttons.push(button);
            }
        }
        self.keys.clear();
        self.shift_users = 0;
        self.buttons.clear();
        (keys, buttons)
    }

    /// True when nothing is held.
    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.keys.values().all(|c| *c == 0) && self.buttons.values().all(|c| *c == 0)
    }
}

#[cfg(test)]
mod tests {
    use super::HeldInput;

    const KEY_A: u8 = 38;
    const SHIFT: u8 = 50;

    #[test]
    fn a_plain_press_is_just_the_key() {
        let mut held = HeldInput::new();
        assert_eq!(
            held.key_press(KEY_A, false, SHIFT).as_slice(),
            [Some(KEY_A), None]
        );
        assert!(!held.is_empty());
    }

    #[test]
    fn the_first_shifted_press_picks_up_shift() {
        let mut held = HeldInput::new();
        assert_eq!(
            held.key_press(KEY_A, true, SHIFT).as_slice(),
            [Some(SHIFT), Some(KEY_A)]
        );
    }

    #[test]
    fn the_second_shifted_press_reuses_the_held_shift() {
        let mut held = HeldInput::new();
        held.key_press(KEY_A, true, SHIFT);
        assert_eq!(held.key_press(24, true, SHIFT).as_slice(), [Some(24), None]);
    }

    #[test]
    fn the_last_shifted_release_puts_shift_back_down_first() {
        let mut held = HeldInput::new();
        held.key_press(KEY_A, true, SHIFT);
        assert_eq!(
            held.key_release(KEY_A, true, SHIFT).as_slice(),
            [Some(KEY_A), Some(SHIFT)]
        );
        assert!(held.is_empty());
    }

    #[test]
    fn an_unpaired_release_releases_nothing() {
        let mut held = HeldInput::new();
        assert_eq!(
            held.key_release(KEY_A, false, SHIFT).as_slice(),
            [None, None]
        );
        held.button_press(1);
        held.button_release(3);
        held.button_release(1);
        assert!(held.is_empty());
    }

    #[test]
    fn blur_releases_every_outstanding_press_once_per_press() {
        let mut held = HeldInput::new();
        held.key_press(KEY_A, true, SHIFT);
        held.key_press(KEY_A, true, SHIFT);
        held.button_press(1);
        held.button_press(3);
        held.button_press(3);
        let (keys, buttons) = held.release_all();
        // Shift went down once for both shifted presses, so it comes up once.
        assert_eq!(keys, vec![KEY_A, KEY_A, SHIFT]);
        assert_eq!(buttons, vec![1, 3, 3]);
        assert!(held.is_empty());
    }
}
