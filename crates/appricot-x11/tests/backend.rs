//! Integration tests: [`X11Backend`] against the container's X server, driven from the
//! outside as an ordinary X client.
//!
//! Every test needs a running X server with the extensions `appricot-x11` requires, and
//! never skips without one. The backend holds `SubstructureRedirect` on the root, which
//! the X server allows only one client at a time — so the tests serialize on a mutex.
//! Each test destroys the windows it made, and the test client destroys any survivors in
//! its `Drop`, so one test's leftovers cannot shift the next test's placement.

use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use appricot_core::{
    CaptureBackend, InputSink, KeyCode, KeyEvent, Keysym, PressState, Rect, Role, Size,
    SurfaceEvent, SurfaceId,
};
use appricot_x11::X11Backend;
use x11rb::connection::Connection as _;
use x11rb::protocol::Event;
use x11rb::protocol::xproto as x;
use x11rb::protocol::xproto::ConnectionExt as _;
use x11rb::rust_connection::RustConnection;
use x11rb::{COPY_DEPTH_FROM_PARENT, COPY_FROM_PARENT, CURRENT_TIME, NONE};

const RUN_HINT: &str = "run inside the dev container: docker compose run --rm dev just test";
const TIMEOUT: Duration = Duration::from_secs(5);

/// The X keysym of `a`: Latin-1 codepoint 0x61, which is also its X11 keysym.
const XK_LOWER_A: u32 = 0x61;

fn display() -> String {
    let display = std::env::var("DISPLAY").unwrap_or_default();
    assert!(!display.is_empty(), "DISPLAY is not set; {RUN_HINT}");
    display
}

/// Xvfb without `-noreset` restarts itself when its last client disconnects, and a
/// restart resets whatever connects next. The tests open and close connections around
/// each other, so one connection lives for the whole binary to keep the server settled.
static KEEPALIVE: OnceLock<RustConnection> = OnceLock::new();

/// Takes the one slot: only one backend at a time may manage the root.
fn the_root() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| {
        let _ = KEEPALIVE.get_or_init(|| {
            x11rb::connect(Some(&display()))
                .expect("a keepalive connection")
                .0
        });
        Mutex::new(())
    })
    .lock()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Drains the backend until one event matches, and returns everything seen meanwhile.
///
/// `drain_events` never blocks, so this polls on a short sleep against a deadline; a
/// hang-free failure is a panic that names the missing event.
fn drain_until<F>(backend: &mut X11Backend, matches: F) -> Vec<SurfaceEvent>
where
    F: Fn(&SurfaceEvent) -> bool,
{
    let deadline = Instant::now() + TIMEOUT;
    let mut seen = Vec::new();
    while Instant::now() < deadline {
        let mut batch = Vec::new();
        backend.drain_events(&mut batch).expect("drain_events");
        if seen.iter().any(&matches) {
            return seen;
        }
        seen.extend(batch);
        if seen.iter().any(&matches) {
            return seen;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("no matching surface event within {TIMEOUT:?}; saw: {seen:?}");
}

/// Pumps the backend and the test client together until the client's event arrives: the
/// backend only answers a selection request while `drain_events` runs, and nothing else
/// would wake it.
fn pump_until_client_event<F>(backend: &mut X11Backend, client: &TestClient, matches: F) -> Event
where
    F: Fn(&Event) -> bool,
{
    let deadline = Instant::now() + TIMEOUT;
    while Instant::now() < deadline {
        let mut batch = Vec::new();
        backend.drain_events(&mut batch).expect("drain_events");
        if let Some(event) = client.conn.poll_for_event().expect("poll_for_event")
            && matches(&event)
        {
            return event;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("no matching X event within {TIMEOUT:?}");
}

/// One plain X connection that cleans up after itself.
struct TestClient {
    conn: RustConnection,
    root: x::Window,
    wm_class: u32,
    wm_protocols: u32,
    wm_transient_for: u32,
    wm_delete_window: u32,
    net_wm_name: u32,
    utf8_string: u32,
    clipboard: u32,
    /// A private atom the selection tests answer into.
    selection_property: u32,
    /// Every window this test made, destroyed on drop.
    windows: Vec<x::Window>,
}

impl TestClient {
    fn new() -> Self {
        let (conn, screen) =
            x11rb::connect(Some(&display())).unwrap_or_else(|e| panic!("test client: {e}"));
        let root = conn.setup().roots[screen].root;
        let atom = |name: &str| -> u32 {
            conn.intern_atom(false, name.as_bytes())
                .expect("intern_atom")
                .reply()
                .expect("intern reply")
                .atom
        };
        Self {
            wm_class: atom("WM_CLASS"),
            wm_protocols: atom("WM_PROTOCOLS"),
            wm_transient_for: atom("WM_TRANSIENT_FOR"),
            wm_delete_window: atom("WM_DELETE_WINDOW"),
            net_wm_name: atom("_NET_WM_NAME"),
            utf8_string: atom("UTF8_STRING"),
            clipboard: atom("CLIPBOARD"),
            selection_property: atom("APPRICOT_TEST_SELECTION_REPLY"),
            root,
            conn,
            windows: Vec::new(),
        }
    }

    /// A managed toplevel asking for `width` x `height`, created but not mapped.
    fn toplevel(&mut self, width: u16, height: u16, key_events: bool) -> x::Window {
        let mask = if key_events {
            x::EventMask::KEY_PRESS | x::EventMask::KEY_RELEASE
        } else {
            x::EventMask::NO_EVENT
        };
        self.window(width, height, 0, 0, false, mask)
    }

    /// An override-redirect popup at root position `(x, y)`.
    fn popup(&mut self, width: u16, height: u16, x: i16, y: i16) -> x::Window {
        self.window(width, height, x, y, true, x::EventMask::NO_EVENT)
    }

    fn window(
        &mut self,
        width: u16,
        height: u16,
        x: i16,
        y: i16,
        override_redirect: bool,
        mask: x::EventMask,
    ) -> x::Window {
        let window = self.conn.generate_id().expect("xid");
        let aux = x::CreateWindowAux::new()
            .event_mask(mask)
            .override_redirect(u32::from(override_redirect))
            .background_pixel(0x00_10_20_30);
        self.conn
            .create_window(
                COPY_DEPTH_FROM_PARENT,
                window,
                self.root,
                x,
                y,
                width,
                height,
                0,
                x::WindowClass::INPUT_OUTPUT,
                COPY_FROM_PARENT,
                &aux,
            )
            .expect("create_window")
            .check()
            .expect("create_window reply");
        self.windows.push(window);
        window
    }

    fn set_text_property(&self, window: x::Window, property: u32, type_: u32, value: &str) {
        self.conn
            .change_property(
                x::PropMode::REPLACE,
                window,
                property,
                type_,
                8,
                u32::try_from(value.len()).unwrap_or(u32::MAX),
                value.as_bytes(),
            )
            .expect("change_property")
            .check()
            .expect("change_property reply");
    }

    fn set_wm_class(&self, window: x::Window, res_name: &str, res_class: &str) {
        let value = format!("{res_name}\0{res_class}\0");
        self.set_text_property(window, self.wm_class, 31u32 /* STRING */, &value);
    }

    fn set_transient_for(&self, window: x::Window, owner: x::Window) {
        self.conn
            .change_property(
                x::PropMode::REPLACE,
                window,
                self.wm_transient_for,
                33u32, /* WINDOW */
                32,
                1,
                &owner.to_ne_bytes(),
            )
            .expect("change_property")
            .check()
            .expect("change_property reply");
    }

    fn set_wm_delete_window(&self, window: x::Window) {
        self.conn
            .change_property(
                x::PropMode::REPLACE,
                window,
                self.wm_protocols,
                4u32, /* ATOM */
                32,
                1,
                &self.wm_delete_window.to_ne_bytes(),
            )
            .expect("change_property")
            .check()
            .expect("change_property reply");
    }

    fn map(&self, window: x::Window) {
        self.conn
            .map_window(window)
            .expect("map")
            .check()
            .expect("map reply");
        self.conn.flush().expect("flush");
    }

    fn unmap(&self, window: x::Window) {
        self.conn
            .unmap_window(window)
            .expect("unmap")
            .check()
            .expect("unmap reply");
        self.conn.flush().expect("flush");
    }

    /// Fills a rectangle of the window with a depth-24 ZPixmap image of colour
    /// `(blue, green, red)`.
    fn fill(&self, window: x::Window, rect: Rect, blue: u8, green: u8, red: u8) {
        let gc = self.conn.generate_id().expect("xid");
        self.conn
            .create_gc(gc, window, &x::CreateGCAux::new())
            .expect("create_gc")
            .check()
            .expect("create_gc reply");
        let width = rect.size.width as usize;
        let height = rect.size.height as usize;
        // This X server stores depth 24 as 32bpp, so the image is 4 bytes per pixel.
        let mut data = Vec::with_capacity(width * height * 4);
        for _ in 0..width * height {
            data.extend_from_slice(&[blue, green, red, 0xff]);
        }
        self.conn
            .put_image(
                x::ImageFormat::Z_PIXMAP,
                window,
                gc,
                u16::try_from(width).unwrap_or(0),
                u16::try_from(height).unwrap_or(0),
                i16::try_from(rect.origin.x).unwrap_or(0),
                i16::try_from(rect.origin.y).unwrap_or(0),
                0,
                24,
                &data,
            )
            .expect("put_image")
            .check()
            .expect("put_image reply");
        self.conn
            .free_gc(gc)
            .expect("free_gc")
            .check()
            .expect("free_gc reply");
        self.conn.flush().expect("flush");
    }

    fn geometry(&self, window: x::Window) -> (i16, i16, u16, u16) {
        let geo = self
            .conn
            .get_geometry(window)
            .expect("get_geometry")
            .reply()
            .expect("geometry reply");
        (geo.x, geo.y, geo.width, geo.height)
    }

    /// The first server event matching `matches`, within the deadline.
    fn wait_for_event_matching<F>(&self, matches: F) -> Event
    where
        F: Fn(&Event) -> bool,
    {
        let deadline = Instant::now() + TIMEOUT;
        while Instant::now() < deadline {
            if let Some(event) = self.conn.poll_for_event().expect("poll_for_event") {
                if matches(&event) {
                    return event;
                }
            } else {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        panic!("no matching X event within {TIMEOUT:?}");
    }

    /// Maps a managed toplevel and waits for the backend to report it. Everything drained
    /// on the way is handed back: a client that set its title before mapping gets its
    /// Metadata in the same batch, and the caller should not have to wait for it again.
    #[allow(clippy::type_complexity)]
    fn map_and_find_created(
        &self,
        backend: &mut X11Backend,
        window: x::Window,
        width: u32,
        height: u32,
    ) -> (SurfaceId, Vec<SurfaceEvent>) {
        self.map(window);
        let events = drain_until(backend, |e| {
            matches!(
                e,
                SurfaceEvent::Created { role: Role::Toplevel, size, .. }
                    if *size == Size::new(width, height)
            )
        });
        let id = events
            .iter()
            .find_map(|e| match e {
                SurfaceEvent::Created { id, .. } => Some(*id),
                _ => None,
            })
            .expect("a Created event");
        (id, events)
    }
}

impl Drop for TestClient {
    fn drop(&mut self) {
        for window in std::mem::take(&mut self.windows) {
            let _ = self.conn.destroy_window(window);
        }
        let _ = self.conn.flush();
    }
}

/// A fresh backend and client pair, with the root slot held for the caller's scope.
///
/// The guard must outlive the test: only one backend at a time may manage the root, and
/// two backends on one display corrupt each other's bookkeeping, not just their results.
/// The previous test's backend was dropped a moment ago; the server releases its grip on
/// the root asynchronously, so a `NotWindowManager` right after is a race, not a verdict.
fn backend_and_client() -> (MutexGuard<'static, ()>, X11Backend, TestClient) {
    let guard = the_root();
    let deadline = Instant::now() + TIMEOUT;
    let mut backend = None;
    while backend.is_none() {
        match X11Backend::connect(Some(&display())) {
            Ok(connected) => backend = Some(connected),
            Err(e) if e.is_takeover_conflict() && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(other) => panic!("the backend connects: {other}"),
        }
    }
    let client = TestClient::new();
    (guard, backend.expect("the loop only leaves Some"), client)
}

/// Maps and waits for the `Created` of the toplevel of exactly `width` x `height` — sizes
/// are a test's window labels — and returns its id and dialog parent.
fn map_and_find_created_parent(
    client: &TestClient,
    backend: &mut X11Backend,
    window: x::Window,
    width: u32,
    height: u32,
) -> (SurfaceId, Option<SurfaceId>) {
    client.map(window);
    let events = drain_until(backend, |e| {
        matches!(
            e,
            SurfaceEvent::Created { role: Role::Toplevel, size, .. }
                if *size == Size::new(width, height)
        )
    });
    events
        .into_iter()
        .find_map(|e| match e {
            SurfaceEvent::Created { id, parent, .. } => Some((id, parent)),
            _ => None,
        })
        .expect("a Created event")
}

#[test]
fn a_transient_toplevel_maps_as_a_dialog_of_its_parent() {
    let (_guard, mut backend, mut client) = backend_and_client();
    let parent = client.toplevel(200, 150, false);
    let (parent_id, _history) = client.map_and_find_created(&mut backend, parent, 200, 150);

    // The property is set before the map, the way a toolkit does it.
    let dialog = client.toplevel(150, 100, false);
    client.set_transient_for(dialog, parent);
    let (dialog_id, parent_of_dialog) =
        map_and_find_created_parent(&client, &mut backend, dialog, 150, 100);
    assert_eq!(parent_of_dialog, Some(parent_id));
    assert_ne!(dialog_id, parent_id);

    // A dialog of the dialog: still a toplevel, still parented.
    let chained = client.toplevel(120, 80, false);
    client.set_transient_for(chained, dialog);
    let (_, parent_of_chained) =
        map_and_find_created_parent(&client, &mut backend, chained, 120, 80);
    assert_eq!(parent_of_chained, Some(dialog_id));
}

#[test]
fn a_transient_target_that_is_no_managed_toplevel_maps_with_no_parent() {
    let (_guard, mut backend, mut client) = backend_and_client();
    let main = client.toplevel(200, 150, false);
    let (_main_id, _history) = client.map_and_find_created(&mut backend, main, 200, 150);

    // A tracked popup: tracked, but no toplevel.
    let popup = client.popup(60, 20, 5, 5);
    client.set_transient_for(popup, main);
    client.map(popup);
    drain_until(&mut backend, |e| {
        matches!(
            e,
            SurfaceEvent::Created {
                role: Role::Popup { .. },
                ..
            }
        )
    });

    // An XID nobody holds.
    let unknown = client.toplevel(110, 60, false);
    client.set_transient_for(unknown, 0x0bad_c0de);
    let (_, parent) = map_and_find_created_parent(&client, &mut backend, unknown, 110, 60);
    assert_eq!(parent, None, "an unknown transient target is no parent");

    // The window itself: never tracked at its own map time.
    let itself = client.toplevel(120, 70, false);
    client.set_transient_for(itself, itself);
    let (_, parent) = map_and_find_created_parent(&client, &mut backend, itself, 120, 70);
    assert_eq!(parent, None, "a window is no parent of itself");

    // The root: ICCCM's "transient for the whole group".
    let of_root = client.toplevel(130, 80, false);
    client.set_transient_for(of_root, client.root);
    let (_, parent) = map_and_find_created_parent(&client, &mut backend, of_root, 130, 80);
    assert_eq!(parent, None, "the root is no parent");

    // The popup above: tracked, but override-redirect.
    let of_popup = client.toplevel(140, 90, false);
    client.set_transient_for(of_popup, popup);
    let (_, parent) = map_and_find_created_parent(&client, &mut backend, of_popup, 140, 90);
    assert_eq!(parent, None, "a popup is no dialog parent");
}

#[test]
fn a_managed_window_maps_as_a_toplevel_with_metadata() {
    let (_guard, mut backend, mut client) = backend_and_client();
    let window = client.toplevel(200, 150, false);
    client.set_text_property(window, client.net_wm_name, client.utf8_string, "Test title");
    client.set_wm_class(window, "resname", "testapp");
    let (id, history) = client.map_and_find_created(&mut backend, window, 200, 150);

    // The title was set before the map, so the Metadata arrives with the Created.
    let in_history: Option<(String, String)> = history.iter().find_map(|e| match e {
        SurfaceEvent::Metadata { title, app_id, .. } => {
            Some((title.as_str().to_owned(), app_id.as_str().to_owned()))
        }
        _ => None,
    });
    let (title, app_id) = if let Some(found) = in_history {
        found
    } else {
        let events = drain_until(
            &mut backend,
            |e| matches!(e, SurfaceEvent::Metadata { id: meta, .. } if *meta == id),
        );
        events
            .into_iter()
            .find_map(|e| match e {
                SurfaceEvent::Metadata { title, app_id, .. } => {
                    Some((title.as_str().to_owned(), app_id.as_str().to_owned()))
                }
                _ => None,
            })
            .expect("a Metadata event")
    };
    assert_eq!(title.as_str(), "Test title");
    assert_eq!(app_id.as_str(), "testapp");

    // The first toplevel sits at the root origin, per the kept-apart layout.
    assert_eq!(client.geometry(window), (0, 0, 200, 150));

    client
        .conn
        .destroy_window(window)
        .expect("destroy")
        .check()
        .expect("destroy reply");
    let events = drain_until(
        &mut backend,
        |e| matches!(e, SurfaceEvent::Destroyed { id: gone } if *gone == id),
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, SurfaceEvent::Destroyed { id: gone } if *gone == id))
    );
}

#[test]
fn an_override_redirect_window_maps_as_a_popup_of_its_transient_parent() {
    let (_guard, mut backend, mut client) = backend_and_client();
    let parent = client.toplevel(200, 150, false);
    let (parent_id, _history) = client.map_and_find_created(&mut backend, parent, 200, 150);

    // 30,40 over the parent's origin: the positioner offset the host sees.
    let popup = client.popup(100, 20, 30, 40);
    client.set_transient_for(popup, parent);
    client.map(popup);

    let events = drain_until(&mut backend, |e| {
        matches!(
            e,
            SurfaceEvent::Created {
                role: Role::Popup { .. },
                ..
            }
        )
    });
    let (popup_id, role) = events
        .into_iter()
        .find_map(|e| match e {
            SurfaceEvent::Created { id, role, .. } => Some((id, role)),
            _ => None,
        })
        .expect("the popup's Created");
    let Role::Popup {
        parent: popup_parent,
        positioner,
    } = role
    else {
        panic!("expected a popup role, got {role:?}");
    };
    assert_eq!(popup_parent, parent_id);
    assert_eq!(positioner.place(), Rect::new(30, 40, 100, 20));

    // A popup unmapping is the popup going away.
    client.unmap(popup);
    let events = drain_until(&mut backend, |e| {
        matches!(e, SurfaceEvent::Destroyed { .. })
    });
    assert!(
        events
            .iter()
            .any(|e| matches!(e, SurfaceEvent::Destroyed { id: gone } if *gone == popup_id))
    );
}

#[test]
fn damage_and_capture_return_the_drawn_pattern() {
    let (_guard, mut backend, mut client) = backend_and_client();
    let window = client.toplevel(200, 150, false);
    let (id, _history) = client.map_and_find_created(&mut backend, window, 200, 150);

    client.fill(window, Rect::new(0, 0, 200, 150), 0x10, 0x20, 0x30);
    client.fill(window, Rect::new(8, 8, 16, 16), 0x33, 0x22, 0x11);

    let events = drain_until(&mut backend, |e| {
        matches!(e, SurfaceEvent::Damaged { id: damaged, rect }
            if *damaged == id && rect.intersection(Rect::new(8, 8, 16, 16)).is_some())
    });
    assert!(
        events
            .iter()
            .any(|e| matches!(e, SurfaceEvent::Damaged { rect, .. }
            if rect.intersection(Rect::new(8, 8, 16, 16)).is_some()))
    );

    let buffer = backend
        .capture(id, Rect::new(0, 0, 200, 150))
        .expect("capture");
    assert_eq!(buffer.size, Size::new(200, 150));
    assert_eq!(buffer.stride, 800);
    assert_eq!(buffer.format, appricot_core::PixelFormat::Bgrx8888);
    let offset = 8 * buffer.stride + 8 * 4;
    assert_eq!(&buffer.data[offset..offset + 4], &[0x33, 0x22, 0x11, 0xff]);
    assert_eq!(&buffer.data[..4], &[0x10, 0x20, 0x30, 0xff]);
}

#[test]
fn an_xtest_key_reaches_the_focused_window() {
    let (_guard, mut backend, mut client) = backend_and_client();
    let window = client.toplevel(200, 150, true);
    let (id, _history) = client.map_and_find_created(&mut backend, window, 200, 150);

    backend.focus(id).expect("focus");
    let key = KeyEvent {
        keysym: Keysym(XK_LOWER_A),
        code: KeyCode::new("KeyA"),
        state: PressState::Pressed,
    };
    backend.key(key.clone()).expect("key press");
    backend
        .key(KeyEvent {
            state: PressState::Released,
            ..key
        })
        .expect("key release");

    let pressed = client.wait_for_event_matching(|event| {
        matches!(event, Event::KeyPress(_) | Event::KeyRelease(_))
    });
    let Event::KeyPress(pressed) = pressed else {
        panic!("expected a KeyPress first, got {pressed:?}");
    };
    // The keycode's unshifted column produces 'a' on the server's keymap.
    let setup = client.conn.setup();
    let min = setup.min_keycode;
    let count = setup.max_keycode - min + 1;
    let map = client
        .conn
        .get_keyboard_mapping(min, count)
        .expect("get_keyboard_mapping")
        .reply()
        .expect("keyboard mapping");
    let index = usize::from(pressed.detail - min) * usize::from(map.keysyms_per_keycode);
    assert_eq!(
        map.keysyms.get(index).copied().unwrap_or_default(),
        XK_LOWER_A,
        "keycode {} column 0 should produce keysym a",
        pressed.detail
    );

    // And the release follows.
    client.wait_for_event_matching(|event| matches!(event, Event::KeyRelease(_)));
}

#[test]
fn close_sends_wm_delete_window_and_is_a_request() {
    let (_guard, mut backend, mut client) = backend_and_client();
    let window = client.toplevel(200, 150, false);
    client.set_wm_delete_window(window);
    let (id, _history) = client.map_and_find_created(&mut backend, window, 200, 150);

    backend.close(id).expect("close");

    let message = client.wait_for_event_matching(|event| matches!(event, Event::ClientMessage(_)));
    let Event::ClientMessage(message) = message else {
        unreachable!("checked in the wait");
    };
    assert_eq!(message.type_, client.wm_protocols);
    assert_eq!(message.data.as_data32()[0], client.wm_delete_window);

    // The window still exists: closing is a request, never a kill.
    assert_eq!(client.geometry(window).2, 200);
}

#[test]
fn two_toplevels_map_apart() {
    let (_guard, mut backend, mut client) = backend_and_client();
    let first = client.toplevel(200, 150, false);
    client.map(first);
    drain_until(&mut backend, |e| matches!(e, SurfaceEvent::Created { .. }));
    let second = client.toplevel(200, 150, false);
    client.map(second);
    drain_until(
        &mut backend,
        |e| matches!(e, SurfaceEvent::Created { size, .. } if *size == Size::new(200, 150)),
    );

    let (x1, y1, w1, h1) = client.geometry(first);
    let (x2, y2, w2, h2) = client.geometry(second);
    assert_eq!((w1, h1), (200, 150));
    assert_eq!((w2, h2), (200, 150));
    assert_ne!(
        (x1, y1),
        (x2, y2),
        "the kept-apart layout places the second toplevel elsewhere"
    );
}

#[test]
fn a_host_configure_resizes_the_window_and_reports_it() {
    let (_guard, mut backend, mut client) = backend_and_client();
    let window = client.toplevel(200, 150, false);
    let (id, _history) = client.map_and_find_created(&mut backend, window, 200, 150);

    backend
        .configure(id, Size::new(300, 200))
        .expect("configure");

    let events = drain_until(
        &mut backend,
        |e| matches!(e, SurfaceEvent::Resized { id: resized, size } if *resized == id && *size == Size::new(300, 200)),
    );
    assert!(
        events.iter().any(
            |e| matches!(e, SurfaceEvent::Resized { size, .. } if *size == Size::new(300, 200))
        )
    );
    assert_eq!(client.geometry(window), (0, 0, 300, 200));
}

#[test]
fn the_backend_serves_the_clipboard_it_was_given() {
    let (_guard, mut backend, mut client) = backend_and_client();
    let window = client.toplevel(200, 150, false);
    let (_id, _history) = client.map_and_find_created(&mut backend, window, 200, 150);

    backend
        .clipboard_set("hello apricot")
        .expect("clipboard_set");

    let property = client.selection_property;
    client
        .conn
        .convert_selection(
            window,
            client.clipboard,
            client.utf8_string,
            property,
            CURRENT_TIME,
        )
        .expect("convert_selection")
        .check()
        .expect("convert_selection reply");
    client.conn.flush().expect("flush");

    let Event::SelectionNotify(notified) =
        pump_until_client_event(&mut backend, &client, |event| {
            matches!(event, Event::SelectionNotify(_))
        })
    else {
        unreachable!("checked in the wait");
    };
    assert_eq!(notified.property, property);
    let reply = client
        .conn
        .get_property(false, window, property, NONE, 0, 64)
        .expect("get_property")
        .reply()
        .expect("property reply");
    assert_eq!(String::from_utf8_lossy(&reply.value), "hello apricot");
}

#[test]
fn a_paste_with_no_text_is_reported_and_refused() {
    let (_guard, mut backend, mut client) = backend_and_client();
    let window = client.toplevel(200, 150, false);
    let (_id, _history) = client.map_and_find_created(&mut backend, window, 200, 150);

    let property = client.selection_property;
    client
        .conn
        .convert_selection(
            window,
            client.clipboard,
            client.utf8_string,
            property,
            CURRENT_TIME,
        )
        .expect("convert_selection")
        .check()
        .expect("convert_selection reply");
    client.conn.flush().expect("flush");

    let events = drain_until(&mut backend, |e| {
        matches!(e, SurfaceEvent::ClipboardRequested)
    });
    assert!(
        events
            .iter()
            .any(|e| matches!(e, SurfaceEvent::ClipboardRequested))
    );

    // The paste is refused: the notify names no property, and none was written.
    let Event::SelectionNotify(notified) =
        client.wait_for_event_matching(|event| matches!(event, Event::SelectionNotify(_)))
    else {
        unreachable!("checked in the wait");
    };
    assert_eq!(notified.property, NONE);
    let reply = client
        .conn
        .get_property(false, window, property, NONE, 0, 64)
        .expect("get_property")
        .reply()
        .expect("property reply");
    assert!(reply.value.is_empty(), "a refused paste writes nothing");
}
