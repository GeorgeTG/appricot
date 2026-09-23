//! The capture side of a backend: the events it reports, and the pixels it hands back.
//!
//! FROZEN INTERFACE of implementation wave 1. `appricot-x11` implements [`CaptureBackend`]
//! on X11; `appricot-streamer` drives it. Fields and variants may be ADDED, in the same change
//! as every match on them: the enums are exhaustive on purpose, inside one unpublished
//! workspace, so the compiler finds every place a new variant needs a decision. An existing
//! signature may not change without every implementor and every caller changing in the same
//! change.

use appricot_proto::limits::{AppId, Title};

use crate::geometry::{Rect, Size};
use crate::pixels::{CursorImage, PixelBuffer};
use crate::role::Role;
use crate::surface::SurfaceId;

/// Something a backend saw happen to a surface, or to the session as a whole.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SurfaceEvent {
    /// A surface appeared: the app mapped a window.
    Created {
        /// The new surface. Ids rise strictly over a backend's life: each `Created` names an
        /// id above every id it named before, and the session refuses one that does not.
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
    /// The app's surface has this size: after a configure, or on its own. A backend reports
    /// it for every configure it is asked to apply, even one that changes nothing: a
    /// proposal of the size the surface already has, or one the app clamped back to that
    /// size. That report is what answers the configure: the session acks the waiting
    /// proposal with it, and without it the host's configure would wait for an ack forever.
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
/// `appricot-x11` implements it on X11; a later Wayland backend would too. The calls are
/// synchronous and may wait on the display server: on X11 a capture or a size query is a
/// round trip. None of them waits on the app or on a timer. The streamer keeps them off its
/// async runtime, on a thread of their own: it drains events on a short poll, and captures
/// when a frame credit is free. The session cuts every size a backend reports to the wire's
/// surface caps, and asks for pixels only inside the size it tracks; a backend still clamps
/// what the display server itself reports to what it can serve.
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
