//! [`X11Backend`]: the `CaptureBackend` and `InputSink` implementation on X11.

use std::time::{Duration, Instant};

use appricot_core::{
    CaptureBackend, InputSink, KeyCode, KeyEvent, MAX_CLIPBOARD_BYTES, MAX_POINTER_AXIS_STEPS,
    PixelBuffer, Point, PointerButton, PressState, Rect, Role, Size, SurfaceEvent, SurfaceId,
};
use x11rb::connection::Connection as _;
use x11rb::cookie::VoidCookie;
use x11rb::errors::ReplyError;
use x11rb::protocol::composite;
use x11rb::protocol::composite::ConnectionExt as _;
use x11rb::protocol::damage;
use x11rb::protocol::damage::ConnectionExt as _;
use x11rb::protocol::xfixes;
use x11rb::protocol::xfixes::ConnectionExt as _;
use x11rb::protocol::xproto as x;
use x11rb::protocol::xproto::ConnectionExt as _;
use x11rb::protocol::xtest::ConnectionExt as _;
use x11rb::protocol::{ErrorKind, Event};
use x11rb::rust_connection::RustConnection;
use x11rb::{COPY_DEPTH_FROM_PARENT, COPY_FROM_PARENT, CURRENT_TIME, NONE};

use crate::atoms::Atoms;
use crate::capture::{convert_zpixmap, place_in, zeroed};
use crate::clipboard::{
    CLIPBOARD_FETCH_LONGS, Clipboard, ClipboardFetch, latin1_decode, latin1_encode,
};
use crate::cursor::{cursor_image, is_grab_refusal};
use crate::error::BackendError;
use crate::input::{HeldInput, KeyId, KeyPress, NoShiftKey, Stroke};
use crate::keymap::{
    KeyKind, Keymap, Plan, Step, XK_CAPS_LOCK, XK_SHIFT_L, key_kind, plan, release_keycode,
};
use crate::wm::{
    MAX_SURFACE_HEIGHT, MAX_SURFACE_WIDTH, SizeHints, TrackedWindow, WindowKind, WindowTable,
    bounded_size, decode_utf8_cut, place_toplevel, size_bound,
};
use crate::{MappedWindow, ParentCandidate, classify};

// Predefined atoms named by value (the X protocol fixes them): `AnyPropertyType`, `ATOM`,
// `INTEGER`, `WINDOW`, and the `PointerRoot` window. Everything else is interned through
// [`Atoms`].
const XA_ANY: u32 = 0;
const XA_ATOM: u32 = 4;
const XA_INTEGER: u32 = 19;
const XA_WINDOW: u32 = 33;
const POINTER_ROOT_WINDOW: u32 = 1;

// XTEST speaks raw event codes.
const X_KEY_PRESS: u8 = 2;
const X_KEY_RELEASE: u8 = 3;
const X_BUTTON_PRESS: u8 = 4;
const X_BUTTON_RELEASE: u8 = 5;
const X_MOTION_NOTIFY: u8 = 6;

// X pointer buttons 1-3 are the logical buttons; 4-7 are the wheel steps.
const BUTTON_LEFT: u8 = 1;
const BUTTON_MIDDLE: u8 = 2;
const BUTTON_RIGHT: u8 = 3;
const WHEEL_UP: u8 = 4;
const WHEEL_DOWN: u8 = 5;
const WHEEL_LEFT: u8 = 6;
const WHEEL_RIGHT: u8 = 7;

/// The ICCCM `WM_STATE` value of a normal (not iconified) window.
const WM_STATE_NORMAL: u32 = 1;

/// The ICCCM `WM_STATE` value of a window its client withdrew.
const WM_STATE_WITHDRAWN: u32 = 0;

/// The most X events one `drain_events` call handles. An app that floods damage cannot
/// keep the call looping; what is left stays queued for the next call.
const MAX_EVENTS_PER_DRAIN: usize = 512;

/// How long the fetch of the app's clipboard text waits for its answer. Nothing blocks on
/// it — drains stay non-blocking whatever the owner does — but an answer this late belongs
/// to a fetch that was given up, and is ignored.
const CLIPBOARD_FETCH_TIMEOUT: Duration = Duration::from_secs(1);

/// The byte cap a title is cut to, mirroring `appricot-proto`'s `MAX_TITLE_BYTES` (512,
/// provisional until the wire spec task fixes it).
const TITLE_CAP: usize = 512;

/// The byte cap an app id is cut to, mirroring `appricot-proto`'s `MAX_APP_ID_BYTES`
/// (256, likewise provisional).
const APP_ID_CAP: usize = 256;

/// Everything the backend reads about a window before managing it.
#[derive(Debug, Clone, Default)]
struct WindowState {
    has_delete: bool,
    take_focus: bool,
    title_net: Option<String>,
    title_name: Option<String>,
    app_id: String,
    hints: SizeHints,
    /// The raw window `WM_TRANSIENT_FOR` names, when the property is a real `WINDOW`; the
    /// caller resolves it against the table, because only the table knows what is a toplevel.
    transient_for: Option<x::Window>,
}

/// The S1 backend: window manager, capture and input on one X connection.
///
/// Build it with [`X11Backend::connect`] and drive it through the `CaptureBackend` and
/// `InputSink` traits. No call waits for an event: the streamer drains what is ready when
/// it polls, and spends input and frame credits when it has them. One `drain_events` call
/// handles a bounded number of events. Calls do make X round trips (reading properties and
/// images, checked requests), and nothing times them out: an X server that stalls, for
/// example under a client's `GrabServer`, stalls the calling thread with it.
///
/// The backend is the only window manager on the display — it takes `SubstructureRedirect`
/// on the root at connect and fails when anyone else holds it. Its layout places toplevels
/// where they overlap least, but they do overlap on a small root; the wm module (private)
/// documents why nothing relies on the layout.
#[derive(Debug)]
pub struct X11Backend {
    conn: RustConnection,
    root: x::Window,
    atoms: Atoms,
    /// The invisible window that owns the CLIPBOARD selection for the backend.
    owner_window: x::Window,
    table: WindowTable,
    clipboard: Clipboard,
    held: HeldInput,
    keymap: Keymap,
    /// The spare keycodes rebound for held keys, with the keysym each carries: released
    /// when the key comes up (or on blur), by writing an empty row back.
    rebound: Vec<(u8, u32)>,
    /// The toplevel input last went to: the host's focus, or the pointer's surface since. A
    /// popup that names no parent is given this one. It never moves the keyboard.
    focused: Option<SurfaceId>,
    /// The surface pointer input last raised, so motion raises once per target change
    /// rather than once per event.
    ensured: Option<SurfaceId>,
    /// The next surface id when `ensured` was raised: a window tracked since may sit above
    /// it, so the next motion raises again.
    raised_at: SurfaceId,
    /// The toplevel keys go to: the one the host last focused, or a focused popup's parent.
    /// Only `focus` sets it, and `blur` keeps it: the next key after a blur goes back there.
    host_focused: Option<SurfaceId>,
    /// The toplevel this backend last gave the X keyboard focus to; `None` after a blur.
    keyboard: Option<SurfaceId>,
    /// Where the backend last put the X pointer: the surface, the local point, the root
    /// point.
    pointer_at: Option<(SurfaceId, Point, Point)>,
    root_size: Size,
    /// The bytes per pixel the server stores a pixmap of each depth at, from its
    /// pixmap-format table: depth 24 is 32bpp on most servers, Xvfb included.
    pixel_bpp: Vec<(u8, usize)>,
    /// Events produced outside a drain (adoption at connect, a configure that changed
    /// nothing), drained before live ones.
    pending: Vec<SurfaceEvent>,
}

impl X11Backend {
    /// Connects to the X server and takes over the display.
    ///
    /// `display` names the display; `None` reads `$DISPLAY`. Beyond opening the connection
    /// this:
    ///
    /// - proves the extensions are present ([`crate::probe_extensions`]),
    /// - takes `SubstructureRedirect` on the root, failing with
    ///   [`BackendError::NotWindowManager`] when another window manager is already there,
    /// - starts the Composite `Manual` redirect of the root's subwindows and holds it for
    ///   the connection's life — a redirect taken after the app maps reads stale pixels
    ///   (docs/architecture.md §3.1),
    /// - owns the `CLIPBOARD` selection through a window of its own, so every paste the
    ///   app makes arrives here,
    /// - selects XFixes cursor and selection notifications,
    /// - reads the keyboard mapping, and
    /// - adopts every window that is already mapped (a streamer reconnecting to a live
    ///   session), reporting it on the first `drain_events`.
    pub fn connect(display: Option<&str>) -> Result<Self, BackendError> {
        let (conn, screen_num) = x11rb::connect(display)?;
        let root = conn.setup().roots[screen_num].root;

        crate::probe_extensions(&conn)?;
        take_root(&conn, root)?;

        conn.composite_redirect_subwindows(root, composite::Redirect::MANUAL)?
            .check()?;

        let atoms = Atoms::intern(&conn)?;
        let owner_window = create_selection_owner(&conn, root, atoms.clipboard)?;
        select_xfixes(&conn, root, atoms.clipboard)?;
        announce_ewmh(&conn, root, owner_window, &atoms)?;

        let keymap = fetch_keymap(&conn)?;
        let root_geometry = conn.get_geometry(root)?.reply()?;
        let root_size = Size::new(
            u32::from(root_geometry.width),
            u32::from(root_geometry.height),
        );

        let pixel_bpp: Vec<(u8, usize)> = conn
            .setup()
            .pixmap_formats
            .iter()
            .map(|format| (format.depth, usize::from(format.bits_per_pixel) / 8))
            .collect();
        let mut backend = Self {
            conn,
            root,
            atoms,
            owner_window,
            table: WindowTable::new(),
            clipboard: Clipboard::default(),
            held: HeldInput::new(),
            keymap,
            rebound: Vec::new(),
            focused: None,
            ensured: None,
            raised_at: SurfaceId::new(0),
            host_focused: None,
            keyboard: None,
            pointer_at: None,
            root_size,
            pixel_bpp,
            pending: Vec::new(),
        };
        backend.adopt_existing()?;
        backend.release_input_left_down(true)?;
        Ok(backend)
    }

    /// Adopts every mapped window that predates the connection: the toplevels first, then
    /// the override-redirect windows as popups of the toplevels `WM_TRANSIENT_FOR` names.
    fn adopt_existing(&mut self) -> Result<(), BackendError> {
        let children = self.conn.query_tree(self.root)?.reply()?.children;
        let mut adopted = Vec::new();
        let mut popups = Vec::new();
        for window in children {
            // A gone window answers with an error; there is nothing to adopt.
            let Ok(attrs) = self.conn.get_window_attributes(window)?.reply() else {
                continue;
            };
            if attrs.map_state != x::MapState::VIEWABLE {
                continue;
            }
            if attrs.override_redirect {
                popups.push(window);
            } else {
                self.manage_window(window, false, &mut adopted)?;
            }
        }
        for window in popups {
            self.show_popup(window, &mut adopted)?;
        }
        self.pending.append(&mut adopted);
        Ok(())
    }

    // --- reading the server ----------------------------------------------------------------

    /// The window's geometry — its outer corner in root coordinates and its inside size —
    /// and its border width, or `None` when it is already gone.
    fn window_geometry(&self, window: x::Window) -> Result<Option<(Rect, u16)>, BackendError> {
        match self.conn.get_geometry(window)?.reply() {
            Ok(geo) => Ok(Some((
                Rect::new(
                    i32::from(geo.x),
                    i32::from(geo.y),
                    u32::from(geo.width),
                    u32::from(geo.height),
                ),
                geo.border_width,
            ))),
            // Only a gone window answers a geometry query with an error.
            Err(_) => Ok(None),
        }
    }

    /// Reads a property as raw bytes; a gone window or a failed read is simply no data.
    fn read_property(
        &self,
        window: x::Window,
        property: u32,
        long_len: u32,
    ) -> Result<Option<x::GetPropertyReply>, BackendError> {
        Ok(self
            .conn
            .get_property(false, window, property, XA_ANY, 0, long_len)?
            .reply()
            .ok())
    }

    /// Everything read about a window before managing it. The requests are written
    /// together and read together, so this costs one round trip.
    fn read_window_state(&self, window: x::Window) -> Result<WindowState, BackendError> {
        let atoms = &self.atoms;
        let c_protocols =
            self.conn
                .get_property(false, window, atoms.wm_protocols, XA_ANY, 0, 16)?;
        let c_net_name = self.conn.get_property(
            false,
            window,
            atoms.net_wm_name,
            XA_ANY,
            0,
            u32::try_from(TITLE_CAP / 4 + 1).unwrap_or(u32::MAX),
        )?;
        let c_name = self.conn.get_property(
            false,
            window,
            atoms.wm_name,
            XA_ANY,
            0,
            u32::try_from(TITLE_CAP / 4 + 1).unwrap_or(u32::MAX),
        )?;
        let c_class = self.conn.get_property(
            false,
            window,
            atoms.wm_class,
            XA_ANY,
            0,
            u32::try_from(APP_ID_CAP / 4 + 1).unwrap_or(u32::MAX),
        )?;
        let c_hints =
            self.conn
                .get_property(false, window, atoms.wm_normal_hints, XA_ANY, 0, 18)?;
        // Asking for type WINDOW makes a wrongly-typed property read as absent, which is
        // the whole validation a map-time transient deserves.
        let c_transient =
            self.conn
                .get_property(false, window, atoms.wm_transient_for, XA_WINDOW, 0, 1)?;

        let protocols = c_protocols
            .reply()
            .ok()
            .map(|r| r.value)
            .unwrap_or_default();
        let has_protocol = |atom: u32| {
            protocols
                .chunks_exact(4)
                .any(|w| u32::from_ne_bytes([w[0], w[1], w[2], w[3]]) == atom)
        };
        // The reads stop at a byte count, which can split the last character: the decode
        // drops that tail instead of the whole title.
        let title_net = c_net_name
            .reply()
            .ok()
            .filter(|r| r.type_ == atoms.utf8_string)
            .map(|r| decode_utf8_cut(&r.value));
        let title_name = c_name.reply().ok().map(|r| {
            if r.type_ == atoms.utf8_string {
                decode_utf8_cut(&r.value)
            } else {
                latin1_decode(&r.value)
            }
        });
        let app_id = c_class
            .reply()
            .ok()
            .map(|r| wm_class_res_class(&r.value))
            .unwrap_or_default();
        let hints = c_hints
            .reply()
            .ok()
            .map(|r| SizeHints::parse(&r.value))
            .unwrap_or_default();
        let transient_for = c_transient
            .reply()
            .ok()
            .filter(|r| r.type_ == XA_WINDOW)
            .and_then(|r| {
                r.value
                    .chunks_exact(4)
                    .next()
                    .map(|w| u32::from_ne_bytes([w[0], w[1], w[2], w[3]]))
                    .filter(|owner| *owner != 0)
            });

        Ok(WindowState {
            has_delete: has_protocol(atoms.wm_delete_window),
            take_focus: has_protocol(atoms.wm_take_focus),
            title_net,
            title_name,
            app_id,
            hints,
            transient_for,
        })
    }

    /// The tracked toplevel `WM_TRANSIENT_FOR` names, as a classify parent candidate.
    fn read_transient_candidate(
        &self,
        window: x::Window,
    ) -> Result<Option<ParentCandidate>, BackendError> {
        let Some(reply) = self.read_property(window, self.atoms.wm_transient_for, 1)? else {
            return Ok(None);
        };
        let owner = reply
            .value
            .chunks_exact(4)
            .next()
            .map(|w| u32::from_ne_bytes([w[0], w[1], w[2], w[3]]))
            .filter(|id| *id != 0);
        Ok(owner
            .and_then(|owner| self.table.get(owner))
            .filter(|t| t.kind == WindowKind::Managed)
            .map(|t| ParentCandidate {
                id: t.id,
                origin: t.geometry.origin,
            }))
    }

    /// The focused toplevel as a classify parent candidate.
    fn focused_candidate(&self) -> Option<ParentCandidate> {
        let id = self.focused?;
        let tracked = self.table.by_surface(id)?;
        (tracked.kind == WindowKind::Managed).then_some(ParentCandidate {
            id,
            origin: tracked.geometry.origin,
        })
    }

    // --- window management -----------------------------------------------------------------

    /// Manages a toplevel: places it inside the root with no border, configures it,
    /// watches its properties and damage, maps it when `map`, and reports `Created` — with
    /// the toplevel parent `WM_TRANSIENT_FOR` names, when it names one this backend manages.
    ///
    /// A client that destroys its window mid-map leaves nothing behind and reports
    /// nothing: every step tolerates the window vanishing under it.
    fn manage_window(
        &mut self,
        window: x::Window,
        map: bool,
        out: &mut Vec<SurfaceEvent>,
    ) -> Result<(), BackendError> {
        if self.table.contains(window) {
            return Ok(());
        }
        let Some((geometry, _border)) = self.window_geometry(window)? else {
            return Ok(());
        };
        let state = self.read_window_state(window)?;
        // A dialog's parent: the transient target is kept only while it is a toplevel this
        // backend already manages. Anything else — an unknown or untracked window, the root
        // (ICCCM's "transient for the whole group"), the window itself (never tracked at
        // its own map time) — leaves a plain toplevel. The property is read once, here:
        // clients set it before mapping, and the wire has no message for a parent that
        // changes later.
        let parent = state
            .transient_for
            .and_then(|owner| self.table.get(owner))
            .filter(|owner| owner.kind == WindowKind::Managed)
            .map(|owner| owner.id);
        let placed = place_toplevel(
            state.hints.clamp(geometry.size, size_bound(self.root_size)),
            self.root_size,
            &self.table.managed_geometries(),
        );

        check_gone(self.conn.change_window_attributes(
            window,
            &x::ChangeWindowAttributesAux::new().event_mask(x::EventMask::PROPERTY_CHANGE),
        )?)?;
        // The border goes: the named pixmap would include it and shift every pixel and
        // pointer coordinate by its width.
        check_gone(
            self.conn.configure_window(
                window,
                &x::ConfigureWindowAux::new()
                    .x(placed.origin.x)
                    .y(placed.origin.y)
                    .width(placed.size.width)
                    .height(placed.size.height)
                    .border_width(0),
            )?,
        )?;
        self.set_wm_state(window, WM_STATE_NORMAL);
        if map {
            check_gone(self.conn.map_window(window)?)?;
        }
        let damage = self.create_damage(window);

        let tracked = TrackedWindow {
            id: self.table.peek_next_surface(),
            window,
            kind: WindowKind::Managed,
            role: Role::Toplevel,
            geometry: placed,
            border: 0,
            hints: state.hints,
            has_delete: state.has_delete,
            take_focus: state.take_focus,
            title_net: state.title_net,
            title_name: state.title_name,
            app_id: state.app_id,
            damage,
            pixmap: None,
        };
        let id = self.table.insert(window, tracked);
        out.push(SurfaceEvent::Created {
            id,
            role: Role::Toplevel,
            size: placed.size,
            parent,
        });
        self.report_metadata(window, false, out);
        Ok(())
    }

    /// Shows a mapped override-redirect window as a popup, when a parent can be named for
    /// it. A parentless one is not shown; it is looked at again the next time it maps.
    ///
    /// The window keeps its own size and border — the WM does not configure what it does
    /// not manage — but the size it is reported at is cut to the root and the wire's
    /// caps, and its position is its inside corner.
    fn show_popup(
        &mut self,
        window: x::Window,
        out: &mut Vec<SurfaceEvent>,
    ) -> Result<(), BackendError> {
        if self.table.contains(window) {
            return Ok(());
        }
        let Some((outer, border)) = self.window_geometry(window)? else {
            return Ok(());
        };
        let geometry = inner_geometry(outer, border);
        let size = bounded_size(geometry.size, size_bound(self.root_size));
        let transient = self.read_transient_candidate(window)?;
        let focused = self.focused_candidate();
        let Some(role) = classify(
            MappedWindow {
                override_redirect: true,
                origin: geometry.origin,
                size,
            },
            transient,
            focused,
        ) else {
            return Ok(());
        };
        let damage = self.create_damage(window);
        let tracked = TrackedWindow {
            id: self.table.peek_next_surface(),
            window,
            kind: WindowKind::OverrideRedirect,
            geometry,
            border,
            hints: SizeHints::default(),
            has_delete: false,
            take_focus: false,
            role,
            title_net: None,
            title_name: None,
            app_id: String::new(),
            damage,
            pixmap: None,
        };
        let role = tracked.role;
        let id = self.table.insert(window, tracked);
        out.push(SurfaceEvent::Created {
            id,
            role,
            size,
            parent: None,
        });
        Ok(())
    }

    /// Frees every server resource a window held and reports it gone.
    fn destroy_window(&mut self, window: x::Window, out: &mut Vec<SurfaceEvent>) {
        if let Some(tracked) = self.table.remove(window) {
            if let Some(damage) = tracked.damage
                && let Ok(cookie) = self.conn.damage_destroy(damage)
            {
                let _ = cookie.check();
            }
            if let Some((pixmap, _)) = tracked.pixmap
                && let Ok(cookie) = self.conn.free_pixmap(pixmap)
            {
                let _ = cookie.check();
            }
            if self.focused == Some(tracked.id) {
                self.focused = None;
            }
            if self.ensured == Some(tracked.id) {
                self.ensured = None;
            }
            out.push(SurfaceEvent::Destroyed { id: tracked.id });
        }
    }

    /// Maintains the ICCCM `WM_STATE` property: normal while managed, withdrawn when the
    /// client unmaps. Toolkits read it to see a real window manager is present, and some
    /// wait for the withdrawn state before they show a window again. The request is not
    /// waited on: a window gone meanwhile has no state left to keep.
    fn set_wm_state(&mut self, window: x::Window, state: u32) {
        let data: Vec<u8> = [state, NONE]
            .iter()
            .flat_map(|word| word.to_ne_bytes())
            .collect();
        if let Ok(cookie) = self.conn.change_property(
            x::PropMode::REPLACE,
            window,
            self.atoms.wm_state,
            self.atoms.wm_state,
            32,
            2,
            &data,
        ) {
            cookie.ignore_error();
        }
    }

    /// Reports the window's current title and app id: always when `always`, else only when
    /// it has either.
    fn report_metadata(&self, window: x::Window, always: bool, out: &mut Vec<SurfaceEvent>) {
        let Some(tracked) = self.table.get(window) else {
            return;
        };
        let title = tracked.title();
        if !always && title.is_empty() && tracked.app_id.is_empty() {
            return;
        }
        let id = tracked.id;
        let app_id = tracked.app_id.clone();
        out.push(SurfaceEvent::Metadata {
            id,
            title: bounded_field(&title, TITLE_CAP),
            app_id: bounded_field(&app_id, APP_ID_CAP),
        });
    }

    /// Creates a Damage object on a window, watching every raw rectangle. `RawRectangles`
    /// reports at damage time, so nothing is lost between a notification and the subtract
    /// that clears the server-side region (see `drain_events`).
    fn create_damage(&mut self, window: x::Window) -> Option<damage::Damage> {
        let damage = self.conn.generate_id().ok()?;
        match self
            .conn
            .damage_create(damage, window, damage::ReportLevel::RAW_RECTANGLES)
        {
            Ok(cookie) => cookie.check().is_ok().then_some(damage),
            Err(_) => None,
        }
    }

    // --- the event handlers ----------------------------------------------------------------

    /// One event from the server, translated into bookkeeping and surface events.
    // The Event is taken by value and torn apart by the match; clippy misreads the
    // destructure as non-consuming.
    #[allow(clippy::needless_pass_by_value)]
    fn handle_event(
        &mut self,
        event: Event,
        out: &mut Vec<SurfaceEvent>,
    ) -> Result<(), BackendError> {
        // Any client can SendEvent to the root with the masks the backend selects there.
        // Only what ICCCM has clients send is believed: the synthetic UnmapNotify of a
        // withdrawal, client messages, and the SelectionNotify an owner owes a requestor
        // (ourselves, when the fetch of the app's clipboard asks). A forged ConfigureNotify
        // must not move root_size, the bound every clamp uses; a forged DestroyNotify must
        // not drop a live window; a forged SelectionNotify can at worst make the backend
        // read a property on its own window, bounded and type-checked like any fetch reply.
        if event.sent_event()
            && !matches!(
                event,
                Event::UnmapNotify(_) | Event::ClientMessage(_) | Event::SelectionNotify(_)
            )
        {
            return Ok(());
        }
        match event {
            Event::MapRequest(e) => self.manage_window(e.window, true, out),
            Event::MapNotify(e) => {
                // Decided from the map itself, not from creation: toolkits map and unmap
                // one menu window many times, a window may predate the connection, and
                // override_redirect may be set after creation. WM_TRANSIENT_FOR is read
                // now, after the client's own writes.
                if e.override_redirect {
                    self.show_popup(e.window, out)
                } else {
                    Ok(())
                }
            }
            Event::UnmapNotify(e) => {
                // A popup unmapping is the popup going away; a managed toplevel
                // unmapping is its surface going away too — a remap arrives as a new
                // MapRequest and a new surface.
                if self
                    .table
                    .get(e.window)
                    .is_some_and(|t| t.kind == WindowKind::Managed)
                {
                    self.set_wm_state(e.window, WM_STATE_WITHDRAWN);
                }
                self.destroy_window(e.window, out);
                Ok(())
            }
            Event::DestroyNotify(e) => {
                self.destroy_window(e.window, out);
                Ok(())
            }
            Event::ReparentNotify(e) => {
                // A window reparented off the root is no longer ours to show.
                if e.parent != self.root {
                    self.destroy_window(e.window, out);
                }
                Ok(())
            }
            Event::ConfigureNotify(e) => {
                self.configure_notify(e, out);
                Ok(())
            }
            Event::ConfigureRequest(e) => self.configure_request(e, out),
            Event::PropertyNotify(e) => self.property_notify(e, out),
            Event::ClientMessage(e) => {
                self.client_message(e, out);
                Ok(())
            }
            Event::DamageNotify(e) => {
                self.damage_notify(e, out);
                Ok(())
            }
            Event::SelectionRequest(e) => self.selection_request(e, out),
            Event::SelectionNotify(e) => self.selection_notify(e, out),
            Event::SelectionClear(e) => {
                if e.selection == self.atoms.clipboard {
                    self.clipboard.text = None;
                }
                Ok(())
            }
            Event::XfixesSelectionNotify(e) => {
                if e.selection == self.atoms.clipboard && e.owner != self.owner_window {
                    // Another client took the selection; the host's text is stale.
                    self.clipboard.text = None;
                    if e.owner == NONE {
                        // Nobody owns the selection; no fetch has anything to ask.
                        self.clipboard.fetch = None;
                    } else {
                        // The app copied: ask the new owner for its UTF-8 text.
                        self.start_clipboard_fetch()?;
                    }
                } else if e.selection == self.atoms.clipboard {
                    // The backend took it: the time the server recorded answers TIMESTAMP,
                    // and any fetch of a previous owner's text is over.
                    self.clipboard.acquired = e.selection_timestamp;
                    self.clipboard.fetch = None;
                }
                Ok(())
            }
            Event::XfixesCursorNotify(_) => {
                let reply = self.conn.xfixes_get_cursor_image()?;
                match reply.reply() {
                    Ok(reply) => {
                        out.push(SurfaceEvent::CursorChanged {
                            cursor: cursor_image(
                                reply.cursor_serial,
                                reply.width,
                                reply.height,
                                reply.xhot,
                                reply.yhot,
                                &reply.cursor_image,
                            ),
                        });
                    }
                    // The grab races moments the display is still fine through, and the
                    // server answers a refusal, not a fault: a Cursor error (the one the
                    // request is specified to carry — the cursor hidden, the pointer off
                    // the screens) or an Access refusal (observed from an X server when a
                    // cursor another client has just set becomes the displayed one;
                    // reproduced with xsetroot, 2026-09-29). Either way this update is
                    // skipped, the next change re-reports, and the session lives on.
                    // Treated as errors they once ended live sessions as a "dead
                    // display", with a log line that pointed at an X server that was
                    // alive.
                    Err(ReplyError::X11Error(e)) if is_grab_refusal(e.error_kind) => {}
                    Err(e) => return Err(e.into()),
                }
                Ok(())
            }
            Event::MappingNotify(_) => self.refresh_keymap(),
            _ => Ok(()),
        }
    }

    /// A tracked window's geometry changed. A size change frees the stale named pixmap, and
    /// a change of the size the surface is reported at is a `Resized`.
    fn configure_notify(&mut self, e: x::ConfigureNotifyEvent, out: &mut Vec<SurfaceEvent>) {
        if e.window == self.root {
            // Only a server-generated event gets here (see handle_event).
            self.root_size = Size::new(u32::from(e.width), u32::from(e.height));
            return;
        }
        let bound = size_bound(self.root_size);
        let Some(tracked) = self.table.get_mut(e.window) else {
            return;
        };
        let new_size = Size::new(u32::from(e.width), u32::from(e.height));
        let old_size = tracked.geometry.size;
        let resized = tracked.size_changed(new_size);
        tracked.border = e.border_width;
        tracked.geometry = inner_geometry(
            Rect::new(
                i32::from(e.x),
                i32::from(e.y),
                new_size.width,
                new_size.height,
            ),
            e.border_width,
        );
        if resized
            && let Some((pixmap, _)) = tracked.pixmap.take()
            && let Ok(cookie) = self.conn.free_pixmap(pixmap)
        {
            // The window has a new backing pixmap now. The named one keeps the old one
            // alive until it is freed (Composite), so it is freed here, without waiting,
            // and named again at the next capture.
            cookie.ignore_error();
        }
        let reported = bounded_size(new_size, bound);
        if reported != bounded_size(old_size, bound) {
            out.push(SurfaceEvent::Resized {
                id: tracked.id,
                size: reported,
            });
        }
    }

    /// A client asking to move, resize, restack or re-border its own toplevel.
    ///
    /// The host decides size, position and stacking (ADR-0003 §5). Before the window is
    /// announced — no surface was created for it yet, so the host has seen nothing — its
    /// size is granted, cut to the root and the wire's caps: the toolkit is still building
    /// the window. After that, nothing is applied. The app hears its unchanged geometry in
    /// the ICCCM synthetic `ConfigureNotify`, and a request that names a width or height
    /// different from the current size is reported as `ResizeRequested`, for the host to
    /// answer with a configure or not at all. Position and stacking are never granted.
    fn configure_request(
        &mut self,
        e: x::ConfigureRequestEvent,
        out: &mut Vec<SurfaceEvent>,
    ) -> Result<(), BackendError> {
        let wants = |bit: x::ConfigWindow| e.value_mask & bit == bit;
        let bound = size_bound(self.root_size);
        let Some(tracked) = self.table.get(e.window) else {
            // Not announced: the size only, bounded, and never a border.
            let mut aux = x::ConfigureWindowAux::new().border_width(0);
            if wants(x::ConfigWindow::WIDTH) {
                aux = aux.width(u32::from(e.width).min(bound.width).max(1));
            }
            if wants(x::ConfigWindow::HEIGHT) {
                aux = aux.height(u32::from(e.height).min(bound.height).max(1));
            }
            check_gone(self.conn.configure_window(e.window, &aux)?)?;
            return Ok(());
        };

        let id = tracked.id;
        let geometry = tracked.geometry;
        let asked = Size::new(
            if wants(x::ConfigWindow::WIDTH) {
                u32::from(e.width)
            } else {
                geometry.size.width
            },
            if wants(x::ConfigWindow::HEIGHT) {
                u32::from(e.height)
            } else {
                geometry.size.height
            },
        );
        let size = tracked.hints.clamp(asked, bound);
        self.send_synthetic_configure(e.window, geometry)?;
        let asks_size = wants(x::ConfigWindow::WIDTH) || wants(x::ConfigWindow::HEIGHT);
        if asks_size && size != geometry.size {
            out.push(SurfaceEvent::ResizeRequested { id, size });
        }
        Ok(())
    }

    /// Sends the ICCCM synthetic `ConfigureNotify` naming `geometry`, which some clients
    /// wait for: after a host configure, and after a request the WM did not apply.
    fn send_synthetic_configure(
        &self,
        window: x::Window,
        geometry: Rect,
    ) -> Result<(), BackendError> {
        let notify = x::ConfigureNotifyEvent {
            response_type: x::CONFIGURE_NOTIFY_EVENT,
            sequence: 0,
            event: window,
            window,
            above_sibling: NONE,
            x: clamp_i16(geometry.origin.x),
            y: clamp_i16(geometry.origin.y),
            width: clamp_u16(geometry.size.width),
            height: clamp_u16(geometry.size.height),
            border_width: 0,
            override_redirect: false,
        };
        check_gone(
            self.conn
                .send_event(false, window, x::EventMask::STRUCTURE_NOTIFY, notify)?,
        )
    }

    /// Titles, app ids, protocols, size hints and the EWMH active window, read on change.
    fn property_notify(
        &mut self,
        e: x::PropertyNotifyEvent,
        out: &mut Vec<SurfaceEvent>,
    ) -> Result<(), BackendError> {
        let net_active = self.atoms.net_active_window;
        if e.window == self.root && e.atom == net_active {
            // A client set the property instead of sending the message.
            if let Some(reply) = self.read_property(self.root, net_active, 1)? {
                let window = reply
                    .value
                    .chunks_exact(4)
                    .next()
                    .map_or(NONE, |w| u32::from_ne_bytes([w[0], w[1], w[2], w[3]]));
                if let Some(tracked) = self.table.get(window) {
                    out.push(SurfaceEvent::FocusRequested { id: tracked.id });
                }
            }
            return Ok(());
        }

        let atoms = &self.atoms;
        let Some(tracked) = self.table.get_mut(e.window) else {
            return Ok(());
        };
        if tracked.kind != WindowKind::Managed {
            return Ok(());
        }
        // WM_TRANSIENT_FOR is deliberately absent from what follows: a dialog's parent is
        // read once, at map (see manage_window), and the v0 wire has no message that could
        // announce a parent changing later — SurfaceNew is the only parent-carrying message
        // and it announces a surface once. A client that sets the property after mapping
        // keeps whatever map time saw.
        let atom = e.atom;
        if atom == atoms.net_wm_name || atom == atoms.wm_name || atom == atoms.wm_class {
            // Re-read the trio so a Metadata event carries both fields coherently. A change
            // is reported even when it empties both: the host must drop the old title.
            let before = (tracked.title(), tracked.app_id.clone());
            let state = self.read_window_state(e.window)?;
            let Some(tracked) = self.table.get_mut(e.window) else {
                return Ok(());
            };
            tracked.title_net = state.title_net;
            tracked.title_name = state.title_name;
            tracked.app_id = state.app_id;
            if (tracked.title(), tracked.app_id.clone()) != before {
                self.report_metadata(e.window, true, out);
            }
            return Ok(());
        }
        let is_protocols = atom == atoms.wm_protocols;
        let is_hints = atom == atoms.wm_normal_hints;
        if !is_protocols && !is_hints {
            return Ok(());
        }
        let _ = tracked;
        let reply = if is_protocols {
            self.read_property(e.window, atoms.wm_protocols, 16)?
        } else {
            self.read_property(e.window, atoms.wm_normal_hints, 18)?
        };
        let Some(tracked) = self.table.get_mut(e.window) else {
            return Ok(());
        };
        if is_protocols {
            if let Some(reply) = reply {
                let has = |atom: u32| {
                    reply
                        .value
                        .chunks_exact(4)
                        .any(|w| u32::from_ne_bytes([w[0], w[1], w[2], w[3]]) == atom)
                };
                tracked.has_delete = has(atoms.wm_delete_window);
                tracked.take_focus = has(atoms.wm_take_focus);
            }
        } else if let Some(reply) = reply {
            tracked.hints = SizeHints::parse(&reply.value);
        }
        Ok(())
    }

    /// Focus asks: `_NET_ACTIVE_WINDOW`, and a client sending itself `WM_TAKE_FOCUS`.
    fn client_message(&mut self, e: x::ClientMessageEvent, out: &mut Vec<SurfaceEvent>) {
        let atoms = &self.atoms;
        let data = e.data.as_data32();
        if e.type_ == atoms.net_active_window
            && e.window == self.root
            && let Some(&window) = data.get(2)
            && let Some(tracked) = self.table.get(window)
        {
            out.push(SurfaceEvent::FocusRequested { id: tracked.id });
            return;
        }
        if e.type_ == atoms.wm_protocols
            && data.first() == Some(&atoms.wm_take_focus)
            && let Some(tracked) = self.table.get(e.window)
        {
            out.push(SurfaceEvent::FocusRequested { id: tracked.id });
        }
    }

    /// Damage happened on a tracked window: report it clipped to the size the surface is
    /// reported at. `drain_events` clears the server-side region afterwards.
    fn damage_notify(&mut self, e: damage::NotifyEvent, out: &mut Vec<SurfaceEvent>) {
        let rect = Rect::new(
            i32::from(e.area.x),
            i32::from(e.area.y),
            u32::from(e.area.width),
            u32::from(e.area.height),
        );
        let bound = size_bound(self.root_size);
        if let Some(tracked) = self.table.get_mut(e.drawable) {
            let size = bounded_size(tracked.geometry.size, bound);
            let bounds = Rect::new(0, 0, size.width, size.height);
            if let Some(inside) = rect.intersection(bounds) {
                let id = tracked.id;
                out.push(SurfaceEvent::Damaged { id, rect: inside });
            }
        }
    }

    // --- selections ------------------------------------------------------------------------

    /// Answers a `SelectionRequest` against the CLIPBOARD the backend owns.
    fn selection_request(
        &mut self,
        e: x::SelectionRequestEvent,
        out: &mut Vec<SurfaceEvent>,
    ) -> Result<(), BackendError> {
        let atoms = &self.atoms;
        // ICCCM: a request with no property names its target as the reply property.
        let property = if e.property == NONE {
            e.target
        } else {
            e.property
        };
        let is_text_target = e.target == atoms.utf8_string
            || e.target == atoms.text
            || e.target == atoms.string
            || e.target == atoms.text_plain_utf8;

        let answer = if e.selection != atoms.clipboard {
            None
        } else if e.target == atoms.targets {
            let targets: [u32; 6] = [
                atoms.targets,
                atoms.timestamp,
                atoms.utf8_string,
                atoms.text,
                atoms.string,
                atoms.text_plain_utf8,
            ];
            self.write_property32(e.requestor, property, XA_ATOM, &targets);
            Some(property)
        } else if e.target == atoms.timestamp {
            let acquired = self.clipboard.acquired;
            self.write_property32(e.requestor, property, XA_INTEGER, &[acquired]);
            Some(property)
        } else if let Some(text) = self.clipboard.text.clone() {
            let (type_, data) =
                if e.target == atoms.utf8_string || e.target == atoms.text_plain_utf8 {
                    (atoms.utf8_string, text.into_bytes())
                } else if e.target == atoms.text || e.target == atoms.string {
                    (atoms.string, latin1_encode(&text))
                } else {
                    // COMPOUND_TEXT, MULTIPLE and anything else: refused, not half-served.
                    (0, Vec::new())
                };
            if data.is_empty() {
                None
            } else {
                self.write_property(e.requestor, property, type_, &data);
                Some(property)
            }
        } else if is_text_target {
            // The app pasted and the backend holds nothing to serve: report it and refuse
            // this one paste.
            out.push(SurfaceEvent::ClipboardRequested);
            None
        } else {
            None
        };

        let notify = x::SelectionNotifyEvent {
            response_type: x::SELECTION_NOTIFY_EVENT,
            sequence: 0,
            requestor: e.requestor,
            selection: e.selection,
            target: e.target,
            property: answer.unwrap_or(NONE),
            time: e.time,
        };
        if let Ok(cookie) = self
            .conn
            .send_event(false, e.requestor, x::EventMask::NO_EVENT, notify)
        {
            let _ = cookie.check();
        }
        self.conn.flush()?;
        Ok(())
    }

    /// Asks the current CLIPBOARD owner for its `UTF8_STRING`, into the backend's own fetch
    /// property, and starts the deadline the answer must beat.
    ///
    /// Called when an XFixes notification says another client took the selection: the app
    /// copied. The backend never blocks on the answer — it arrives as a `SelectionNotify`
    /// event the next drains pick up, or it does not, and [`ClipboardFetch::deadline`]
    /// bounds how long the fetch is still believed live.
    fn start_clipboard_fetch(&mut self) -> Result<(), BackendError> {
        self.clipboard.fetch = Some(ClipboardFetch {
            deadline: Instant::now() + CLIPBOARD_FETCH_TIMEOUT,
        });
        self.conn
            .convert_selection(
                self.owner_window,
                self.atoms.clipboard,
                self.atoms.utf8_string,
                self.atoms.clipboard_fetch,
                CURRENT_TIME,
            )?
            .check()?;
        self.conn.flush()?;
        Ok(())
    }

    /// The answer to the backend's own selection request: the app's clipboard text, when the
    /// fetch is live, the answer names it, and the property is a carriable `UTF8_STRING`.
    ///
    /// Anything else — a stale answer, a refusal, a different type (an `INCR` the wire never
    /// asked for), a text over `MAX_CLIPBOARD_BYTES`, bytes that are not UTF-8 — sends
    /// nothing and disturbs nothing; the property is still deleted, so a hostile owner
    /// cannot leave data on the backend's window.
    fn selection_notify(
        &mut self,
        e: x::SelectionNotifyEvent,
        out: &mut Vec<SurfaceEvent>,
    ) -> Result<(), BackendError> {
        if self.clipboard.fetch.is_none()
            || e.requestor != self.owner_window
            || e.selection != self.atoms.clipboard
            || e.target != self.atoms.utf8_string
        {
            return Ok(()); // not ours, or an answer to a fetch already given up
        }
        self.clipboard.fetch = None;
        if e.property == NONE {
            return Ok(()); // refused, or the owner holds no UTF-8 text
        }
        // Bounded in longs: one byte past the cap, enough to tell carriable from not.
        let reply = self
            .conn
            .get_property(
                true,
                self.owner_window,
                e.property,
                XA_ANY,
                0,
                CLIPBOARD_FETCH_LONGS,
            )?
            .reply()?;
        if reply.type_ != self.atoms.utf8_string
            || reply.format != 8
            || reply.value.len() > MAX_CLIPBOARD_BYTES
        {
            return Ok(());
        }
        // Untrusted text, capped; the session decides whether the host already has it.
        // Invalid UTF-8 is dropped whole: the wire message the text feeds is UTF-8 only.
        if let Ok(text) = String::from_utf8(reply.value) {
            out.push(SurfaceEvent::ClipboardText { text });
        }
        Ok(())
    }

    /// Writes a reply property; a gone requestor ends the exchange, it does not fail it.
    fn write_property(&self, requestor: x::Window, property: u32, type_: u32, data: &[u8]) -> bool {
        match self.conn.change_property(
            x::PropMode::REPLACE,
            requestor,
            property,
            type_,
            8,
            u32::try_from(data.len()).unwrap_or(u32::MAX),
            data,
        ) {
            Ok(cookie) => cookie.check().is_ok(),
            Err(_) => false,
        }
    }

    /// Writes a reply property of 32-bit values (`ATOM`, `INTEGER`): format 32, so the
    /// server byte-swaps it for a requestor of the other byte order. The length is the
    /// number of values, not of bytes.
    fn write_property32(
        &self,
        requestor: x::Window,
        property: u32,
        type_: u32,
        values: &[u32],
    ) -> bool {
        let data: Vec<u8> = values.iter().flat_map(|v| v.to_ne_bytes()).collect();
        match self.conn.change_property(
            x::PropMode::REPLACE,
            requestor,
            property,
            type_,
            32,
            u32::try_from(values.len()).unwrap_or(u32::MAX),
            &data,
        ) {
            Ok(cookie) => cookie.check().is_ok(),
            Err(_) => false,
        }
    }

    // --- input -----------------------------------------------------------------------------

    /// The toplevel whose window holds the keyboard for surface `id`: the surface itself, or
    /// a popup's parent toplevel (a popup takes pointer events itself, but the keyboard
    /// stays with its parent).
    fn keyboard_owner(&self, id: SurfaceId) -> Option<SurfaceId> {
        let tracked = self.table.by_surface(id)?;
        let owner = match (&tracked.kind, &tracked.role) {
            (WindowKind::OverrideRedirect, Role::Popup { parent, .. }) => self
                .table
                .by_surface(*parent)
                .filter(|p| p.kind == WindowKind::Managed)
                .unwrap_or(tracked),
            _ => tracked,
        };
        Some(owner.id)
    }

    /// Raises surface `id` above its siblings. An XTEST click lands on whichever window is
    /// on top at its point (docs/architecture.md §3.2), and toplevels can overlap, so the
    /// target goes on top first. The host decides stacking; this is the only place the
    /// backend changes it.
    fn raise(&mut self, id: SurfaceId) -> Result<(), BackendError> {
        let Some(tracked) = self.table.by_surface(id) else {
            return Err(BackendError::UnknownSurface(id));
        };
        // Not checked: requests run in order, so the raise lands before the XTEST event
        // that follows it, and a window gone meanwhile has nothing left to raise.
        self.conn
            .configure_window(
                tracked.window,
                &x::ConfigureWindowAux::new().stack_mode(x::StackMode::ABOVE),
            )?
            .ignore_error();
        self.ensured = Some(id);
        self.raised_at = self.table.peek_next_surface();
        Ok(())
    }

    /// Raises surface `id` unless pointer input already raised it and no window has been
    /// tracked since, which could sit above it.
    fn raise_if_stale(&mut self, id: SurfaceId) -> Result<(), BackendError> {
        if self.ensured == Some(id) && self.raised_at == self.table.peek_next_surface() {
            return Ok(());
        }
        self.raise(id)
    }

    /// Gives toplevel `target` the X keyboard focus, unless this backend already did. Keys
    /// reach the focused window (docs/architecture.md §3.2).
    fn give_keyboard(&mut self, target: SurfaceId) -> Result<(), BackendError> {
        if self.keyboard == Some(target) {
            return Ok(());
        }
        let Some(tracked) = self.table.by_surface(target) else {
            return Err(BackendError::UnknownSurface(target));
        };
        let (window, take_focus) = (tracked.window, tracked.take_focus);
        check_gone(self.conn.set_input_focus(
            x::InputFocus::POINTER_ROOT,
            window,
            CURRENT_TIME,
        )?)?;
        if take_focus {
            // The ICCCM focus handshake for clients that asked for it.
            self.send_protocols_message(window, self.atoms.wm_take_focus)?;
        }
        self.keyboard = Some(target);
        Ok(())
    }

    /// Moves the X pointer to local point `local` of surface `id`.
    fn move_pointer(
        &mut self,
        id: SurfaceId,
        geometry: Rect,
        local: Point,
    ) -> Result<(), BackendError> {
        let root_at = Point::new(
            geometry.origin.x.saturating_add(local.x),
            geometry.origin.y.saturating_add(local.y),
        );
        self.xtest_motion(root_at)?;
        self.pointer_at = Some((id, local, root_at));
        Ok(())
    }

    /// Puts the X pointer inside surface `id` before a button or wheel event, which carries
    /// no position and lands wherever the pointer is. The pointer stays where the last
    /// motion into `id` left it. After motion elsewhere, or none at all (a touch tap sends
    /// no motion before its press), it goes to the middle of `id`.
    fn pointer_into(&mut self, id: SurfaceId, geometry: Rect) -> Result<(), BackendError> {
        let local = match self.pointer_at {
            Some((at, local, _)) if at == id => clamp_inside(local, geometry.size),
            _ => Point::new(
                i32::try_from(geometry.size.width / 2).unwrap_or(0),
                i32::try_from(geometry.size.height / 2).unwrap_or(0),
            ),
        };
        let root_at = Point::new(
            geometry.origin.x.saturating_add(local.x),
            geometry.origin.y.saturating_add(local.y),
        );
        if self.pointer_at == Some((id, local, root_at)) {
            return Ok(());
        }
        self.move_pointer(id, geometry, local)
    }

    /// Releases every key and button the X server reports down. A new backend's tally is
    /// empty, so nothing else could release what a previous one held when it died; and a
    /// tally that lost count leaves nothing stuck after a blur either. With `unlock`, a Caps
    /// Lock left on is turned off too: the backend consumes Caps Lock (see the input
    /// module), so X's Lock must stay off.
    fn release_input_left_down(&mut self, unlock: bool) -> Result<(), BackendError> {
        let keys = self.conn.query_keymap()?;
        let pointer = self.conn.query_pointer(self.root)?;
        let keys = keys.reply()?.keys;
        let mask = pointer.reply()?.mask;
        for (byte, bits) in (0u8..).zip(keys) {
            for bit in 0..8u8 {
                if bits & (1 << bit) != 0 {
                    self.xtest_key(byte.wrapping_mul(8).wrapping_add(bit), PressState::Released)?;
                }
            }
        }
        let buttons = [
            (BUTTON_LEFT, x::KeyButMask::BUTTON1),
            (BUTTON_MIDDLE, x::KeyButMask::BUTTON2),
            (BUTTON_RIGHT, x::KeyButMask::BUTTON3),
            (WHEEL_UP, x::KeyButMask::BUTTON4),
            (WHEEL_DOWN, x::KeyButMask::BUTTON5),
        ];
        for (button, bit) in buttons {
            if mask.contains(bit) {
                self.xtest_button(button, PressState::Released)?;
            }
        }
        if unlock
            && mask.contains(x::KeyButMask::LOCK)
            && let Some(caps) = self.keymap.resolve(XK_CAPS_LOCK)
        {
            self.xtest_key(caps.keycode, PressState::Pressed)?;
            self.xtest_key(caps.keycode, PressState::Released)?;
        }
        self.conn.flush()?;
        Ok(())
    }

    /// Sends a `WM_PROTOCOLS` client message carrying `protocol` to `window`.
    fn send_protocols_message(
        &mut self,
        window: x::Window,
        protocol: u32,
    ) -> Result<(), BackendError> {
        let event = x::ClientMessageEvent {
            response_type: x::CLIENT_MESSAGE_EVENT,
            sequence: 0,
            format: 32,
            window,
            type_: self.atoms.wm_protocols,
            data: x::ClientMessageData::from([protocol, CURRENT_TIME, 0, 0, 0]),
        };
        self.conn
            .send_event(false, window, x::EventMask::NO_EVENT, event)?
            .check()?;
        Ok(())
    }

    /// Reads the keyboard mapping again.
    fn refresh_keymap(&mut self) -> Result<(), BackendError> {
        self.keymap = fetch_keymap(&self.conn)?;
        Ok(())
    }

    /// Binds `keysym` onto an unused keycode and returns it, for a press no modifier state
    /// reaches — the technique VNC servers use. The row's first two columns both carry the
    /// keysym, so the press needs no Shift choreography, and the server broadcasts a
    /// `MappingNotify` so the apps refetch. `requested` names the wire keysym in the error
    /// when no spare keycode exists.
    fn bind_spare(&mut self, keysym: u32, requested: u32) -> Result<u8, BackendError> {
        let taken: Vec<u8> = self.rebound.iter().map(|(k, _)| *k).collect();
        let Some(spare) = self.keymap.spare_keycode(&taken) else {
            return Err(BackendError::KeysymUnavailable(requested));
        };
        let width = self.keymap.width();
        let mut row = vec![0u32; usize::from(width)];
        row[0] = keysym;
        if let Some(second) = row.get_mut(1) {
            *second = keysym;
        }
        self.conn
            .change_keyboard_mapping(1, spare, width, &row)?
            .check()?;
        self.conn.flush()?;
        self.rebound.push((spare, keysym));
        Ok(spare)
    }

    /// Writes an empty row back over `keycode`, ending its rebind.
    fn restore_spare(&mut self, keycode: u8) -> Result<(), BackendError> {
        let width = self.keymap.width();
        let row = vec![0u32; usize::from(width)];
        self.conn
            .change_keyboard_mapping(1, keycode, width, &row)?
            .check()?;
        self.rebound.retain(|(k, _)| *k != keycode);
        self.conn.flush()?;
        Ok(())
    }

    /// Types and releases one dead key of a sequence, with the text modifier choreography
    /// around it, before the base key goes down. A tap that needs a rebind keeps it until
    /// the sequence's key comes up: the binding is restored with the base's, so a client
    /// reading the keymap at any point of the sequence sees each rebound row as it is.
    fn deliver_tap(&mut self, tap: Step) -> Result<(), BackendError> {
        let (keysym, keycode, shift, level3) = match tap {
            Step::Direct {
                keysym,
                keycode,
                shift,
                level3,
            } => (
                keysym,
                keycode,
                shift,
                if level3 {
                    self.keymap.level3_keycode()
                } else {
                    None
                },
            ),
            Step::Rebind { keysym } => (keysym, self.bind_spare(keysym, keysym)?, false, None),
        };
        let press = KeyPress {
            keysym,
            code: None,
            kind: KeyKind::Character,
            keycode,
            shifted: shift,
            level3,
        };
        let shift_key = self.keymap.shift_keycode();
        let strokes = self
            .held
            .press_key(KeyId::Keycode(keycode), press, shift_key)
            .map_err(|NoShiftKey| BackendError::KeysymUnavailable(XK_SHIFT_L))?;
        for stroke in strokes {
            self.xtest_stroke(stroke)?;
        }
        for stroke in self.held.release_key(&KeyId::Keycode(keycode)) {
            self.xtest_stroke(stroke)?;
        }
        self.conn.flush()?;
        Ok(())
    }

    // The XTEST events are written, not waited on: a checked request is a round trip, and
    // one wheel message used to cost two per step. The caller flushes once at the end.
    // Requests on one connection run in order, so nothing is reordered; an error the
    // server answers one with (a keycode outside the keymap) is dropped.

    /// One XTEST key event.
    fn xtest_key(&mut self, keycode: u8, state: PressState) -> Result<(), BackendError> {
        let kind = match state {
            PressState::Pressed => X_KEY_PRESS,
            PressState::Released => X_KEY_RELEASE,
        };
        self.conn
            .xtest_fake_input(kind, keycode, CURRENT_TIME, self.root, 0, 0, 0)?
            .ignore_error();
        Ok(())
    }

    /// One XTEST key stroke.
    fn xtest_stroke(&mut self, stroke: Stroke) -> Result<(), BackendError> {
        match stroke {
            Stroke::Down(keycode) => self.xtest_key(keycode, PressState::Pressed),
            Stroke::Up(keycode) => self.xtest_key(keycode, PressState::Released),
        }
    }

    /// One XTEST button event.
    fn xtest_button(&mut self, button: u8, state: PressState) -> Result<(), BackendError> {
        let kind = match state {
            PressState::Pressed => X_BUTTON_PRESS,
            PressState::Released => X_BUTTON_RELEASE,
        };
        self.conn
            .xtest_fake_input(kind, button, CURRENT_TIME, self.root, 0, 0, 0)?
            .ignore_error();
        Ok(())
    }

    /// One XTEST motion to root coordinates.
    fn xtest_motion(&mut self, at: Point) -> Result<(), BackendError> {
        self.conn
            .xtest_fake_input(
                X_MOTION_NOTIFY,
                0,
                CURRENT_TIME,
                self.root,
                clamp_i16(at.x),
                clamp_i16(at.y),
                0,
            )?
            .ignore_error();
        Ok(())
    }

    /// The bytes per pixel the server stores a `depth` pixmap at; 0 when the server does
    /// not list the depth, which the converter refuses.
    fn bpp_for_depth(&self, depth: u8) -> usize {
        self.pixel_bpp
            .iter()
            .find(|(d, _)| *d == depth)
            .map_or(0, |(_, bpp)| *bpp)
    }
    /// Names the window's Composite pixmap at its current size, freeing any older one.
    /// `None` means the window is gone or not yet mapped.
    fn name_pixmap(
        &mut self,
        window: x::Window,
        size: Size,
    ) -> Result<Option<x::Pixmap>, BackendError> {
        if let Some((old, _)) = self.table.get(window).and_then(|t| t.pixmap)
            && let Ok(cookie) = self.conn.free_pixmap(old)
        {
            let _ = cookie.check();
        }
        let pixmap = self.conn.generate_id()?;
        match self.conn.composite_name_window_pixmap(window, pixmap) {
            Ok(cookie) => {
                if cookie.check().is_ok() {
                    if let Some(tracked) = self.table.get_mut(window) {
                        tracked.pixmap = Some((pixmap, size));
                    }
                    Ok(Some(pixmap))
                } else {
                    Ok(None)
                }
            }
            Err(_) => Ok(None),
        }
    }
}

impl CaptureBackend for X11Backend {
    type Error = BackendError;

    /// Moves what is ready into `out`: at most 512 X events per call, so a flood cannot
    /// keep the caller from its other work. The rest stays queued for the next call.
    fn drain_events(&mut self, out: &mut Vec<SurfaceEvent>) -> Result<(), Self::Error> {
        out.append(&mut self.pending);
        // A fetch past its deadline is over: an app that never answered cannot hold one open.
        if self
            .clipboard
            .fetch
            .is_some_and(|fetch| Instant::now() > fetch.deadline)
        {
            self.clipboard.fetch = None;
        }
        self.conn.flush()?;
        let mut damaged: Vec<damage::Damage> = Vec::new();
        for _ in 0..MAX_EVENTS_PER_DRAIN {
            let Some(event) = self.conn.poll_for_event()? else {
                break;
            };
            if let Event::DamageNotify(e) = &event
                && !event.sent_event()
                && !damaged.contains(&e.damage)
            {
                damaged.push(e.damage);
            }
            self.handle_event(event, out)?;
        }
        // Clear each reporting damage object's server-side region, once per drain and
        // without waiting, so it cannot grow without bound. New damage still reports:
        // RawRectangles events are generated when the damage happens, not when the region
        // empties. The error for a window destroyed meanwhile is dropped.
        for damage in damaged {
            self.conn
                .damage_subtract(damage, NONE, NONE)?
                .ignore_error();
        }
        self.conn.flush()?;
        Ok(())
    }

    fn root_size(&mut self) -> Result<Size, Self::Error> {
        Ok(self.root_size)
    }

    /// Copies `rect` of surface `id`. The buffer is always exactly `rect.size` (C2): the
    /// part of `rect` outside the window's current geometry — a tile planned before a
    /// shrink reached the session — is zeros. A `rect` past the wire's surface caps breaks
    /// the trait contract (it is at most one tile) and is cut to them, which keeps the
    /// zero fill bounded.
    fn capture(&mut self, id: SurfaceId, rect: Rect) -> Result<PixelBuffer, Self::Error> {
        let Some(tracked) = self.table.by_surface(id) else {
            return Err(BackendError::UnknownSurface(id));
        };
        let rect = Rect::new(
            rect.origin.x,
            rect.origin.y,
            rect.size.width.min(MAX_SURFACE_WIDTH),
            rect.size.height.min(MAX_SURFACE_HEIGHT),
        );
        let window = tracked.window;
        let border = i32::from(tracked.border);
        let surface_size = tracked.geometry.size;
        let bounds = Rect::new(0, 0, surface_size.width, surface_size.height);
        let Some(clamped) = rect.intersection(bounds) else {
            return Ok(zeroed(rect.size));
        };

        // The named pixmap tracks the window's size; a resize freed it, so name again.
        let current = self
            .table
            .get(window)
            .and_then(|t| t.pixmap)
            .filter(|(_, size)| *size == surface_size);
        let pixmap = if let Some((pixmap, _)) = current {
            pixmap
        } else {
            let Some(pixmap) = self.name_pixmap(window, surface_size)? else {
                return Err(BackendError::UnknownSurface(id));
            };
            pixmap
        };

        // The named pixmap includes the border; the surface starts inside it.
        let reply = self
            .conn
            .get_image(
                x::ImageFormat::Z_PIXMAP,
                pixmap,
                clamp_i16(clamped.origin.x.saturating_add(border)),
                clamp_i16(clamped.origin.y.saturating_add(border)),
                clamp_u16(clamped.size.width),
                clamp_u16(clamped.size.height),
                !0,
            )?
            .reply()?;
        let bpp = self.bpp_for_depth(reply.depth);
        let part = convert_zpixmap(reply.depth, &reply.data, clamped.size, bpp)?;
        Ok(place_in(part, rect, clamped.origin))
    }
}

impl InputSink for X11Backend {
    type Error = BackendError;

    /// Pointer input raises its surface but never moves the keyboard focus: keys follow
    /// the host's focus alone (docs/protocol/v0.md §4.4).
    fn pointer_motion(&mut self, id: SurfaceId, at: Point) -> Result<(), Self::Error> {
        let Some(tracked) = self.table.by_surface(id) else {
            return Err(BackendError::UnknownSurface(id));
        };
        let geometry = tracked.geometry;
        self.raise_if_stale(id)?;
        self.focused = self.keyboard_owner(id);
        self.move_pointer(id, geometry, clamp_inside(at, geometry.size))?;
        self.conn.flush()?;
        Ok(())
    }

    fn pointer_button(
        &mut self,
        id: SurfaceId,
        button: PointerButton,
        state: PressState,
    ) -> Result<(), Self::Error> {
        let detail = match button {
            PointerButton::Left => BUTTON_LEFT,
            PointerButton::Middle => BUTTON_MIDDLE,
            PointerButton::Right => BUTTON_RIGHT,
        };
        match state {
            PressState::Pressed => {
                let Some(tracked) = self.table.by_surface(id) else {
                    return Err(BackendError::UnknownSurface(id));
                };
                let geometry = tracked.geometry;
                // Every press raises: a window mapped or restacked since the last raise
                // must not take this click. The release needs neither raise, position nor
                // even its surface: the press's implicit grab takes it to the same window,
                // and a window gone mid-drag must not leave the button down.
                self.raise(id)?;
                self.pointer_into(id, geometry)?;
                self.focused = self.keyboard_owner(id);
                self.held.button_press(detail);
            }
            PressState::Released => self.held.button_release(detail),
        }
        self.xtest_button(detail, state)?;
        self.conn.flush()?;
        Ok(())
    }

    /// At most [`MAX_POINTER_AXIS_STEPS`] steps per axis are sent (the wire's cap); the rest
    /// are dropped.
    fn pointer_axis(&mut self, id: SurfaceId, steps: Point) -> Result<(), Self::Error> {
        let Some(tracked) = self.table.by_surface(id) else {
            return Err(BackendError::UnknownSurface(id));
        };
        let geometry = tracked.geometry;
        self.raise_if_stale(id)?;
        self.pointer_into(id, geometry)?;
        self.focused = self.keyboard_owner(id);
        let vertical = if steps.y < 0 { WHEEL_UP } else { WHEEL_DOWN };
        let horizontal = if steps.x < 0 { WHEEL_LEFT } else { WHEEL_RIGHT };
        for _ in 0..steps.y.unsigned_abs().min(MAX_POINTER_AXIS_STEPS) {
            self.xtest_button(vertical, PressState::Pressed)?;
            self.xtest_button(vertical, PressState::Released)?;
        }
        for _ in 0..steps.x.unsigned_abs().min(MAX_POINTER_AXIS_STEPS) {
            self.xtest_button(horizontal, PressState::Pressed)?;
            self.xtest_button(horizontal, PressState::Released)?;
        }
        self.conn.flush()?;
        Ok(())
    }

    /// A press goes to the surface the host focused, and only there; with none focused it
    /// is dropped. The keysym is authoritative, so the X modifier state is set around each
    /// press to produce exactly it (see the input module), and a keysym the keymap holds
    /// only past the first two columns is typed by holding the level-3 key around the
    /// press or by rebinding an unused keycode for it (see the keymap module). A release
    /// comes off whatever its press put down, focus or not.
    fn key(&mut self, key: KeyEvent) -> Result<(), Self::Error> {
        let keysym = key.keysym.0;
        let kind = key_kind(keysym);
        if kind == KeyKind::Consumed {
            return Ok(());
        }
        let code = KeyId::usable_code(key.code.as_ref().map(KeyCode::as_str));
        let strokes = match key.state {
            PressState::Released => {
                let id = if let Some(code) = code {
                    KeyId::Code(code.to_owned())
                } else if let Some(keycode) = release_keycode(&self.keymap, keysym, &self.rebound) {
                    KeyId::Keycode(keycode)
                } else {
                    return Ok(()); // nothing was pressed under it
                };
                let strokes = self.held.release_key(&id);
                // A rebound keycode no held key keeps down ends its rebind: the base key
                // this release may have just freed, and any dead-key tap of its sequence,
                // whose binding waited for exactly that.
                let spent: Vec<u8> = self
                    .rebound
                    .iter()
                    .map(|(k, _)| *k)
                    .filter(|k| !self.held.holds_keycode(*k))
                    .collect();
                for keycode in spent {
                    self.restore_spare(keycode)?;
                }
                strokes
            }
            PressState::Pressed => {
                let Some(target) = self
                    .host_focused
                    .filter(|t| self.table.by_surface(*t).is_some())
                else {
                    return Ok(());
                };
                self.give_keyboard(target)?;
                let planned = plan(&self.keymap, keysym)?;
                if let Plan::Dead { taps, .. } = &planned {
                    for tap in taps.iter().copied() {
                        self.deliver_tap(tap)?;
                    }
                }
                let step = match &planned {
                    Plan::Key(step) => *step,
                    Plan::Dead { base, .. } => *base,
                };
                let (keycode, shift, level3) = match step {
                    Step::Direct {
                        keycode,
                        shift,
                        level3,
                        ..
                    } => (
                        keycode,
                        shift,
                        if level3 {
                            self.keymap.level3_keycode()
                        } else {
                            None
                        },
                    ),
                    // The keymap holds this keysym out of every modifier's reach: press
                    // it on a keycode rebound for as long as the key stays down.
                    Step::Rebind { keysym: bound } => {
                        (self.bind_spare(bound, keysym)?, false, None)
                    }
                };
                let id = code.map_or(KeyId::Keycode(keycode), |c| KeyId::Code(c.to_owned()));
                if let KeyKind::Modifier(modifier) = kind {
                    self.held.press_modifier(id, keycode, modifier)
                } else {
                    let press = KeyPress {
                        keysym,
                        code,
                        kind,
                        keycode,
                        shifted: shift,
                        level3,
                    };
                    let shift_key = self.keymap.shift_keycode();
                    self.held
                        .press_key(id, press, shift_key)
                        .map_err(|NoShiftKey| BackendError::KeysymUnavailable(XK_SHIFT_L))?
                }
            }
        };
        for stroke in strokes {
            self.xtest_stroke(stroke)?;
        }
        self.conn.flush()?;
        Ok(())
    }

    /// The host's focus: raises the surface and gives its toplevel (a popup's parent) the
    /// keyboard. This is the only call that moves the keyboard.
    fn focus(&mut self, id: SurfaceId) -> Result<(), Self::Error> {
        let Some(target) = self.keyboard_owner(id) else {
            return Err(BackendError::UnknownSurface(id));
        };
        self.raise(id)?;
        self.host_focused = Some(target);
        self.focused = Some(target);
        self.give_keyboard(target)?;
        self.conn.flush()?;
        Ok(())
    }

    /// Releases every key and button held, the modifiers included, restores every keycode
    /// a rebind still holds, then anything else X still reports down, and takes the
    /// keyboard focus off the app. The streamer calls it for `BlurRelease`, and when the
    /// socket is lost or the session ends. The next key press goes back to the surface the
    /// host last focused.
    fn blur(&mut self) -> Result<(), Self::Error> {
        let (keys, buttons) = self.held.release_all();
        for keycode in keys {
            self.xtest_key(keycode, PressState::Released)?;
        }
        for button in buttons {
            self.xtest_button(button, PressState::Released)?;
        }
        for (keycode, _) in std::mem::take(&mut self.rebound) {
            self.restore_spare(keycode)?;
        }
        self.release_input_left_down(false)?;
        check_gone(self.conn.set_input_focus(
            x::InputFocus::POINTER_ROOT,
            POINTER_ROOT_WINDOW,
            CURRENT_TIME,
        )?)?;
        self.focused = None;
        self.ensured = None;
        self.keyboard = None;
        self.conn.flush()?;
        Ok(())
    }

    /// Applies the host's configure, clamped to the size hints, the root and the wire's
    /// caps. When that leaves the reported size as it is, the X server reports nothing, so
    /// the backend reports `Resized` with the size the window has itself, and the host's
    /// configure is answered all the same (C1).
    fn configure(&mut self, id: SurfaceId, size: Size) -> Result<(), Self::Error> {
        let bound = size_bound(self.root_size);
        let Some(tracked) = self.table.by_surface(id) else {
            return Err(BackendError::UnknownSurface(id));
        };
        let size = tracked.hints.clamp(size, bound);
        let window = tracked.window;
        let origin = tracked.geometry.origin;
        let unchanged = size == bounded_size(tracked.geometry.size, bound);
        check_gone(
            self.conn.configure_window(
                window,
                &x::ConfigureWindowAux::new()
                    .width(size.width)
                    .height(size.height),
            )?,
        )?;
        // The synthetic ICCCM ConfigureNotify: some clients only react to it. The size it
        // names is the one applied; a following server ConfigureNotify reports what the
        // app actually took.
        self.send_synthetic_configure(
            window,
            Rect::new(origin.x, origin.y, size.width, size.height),
        )?;
        if unchanged {
            self.pending.push(SurfaceEvent::Resized { id, size });
        }
        self.conn.flush()?;
        Ok(())
    }

    fn close(&mut self, id: SurfaceId) -> Result<(), Self::Error> {
        let Some(tracked) = self.table.by_surface(id) else {
            return Err(BackendError::UnknownSurface(id));
        };
        // A window that never listed WM_DELETE_WINDOW is never killed: the request is
        // simply not deliverable.
        if tracked.has_delete {
            let window = tracked.window;
            self.send_protocols_message(window, self.atoms.wm_delete_window)?;
            self.conn.flush()?;
        }
        Ok(())
    }

    fn clipboard_set(&mut self, text: &str) -> Result<(), Self::Error> {
        self.clipboard.text = Some(text.to_owned());
        self.conn
            .set_selection_owner(self.owner_window, self.atoms.clipboard, CURRENT_TIME)?
            .check()?;
        self.conn.flush()?;
        Ok(())
    }
}

/// Takes the window-manager rights on the root: `SubstructureRedirect` (map and
/// configure requests come here instead of happening) plus the notifications the backend
/// reads. An `Access` error means another window manager is already there.
fn take_root(conn: &RustConnection, root: x::Window) -> Result<(), BackendError> {
    let mask = x::EventMask::SUBSTRUCTURE_REDIRECT
        | x::EventMask::SUBSTRUCTURE_NOTIFY
        | x::EventMask::STRUCTURE_NOTIFY
        | x::EventMask::PROPERTY_CHANGE;
    if let Err(e) = conn
        .change_window_attributes(root, &x::ChangeWindowAttributesAux::new().event_mask(mask))?
        .check()
    {
        return Err(match e {
            ReplyError::X11Error(ref err) if err.error_kind == ErrorKind::Access => {
                BackendError::NotWindowManager
            }
            other => BackendError::from(other),
        });
    }
    Ok(())
}

/// Creates the invisible window that owns the CLIPBOARD selection, and takes ownership.
fn create_selection_owner(
    conn: &RustConnection,
    root: x::Window,
    clipboard: u32,
) -> Result<x::Window, BackendError> {
    let owner = conn.generate_id()?;
    conn.create_window(
        COPY_DEPTH_FROM_PARENT,
        owner,
        root,
        0,
        0,
        1,
        1,
        0,
        x::WindowClass::INPUT_ONLY,
        COPY_FROM_PARENT,
        &x::CreateWindowAux::new(),
    )?
    .check()?;
    conn.set_selection_owner(owner, clipboard, CURRENT_TIME)?
        .check()?;
    Ok(owner)
}

/// Selects the XFixes notifications: cursor changes and CLIPBOARD ownership changes.
fn select_xfixes(
    conn: &RustConnection,
    root: x::Window,
    clipboard: u32,
) -> Result<(), BackendError> {
    conn.xfixes_select_cursor_input(root, xfixes::CursorNotifyMask::DISPLAY_CURSOR)?
        .check()?;
    conn.xfixes_select_selection_input(
        root,
        clipboard,
        xfixes::SelectionEventMask::SET_SELECTION_OWNER,
    )?
    .check()?;
    Ok(())
}

/// Writes the minimum EWMH presence, so apps can tell a window manager exists.
///
/// The check window names itself in `_NET_SUPPORTING_WM_CHECK` too, as EWMH requires:
/// toolkits compare the two before they believe the root's. `_NET_SUPPORTED` lists what the
/// backend acts on: the check, `_NET_WM_NAME` titles, and `_NET_ACTIVE_WINDOW` requests,
/// which it reports to the host rather than obeys.
fn announce_ewmh(
    conn: &RustConnection,
    root: x::Window,
    owner: x::Window,
    atoms: &Atoms,
) -> Result<(), BackendError> {
    for window in [root, owner] {
        conn.change_property(
            x::PropMode::REPLACE,
            window,
            atoms.net_supporting_wm_check,
            XA_WINDOW,
            32,
            1,
            &owner.to_ne_bytes(),
        )?
        .check()?;
    }
    conn.change_property(
        x::PropMode::REPLACE,
        owner,
        atoms.net_wm_name,
        atoms.utf8_string,
        8,
        u32::try_from(b"appricot".len()).unwrap_or(u32::MAX),
        b"appricot",
    )?
    .check()?;
    let supported = [
        atoms.net_supported,
        atoms.net_supporting_wm_check,
        atoms.net_wm_name,
        atoms.net_active_window,
    ];
    let data: Vec<u8> = supported.iter().flat_map(|a| a.to_ne_bytes()).collect();
    conn.change_property(
        x::PropMode::REPLACE,
        root,
        atoms.net_supported,
        XA_ATOM,
        32,
        // Format 32 counts elements, not bytes.
        u32::try_from(supported.len()).unwrap_or(0),
        &data,
    )?
    .check()?;
    Ok(())
}

/// Runs `cookie` and treats a window-gone reply as success: the client died mid-handling
/// and there is nothing left to do about it. Anything else fails the call.
fn check_gone(cookie: VoidCookie<'_, RustConnection>) -> Result<(), BackendError> {
    match cookie.check() {
        Ok(()) => Ok(()),
        Err(e) => {
            let error = BackendError::from(e);
            if error.is_window_gone() {
                Ok(())
            } else {
                Err(error)
            }
        }
    }
}

/// Clamps to the protocol's 16-bit coordinates.
fn clamp_i16(v: i32) -> i16 {
    i16::try_from(v.clamp(i32::from(i16::MIN), i32::from(i16::MAX))).unwrap_or(0)
}

/// Clamps to the protocol's 16-bit sizes.
fn clamp_u16(v: u32) -> u16 {
    u16::try_from(v.min(u32::from(u16::MAX))).unwrap_or(0)
}

/// Reads the whole core keyboard mapping.
fn fetch_keymap(conn: &RustConnection) -> Result<Keymap, BackendError> {
    let setup = conn.setup();
    let min = setup.min_keycode;
    let count = setup.max_keycode.saturating_sub(min).saturating_add(1);
    let reply = conn.get_keyboard_mapping(min, count)?.reply()?;
    Ok(Keymap::from_flat(
        min,
        reply.keysyms_per_keycode,
        &reply.keysyms,
    ))
}

/// The app id `WM_CLASS` carries: `res_class`, the second of its two strings.
fn wm_class_res_class(raw: &[u8]) -> String {
    let mut parts = raw.split(|&b| b == 0);
    let _res_name = parts.next();
    parts.next().map(latin1_decode).unwrap_or_default()
}

/// A window's inside geometry from its outer one: the origin moves past the border; X
/// already reports the size without it.
fn inner_geometry(outer: Rect, border: u16) -> Rect {
    let border = i32::from(border);
    Rect::new(
        outer.origin.x.saturating_add(border),
        outer.origin.y.saturating_add(border),
        outer.size.width,
        outer.size.height,
    )
}

/// Builds one of the bounded strings a `SurfaceEvent` field carries.
///
/// The backend cannot name the field's type: `appricot-proto` is not this crate's
/// dependency (the workspace edges are core -> proto and x11 -> core, and the lockfile
/// pins them), so the type is reached through the field itself — `T` is inferred from
/// where the call sits. Text longer than `cap` bytes is cut at a character boundary; a
/// string the cap still cannot fit (the caps are provisional until the wire spec task
/// fixes them) becomes the empty string rather than a panic.
fn bounded_field<T>(text: &str, cap: usize) -> T
where
    T: for<'a> TryFrom<&'a str>,
{
    let mut end = text.len().min(cap);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    match T::try_from(&text[..end]) {
        Ok(value) => value,
        Err(_) => T::try_from("").unwrap_or_else(|_| panic!("the empty string must fit every cap")),
    }
}

/// Keeps a surface-local point inside the surface.
fn clamp_inside(at: Point, size: Size) -> Point {
    Point::new(
        at.x.clamp(0, i32::try_from(size.width.saturating_sub(1)).unwrap_or(0)),
        at.y.clamp(0, i32::try_from(size.height.saturating_sub(1)).unwrap_or(0)),
    )
}
