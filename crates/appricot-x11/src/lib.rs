//! The S1 capture backend: X11, through x11rb.
//!
//! # Responsibility
//!
//! - Redirect every window off-screen with Composite, and read one window's pixels from its
//!   named pixmap, even while another window covers it. Confirmed on a plain Xvfb, with no
//!   compositing window manager present, in an internal benchmark (September 2026).
//! - Track changed areas with Damage and feed them into `appricot-core`'s per-surface damage.
//! - Inject pointer and key input with XTEST.
//! - Read the cursor image and the selections with XFixes.
//! - Do the minimal window-manager duties, because there is no other window manager in the
//!   container: answer map and configure requests, set focus, ask windows to close.
//! - Map X windows into the `appricot-core` model. An override-redirect window becomes a
//!   popup, and `WM_TRANSIENT_FOR` names a parent (see [`classify`]).
//!
//! The backend itself is [`X11Backend::connect`]. Its layout — toplevels placed where they
//! overlap least, with nothing relying on it — is documented in the `wm` module.
//!
//! Every size the backend reports is cut to the root and to the wire's surface caps, the
//! size of an override-redirect window included: the X server lets a window be any size,
//! and one the wire cannot carry would end the session.
//!
//! # Not its responsibility
//!
//! - The wire protocol, encoding, networking and window chrome.
//! - Trusting the app. A focus or size request from an X client is reported and clamped,
//!   then decided by the host, never obeyed as it comes (docs/adr/0003, §5). Position and
//!   stacking requests are never granted. The one exception is a toplevel the host has not
//!   seen yet: its own size request is granted, clamped, while its toolkit builds it.
//!
//! The connection is x11rb's `RustConnection`, which is pure Rust: no libxcb, and no unsafe
//! code unless x11rb's `allow-unsafe-code` feature is on, which this workspace does not turn
//! on (x11rb 0.14.0 source, <https://static.crates.io/crates/x11rb/x11rb-0.14.0.crate>). So
//! this crate keeps the workspace's `unsafe_code = "deny"`.

use std::fmt;

use appricot_core::{Point, Positioner, Role, Size, SurfaceId};
use x11rb::connection::RequestConnection;
use x11rb::errors::{ConnectionError, ReplyError};
use x11rb::protocol::composite::ConnectionExt as _;
use x11rb::protocol::damage::ConnectionExt as _;
use x11rb::protocol::xfixes::ConnectionExt as _;
use x11rb::protocol::xtest::ConnectionExt as _;
use x11rb::protocol::{composite, damage, xfixes, xtest};

mod atoms;
mod backend;
mod capture;
mod clipboard;
mod cursor;
mod error;
mod input;
mod keymap;
mod wm;

pub use backend::X11Backend;
pub use error::BackendError;

/// The lowest Composite version the backend accepts: 0.4.
///
/// 0.4 is also the version x11rb 0.14.0's bindings are generated from
/// (`composite::X11_XML_VERSION`).
pub const MIN_COMPOSITE: (u32, u32) = (0, 4);

/// The extensions the backend cannot work without, by their X11 names.
pub const REQUIRED_EXTENSIONS: [&str; 4] = [
    composite::X11_EXTENSION_NAME,
    damage::X11_EXTENSION_NAME,
    xfixes::X11_EXTENSION_NAME,
    xtest::X11_EXTENSION_NAME,
];

/// The versions the X server agreed to, as `(major, minor)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Extensions {
    /// Composite.
    pub composite: (u32, u32),
    /// Damage.
    pub damage: (u32, u32),
    /// XFixes.
    pub xfixes: (u32, u32),
    /// XTEST.
    pub xtest: (u8, u16),
}

/// Why an X server cannot host the backend.
#[derive(Debug)]
#[non_exhaustive]
pub enum ProbeError {
    /// A required extension is absent. The name is its X11 name.
    Missing(&'static str),
    /// Composite is older than [`MIN_COMPOSITE`]. The version is `(major, minor)`.
    CompositeTooOld((u32, u32)),
    /// The connection failed.
    Connection(ConnectionError),
    /// The server answered a version query with an error.
    Reply(ReplyError),
}

impl fmt::Display for ProbeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing(name) => write!(f, "the X server lacks the {name} extension"),
            Self::CompositeTooOld((major, minor)) => {
                write!(f, "Composite {major}.{minor} is too old")
            }
            Self::Connection(e) => write!(f, "the X connection failed: {e}"),
            Self::Reply(e) => write!(f, "an X version query failed: {e}"),
        }
    }
}

impl std::error::Error for ProbeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Connection(e) => Some(e),
            Self::Reply(e) => Some(e),
            Self::Missing(_) | Self::CompositeTooOld(_) => None,
        }
    }
}

impl From<ConnectionError> for ProbeError {
    fn from(e: ConnectionError) -> Self {
        Self::Connection(e)
    }
}

impl From<ReplyError> for ProbeError {
    fn from(e: ReplyError) -> Self {
        Self::Reply(e)
    }
}

/// Checks that the X server has every required extension, and negotiates their versions.
///
/// The streamer runs it once at start-up, and its readiness endpoint stays red until it
/// passes. Fails with the first extension that is missing or too old, or the first request
/// that fails.
pub fn probe_extensions<C: RequestConnection>(conn: &C) -> Result<Extensions, ProbeError> {
    for name in REQUIRED_EXTENSIONS {
        if conn.extension_information(name)?.is_none() {
            return Err(ProbeError::Missing(name));
        }
    }

    let (major, minor) = MIN_COMPOSITE;
    let reply = conn.composite_query_version(major, minor)?.reply()?;
    let composite = (reply.major_version, reply.minor_version);
    if composite < MIN_COMPOSITE {
        return Err(ProbeError::CompositeTooOld(composite));
    }

    let (major, minor) = damage::X11_XML_VERSION;
    let reply = conn.damage_query_version(major, minor)?.reply()?;
    let damage = (reply.major_version, reply.minor_version);

    let (major, minor) = xfixes::X11_XML_VERSION;
    let reply = conn.xfixes_query_version(major, minor)?.reply()?;
    let xfixes = (reply.major_version, reply.minor_version);

    // XTEST's version request takes a u8 major and a u16 minor; 2.2 is the version x11rb
    // 0.14.0's bindings describe.
    let reply = conn.xtest_get_version(2, 2)?.reply()?;
    let xtest = (reply.major_version, reply.minor_version);

    Ok(Extensions {
        composite,
        damage,
        xfixes,
        xtest,
    })
}

/// An X window at the moment it maps, as the window manager sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MappedWindow {
    /// The window bypasses the window manager: a menu, a tooltip, a combo list.
    pub override_redirect: bool,
    /// Its position on the root window.
    pub origin: Point,
    /// Its size.
    pub size: Size,
}

/// A surface that may become a popup's parent, with its position on the root window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParentCandidate {
    /// The surface.
    pub id: SurfaceId,
    /// Its position on the root window.
    pub origin: Point,
}

/// Chooses the model role of an X window that maps (docs/architecture.md §4.2).
///
/// - A window that maps through the window manager is a toplevel. If `WM_TRANSIENT_FOR`
///   names a toplevel, the caller records it as a dialog parent with
///   `Surface::set_toplevel_parent`.
/// - An override-redirect window is a popup. Its parent is `transient_for`, the surface
///   `WM_TRANSIENT_FOR` names, or else `focused`, the focused toplevel. Its position is its
///   root position minus its parent's.
///
/// Returns `None` for an override-redirect window with neither: a popup needs a parent, and
/// one without is not shown.
pub fn classify(
    window: MappedWindow,
    transient_for: Option<ParentCandidate>,
    focused: Option<ParentCandidate>,
) -> Option<Role> {
    if !window.override_redirect {
        return Some(Role::Toplevel);
    }
    let parent = transient_for.or(focused)?;
    let at = Point::new(
        window.origin.x.saturating_sub(parent.origin.x),
        window.origin.y.saturating_sub(parent.origin.y),
    );
    let positioner = Positioner::at(at, window.size);
    Some(Role::Popup {
        parent: parent.id,
        positioner,
    })
}

#[cfg(test)]
mod tests {
    use appricot_core::{Point, Positioner, Rect, Role, Size, SurfaceId};

    use super::{MappedWindow, ParentCandidate, classify};

    fn candidate(id: u32, x: i32, y: i32) -> ParentCandidate {
        ParentCandidate {
            id: SurfaceId::new(id),
            origin: Point::new(x, y),
        }
    }

    fn window(override_redirect: bool) -> MappedWindow {
        MappedWindow {
            override_redirect,
            origin: Point::new(130, 240),
            size: Size::new(100, 20),
        }
    }

    #[test]
    fn a_managed_window_is_a_toplevel() {
        let focused = Some(candidate(1, 0, 0));
        let role = classify(window(false), None, focused);
        assert_eq!(role, Some(Role::Toplevel));
    }

    #[test]
    fn an_override_redirect_window_is_a_popup_of_its_transient_for() {
        let owner = Some(candidate(1, 100, 200));
        let focused = Some(candidate(2, 0, 0));
        let role = classify(window(true), owner, focused);
        let parent = SurfaceId::new(1);
        let positioner = Positioner::at(Point::new(30, 40), Size::new(100, 20));
        assert_eq!(role, Some(Role::Popup { parent, positioner }));
        assert_eq!(positioner.place(), Rect::new(30, 40, 100, 20));
    }

    #[test]
    fn without_transient_for_the_focused_toplevel_is_the_parent() {
        let focused = Some(candidate(2, 30, 40));
        let role = classify(window(true), None, focused);
        let Some(Role::Popup { parent, positioner }) = role else {
            panic!("expected a popup, got {role:?}");
        };
        assert_eq!(parent, SurfaceId::new(2));
        assert_eq!(positioner.place(), Rect::new(100, 200, 100, 20));
    }

    #[test]
    fn a_popup_with_no_parent_is_not_shown() {
        assert_eq!(classify(window(true), None, None), None);
    }
}
