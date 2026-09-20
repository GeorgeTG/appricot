//! The capture side of a backend: the events it reports, and the pixels it hands back.
//!
//! FROZEN INTERFACE of implementation wave 1. `appricot-x11` implements [`CaptureBackend`]
//! on X11; `appricot-streamer` drives it. Fields and variants may be ADDED (the enums are
//! `#[non_exhaustive]`, so match with a wildcard arm); an existing signature may not change
//! without every implementor and every caller changing in the same change.

use appricot_proto::limits::{AppId, Title};

use crate::geometry::{Rect, Size};
use crate::pixels::{CursorImage, PixelBuffer};
use crate::role::Role;
use crate::surface::SurfaceId;

/// Something a backend saw happen to a surface, or to the session as a whole.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SurfaceEvent {
    /// A surface appeared: the app mapped a window.
    Created {
        /// The new surface.
        id: SurfaceId,
        /// What it is for.
        role: Role,
        /// Its size.
        size: Size,
        /// A dialog's toplevel parent, when it names one at map time (X11
        /// `WM_TRANSIENT_FOR`); `None` for a plain toplevel. A popup's parent lives in its
        /// role, never here.
        parent: Option<SurfaceId>,
    },
    /// The app set a title or an app id. Both are untrusted text, already capped.
    Metadata {
        /// The surface.
        id: SurfaceId,
        /// The title.
        title: Title,
        /// The app id.
        app_id: AppId,
    },
    /// Pixels changed inside `rect`, in surface coordinates.
    Damaged {
        /// The surface.
        id: SurfaceId,
        /// The changed area.
        rect: Rect,
    },
    /// The app resized its surface, after an ack or on its own.
    Resized {
        /// The surface.
        id: SurfaceId,
        /// The new size.
        size: Size,
    },
    /// The surface is gone. Its id is never reused.
    Destroyed {
        /// The surface.
        id: SurfaceId,
    },
    /// The app asked for the keyboard focus (X11 `_NET_ACTIVE_WINDOW`). The host decides;
    /// the streamer reports and never obeys directly (ADR-0003 §5).
    FocusRequested {
        /// The surface that asked.
        id: SurfaceId,
    },
    /// The app asked to resize itself. The host decides; the streamer reports.
    ResizeRequested {
        /// The surface that asked.
        id: SurfaceId,
        /// The size it asked for, clamped to the limits.
        size: Size,
    },
    /// The cursor image changed. One cursor per session, not per surface.
    CursorChanged {
        /// The new cursor image.
        cursor: CursorImage,
    },
    /// The app pasted and the backend holds no clipboard text. The host decides what, if
    /// anything, to send back through
    /// [`InputSink::clipboard_set`](crate::InputSink::clipboard_set).
    ClipboardRequested,
}

/// Reads surfaces and their pixels from a display server.
///
/// `appricot-x11` implements it on X11; a later Wayland backend would too. The calls do not
/// block: the streamer makes them when the display connection is readable, or when a frame
/// credit is free. Sizes handed in and out are already clamped to the wire limits by the
/// caller; a backend clamps again what the display server itself reports.
pub trait CaptureBackend {
    /// What can go wrong talking to the display server.
    type Error: std::error::Error + Send + Sync + 'static;

    /// Moves every event that is ready into `out`, without blocking.
    ///
    /// Fails when the display connection fails; the session cannot go on after that.
    fn drain_events(&mut self, out: &mut Vec<SurfaceEvent>) -> Result<(), Self::Error>;

    /// The root window's size, in pixels. Toplevels are placed inside it, and no configure
    /// the streamer applies may exceed it.
    fn root_size(&mut self) -> Result<Size, Self::Error>;

    /// Copies the pixels of `rect`, in surface coordinates, of surface `id`.
    ///
    /// `rect` is at most one tile (the wire limits cap tiles at 256x256); the buffer it
    /// returns carries the pixels row by row, its size matching `rect`. Fails when the
    /// surface is gone or the display connection fails. A partially covered or unmapped
    /// window still returns its last pixels: the backend holds the Composite redirect.
    fn capture(&mut self, id: SurfaceId, rect: Rect) -> Result<PixelBuffer, Self::Error>;
}
