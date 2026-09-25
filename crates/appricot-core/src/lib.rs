//! The backend-agnostic window model of APPricot, shaped after Wayland's xdg-shell.
//!
//! # Responsibility
//!
//! - Surfaces: an id, a role, a size, a scale, damage, and explicit configure and ack.
//! - Roles: a toplevel, or a popup placed by a positioner relative to its parent.
//! - Per-surface damage accumulation, bounded.
//! - Frame pacing by client credits, never by a timer.
//! - One session's surface set under the wire caps: what the streamer must put on the wire,
//!   the configure queue and its acks, detach and resume, and the frames planned over
//!   per-surface credits ([`Session`]).
//! - The traits a capture backend implements: [`CaptureBackend`] and [`InputSink`]. Their
//!   signatures are the frozen interface of implementation wave 1.
//!
//! # Not its responsibility
//!
//! - No X11 types and no Wayland types. A backend maps its windows into this model:
//!   `appricot-x11` turns an override-redirect window into a popup and follows
//!   `WM_TRANSIENT_FOR` to a parent.
//! - No I/O: no sockets, no threads, no async runtime, no clock.
//! - No encoding and no wire format. Those are `appricot-encode` and `appricot-proto`.
//!
//! In this model the host plays the compositor (it decides size, position, stacking and
//! focus) and the streamed app plays the Wayland client. `docs/architecture.md` §4 explains
//! the model.

mod capture;
mod damage;
mod frame;
mod geometry;
mod input;
mod pixels;
mod role;
mod session;
mod surface;

pub use capture::{CaptureBackend, MAX_CLIPBOARD_BYTES, SurfaceEvent};
pub use damage::{Damage, MAX_DAMAGE_RECTS};
pub use frame::FrameCredits;
pub use geometry::{Point, Rect, Size};
pub use input::{
    InputSink, KeyCode, KeyEvent, Keysym, MAX_POINTER_AXIS_STEPS, PointerButton, PressState,
};
pub use pixels::{CursorImage, PixelBuffer, PixelFormat};
pub use role::{Anchor, Positioner, Role};
pub use session::{
    FramePlan, GoneReason, MAX_FRAME_CREDITS, MAX_POPUPS_PER_PARENT, MAX_SURFACE_HEIGHT,
    MAX_SURFACE_WIDTH, MAX_SURFACES, Session, SessionEvent,
};
pub use surface::{ConfigureSerial, MAX_PENDING_CONFIGURES, Scale, Surface, SurfaceId};
