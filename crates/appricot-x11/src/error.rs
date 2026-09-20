//! [`BackendError`]: everything that can go wrong between `appricot-x11` and the X server.

use std::fmt;

use appricot_core::SurfaceId;
use x11rb::errors::{ConnectError, ConnectionError, ReplyError, ReplyOrIdError};
use x11rb::protocol::ErrorKind;

use crate::ProbeError;

/// Why a call on [`X11Backend`](crate::X11Backend) failed.
///
/// It is the `Error` type of both `CaptureBackend` and `InputSink`, so it is `Send + Sync`
/// and names the surface involved where one is.
#[derive(Debug)]
#[non_exhaustive]
pub enum BackendError {
    /// The display could not be opened at all.
    Connect(ConnectError),
    /// The connection to the X server broke after it was established. The session cannot go
    /// on after this.
    Connection(ConnectionError),
    /// The X server answered a request with an error.
    Reply(ReplyError),
    /// The X server cannot host the backend: an extension is missing or too old.
    Probe(ProbeError),
    /// Another client already manages the root window (it holds `SubstructureRedirect`),
    /// so there is no room for this backend's window-manager duties.
    NotWindowManager,
    /// The surface id names no surface this backend tracks: it never existed, or it is gone.
    UnknownSurface(SurfaceId),
    /// The keymap has no keycode for the keysym, or reaching it needs a modifier level the
    /// backend does not translate (level 2 and up, AltGr). Greek input needs a Greek layout
    /// on the server plus the keysym mapping in [`crate`]'s keymap module.
    KeysymUnavailable(u32),
    /// `GetImage` returned a depth the backend cannot turn into [`appricot_core::PixelFormat`].
    UnsupportedDepth(u8),
    /// `GetImage` returned fewer bytes than the rectangle needs.
    ShortImage {
        /// The bytes that arrived.
        got: usize,
        /// The bytes at least one row of the rectangle needs.
        wanted: usize,
    },
    /// The server's XID space is exhausted: no more windows, pixmaps or damage objects
    /// fit on this connection.
    IdsExhausted,
}

impl fmt::Display for BackendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connect(e) => write!(f, "cannot open the display: {e}"),
            Self::Connection(e) => write!(f, "the X connection failed: {e}"),
            Self::Reply(e) => write!(f, "the X server refused a request: {e}"),
            Self::Probe(e) => write!(f, "the X server cannot host the backend: {e}"),
            Self::NotWindowManager => {
                f.write_str("another client is already the window manager on this root")
            }
            Self::UnknownSurface(id) => write!(f, "surface {} is not known here", id.get()),
            Self::KeysymUnavailable(sym) => {
                write!(f, "no keycode for keysym 0x{sym:x} in this keymap")
            }
            Self::UnsupportedDepth(depth) => write!(f, "cannot read pixels of depth {depth}"),
            Self::ShortImage { got, wanted } => {
                write!(f, "GetImage gave {got} bytes where {wanted} were needed")
            }
            Self::IdsExhausted => f.write_str("the X server's id space is exhausted"),
        }
    }
}

impl std::error::Error for BackendError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Connect(e) => Some(e),
            Self::Connection(e) => Some(e),
            Self::Reply(e) => Some(e),
            Self::Probe(e) => Some(e),
            Self::NotWindowManager
            | Self::UnknownSurface(_)
            | Self::KeysymUnavailable(_)
            | Self::UnsupportedDepth(_)
            | Self::ShortImage { .. }
            | Self::IdsExhausted => None,
        }
    }
}

impl From<ConnectError> for BackendError {
    fn from(e: ConnectError) -> Self {
        Self::Connect(e)
    }
}

impl From<ConnectionError> for BackendError {
    fn from(e: ConnectionError) -> Self {
        Self::Connection(e)
    }
}

impl From<ReplyError> for BackendError {
    fn from(e: ReplyError) -> Self {
        Self::Reply(e)
    }
}

impl From<ProbeError> for BackendError {
    fn from(e: ProbeError) -> Self {
        Self::Probe(e)
    }
}

impl From<ReplyOrIdError> for BackendError {
    fn from(e: ReplyOrIdError) -> Self {
        match e {
            ReplyOrIdError::IdsExhausted => Self::IdsExhausted,
            ReplyOrIdError::ConnectionError(e) => Self::Connection(e),
            ReplyOrIdError::X11Error(e) => Self::Reply(ReplyError::X11Error(e)),
        }
    }
}

impl BackendError {
    /// True when the error says the server still grants an exclusive right to another
    /// client that has just disconnected: the window-manager mask or the Composite manual
    /// redirect are both single-holder, and the server releases them asynchronously. A
    /// caller reconnecting right after a previous backend dropped may have to try again.
    pub fn is_takeover_conflict(&self) -> bool {
        match self {
            Self::NotWindowManager => true,
            Self::Reply(ReplyError::X11Error(e)) => e.error_kind == ErrorKind::Access,
            _ => false,
        }
    }

    /// True when the error only says that the X window involved is gone or was never there.
    ///
    /// Events about a window can arrive a hair after its client destroyed it; when this is
    /// true, the caller drops the bookkeeping instead of failing the session.
    pub fn is_window_gone(&self) -> bool {
        let Self::Reply(ReplyError::X11Error(e)) = self else {
            return false;
        };
        matches!(
            e.error_kind,
            ErrorKind::Window
                | ErrorKind::Match
                | ErrorKind::Drawable
                | ErrorKind::Pixmap
                | ErrorKind::DamageBadDamage
        )
    }
}
