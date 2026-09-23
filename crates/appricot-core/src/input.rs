//! The input side of a backend: how the host's input and window requests reach the app.
//!
//! FROZEN INTERFACE of implementation wave 1. `appricot-x11` implements [`InputSink`] on
//! X11; `appricot-streamer` drives it. Signatures may not change without every implementor
//! and every caller changing in the same change.

use crate::geometry::{Point, Size};
use crate::surface::SurfaceId;

/// A key, as an X11 keysym number. The client computes it from the browser's key event;
/// the mapping is fixed by the wire spec, which the task l1-wire-spec-v0 writes into
/// `docs/protocol/`. Until then the mapping here is provisional.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Keysym(pub u32);

/// A physical key name: the browser's `KeyboardEvent.code` (for example `KeyA`, `ShiftLeft`,
/// `Numpad4`), as the wire carries it. ASCII, non-empty, at most 16 bytes.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct KeyCode(String);

impl KeyCode {
    /// The most bytes a key code may carry: the wire's `MAX_KEY_CODE_BYTES`.
    pub const MAX_BYTES: usize = appricot_proto::limits::MAX_KEY_CODE_BYTES;

    /// Wraps `code` when it is ASCII, non-empty and short enough, else `None`.
    pub fn new(code: &str) -> Option<Self> {
        if code.is_empty() || code.len() > Self::MAX_BYTES || !code.is_ascii() {
            return None;
        }
        Some(Self(code.to_owned()))
    }

    /// The key code.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A key event: a keysym, the physical key that produced it when the client knows it, and
/// where in the press/release cycle it is. The keysym is authoritative for characters; the
/// code is what non-printing keys and shortcuts are matched on.
///
/// Unlike its neighbours this is `Clone` but not `Copy`: [`KeyCode`] owns its bytes.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct KeyEvent {
    /// The keysym.
    pub keysym: Keysym,
    /// The physical key, when the client sent one.
    pub code: Option<KeyCode>,
    /// Down or up.
    pub state: PressState,
}

/// A pointer button.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PointerButton {
    /// The primary button.
    Left,
    /// The middle button. Wheel motion is not a button here; it goes to
    /// [`InputSink::pointer_axis`].
    Middle,
    /// The secondary button.
    Right,
}

/// Whether a key or a button went down or came up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PressState {
    /// It went down.
    Pressed,
    /// It came up.
    Released,
}

/// Delivers the host's input and window requests to the app.
///
/// Coordinates are local to a surface; the backend turns them into its own. Only the host
/// decides focus: a backend never moves focus because the app asked (ADR-0003 §5) — it
/// reports the ask through [`SurfaceEvent::FocusRequested`](crate::SurfaceEvent) instead.
/// A backend remembers what it holds down, and [`InputSink::blur`] releases all of it.
pub trait InputSink {
    /// What can go wrong delivering input.
    type Error: std::error::Error + Send + Sync + 'static;

    /// Moves the pointer to `at` inside surface `id`.
    fn pointer_motion(&mut self, id: SurfaceId, at: Point) -> Result<(), Self::Error>;

    /// Presses or releases a pointer button over surface `id`.
    fn pointer_button(
        &mut self,
        id: SurfaceId,
        button: PointerButton,
        state: PressState,
    ) -> Result<(), Self::Error>;

    /// Scrolls by whole steps inside surface `id`. `steps` is `(horizontal, vertical)`;
    /// negative is left and up, positive is right and down.
    fn pointer_axis(&mut self, id: SurfaceId, steps: Point) -> Result<(), Self::Error>;

    /// Presses or releases a key in the focused surface.
    fn key(&mut self, key: KeyEvent) -> Result<(), Self::Error>;

    /// Gives surface `id` the keyboard focus.
    fn focus(&mut self, id: SurfaceId) -> Result<(), Self::Error>;

    /// Focus left every APPricot surface: releases every key and button the backend holds,
    /// so the app is left with none stuck down.
    fn blur(&mut self) -> Result<(), Self::Error>;

    /// Applies the host's configure to surface `id`. The backend resizes the window and
    /// tells the app, the way its display server expects (on X11, a `ConfigureWindow` plus
    /// a synthetic `ConfigureNotify`). When the app has taken the size — it may clamp to
    /// its own minimum — the backend reports it with
    /// [`SurfaceEvent::Resized`](crate::SurfaceEvent), even when that size is the one the
    /// surface already had: the report is what answers the configure.
    fn configure(&mut self, id: SurfaceId, size: Size) -> Result<(), Self::Error>;

    /// Asks the app to close surface `id`. It is a request: the app decides, nothing is
    /// killed.
    fn close(&mut self, id: SurfaceId) -> Result<(), Self::Error>;

    /// Makes `text` the clipboard the backend serves to the app, as plain text. What the
    /// host sends here is its own policy decision (ADR-0003 §6).
    fn clipboard_set(&mut self, text: &str) -> Result<(), Self::Error>;
}
