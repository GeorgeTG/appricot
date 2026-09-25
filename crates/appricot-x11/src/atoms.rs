//! Every atom the backend interns, once, at connect.
//!
//! All requests are written before any reply is read, so interning costs one round trip
//! however many atoms there are.

use x11rb::connection::RequestConnection;
use x11rb::protocol::xproto::Atom;
use x11rb::protocol::xproto::ConnectionExt as _;

use crate::BackendError;

/// The atoms the backend works with, by field name.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Atoms {
    /// `WM_PROTOCOLS`: the property listing the protocols a window takes part in.
    pub wm_protocols: Atom,
    /// `WM_DELETE_WINDOW`: the close protocol.
    pub wm_delete_window: Atom,
    /// `WM_TAKE_FOCUS`: the focus protocol.
    pub wm_take_focus: Atom,
    /// `WM_TRANSIENT_FOR`: names a window's transient parent.
    pub wm_transient_for: Atom,
    /// `WM_NAME`: the Latin-1 title.
    pub wm_name: Atom,
    /// `WM_CLASS`: `res_name\0res_class\0`, the app id source.
    pub wm_class: Atom,
    /// `WM_NORMAL_HINTS`: the size hints.
    pub wm_normal_hints: Atom,
    /// `WM_STATE` (both the property and its type): the ICCCM state a WM maintains.
    pub wm_state: Atom,
    /// `_NET_WM_NAME`: the UTF-8 title.
    pub net_wm_name: Atom,
    /// `_NET_ACTIVE_WINDOW`: how a client asks the WM for focus.
    pub net_active_window: Atom,
    /// `_NET_SUPPORTING_WM_CHECK`: the EWMH check window property.
    pub net_supporting_wm_check: Atom,
    /// `_NET_SUPPORTED`: the EWMH capabilities list.
    pub net_supported: Atom,
    /// `CLIPBOARD`: the selection the backend owns.
    pub clipboard: Atom,
    /// `TARGETS`: the selection target that lists targets.
    pub targets: Atom,
    /// `TIMESTAMP`: the selection target that asks when the owner took the selection.
    pub timestamp: Atom,
    /// `UTF8_STRING`: the UTF-8 text selection target.
    pub utf8_string: Atom,
    /// `TEXT`: the Latin-1-ish text selection target.
    pub text: Atom,
    /// `STRING`: the Latin-1 text selection target (a predefined atom).
    pub string: Atom,
    /// `text/plain;charset=utf-8`: the MIME text selection target.
    pub text_plain_utf8: Atom,
    /// `APPRICOT_CLIPBOARD_FETCH`: the property the backend's own UTF8_STRING request of the
    /// app's selection lands in.
    pub clipboard_fetch: Atom,
}

macro_rules! intern_all {
    ($conn:expr, $($cookie:ident => $name:literal),+ $(,)?) => {{
        $(let $cookie = $conn.intern_atom(false, $name.as_bytes())?;)+
        Ok(Self {
            $( $cookie: $cookie.reply()?.atom, )+
        })
    }};
}

impl Atoms {
    /// Interns everything. Fails when the connection fails or the server answers an
    /// intern with an error.
    pub(crate) fn intern<C: RequestConnection>(conn: &C) -> Result<Self, BackendError> {
        intern_all!(
            conn,
            wm_protocols => "WM_PROTOCOLS",
            wm_delete_window => "WM_DELETE_WINDOW",
            wm_take_focus => "WM_TAKE_FOCUS",
            wm_transient_for => "WM_TRANSIENT_FOR",
            wm_name => "WM_NAME",
            wm_class => "WM_CLASS",
            wm_normal_hints => "WM_NORMAL_HINTS",
            wm_state => "WM_STATE",
            net_wm_name => "_NET_WM_NAME",
            net_active_window => "_NET_ACTIVE_WINDOW",
            net_supporting_wm_check => "_NET_SUPPORTING_WM_CHECK",
            net_supported => "_NET_SUPPORTED",
            clipboard => "CLIPBOARD",
            targets => "TARGETS",
            timestamp => "TIMESTAMP",
            utf8_string => "UTF8_STRING",
            text => "TEXT",
            string => "STRING",
            text_plain_utf8 => "text/plain;charset=utf-8",
            clipboard_fetch => "APPRICOT_CLIPBOARD_FETCH",
        )
    }
}
