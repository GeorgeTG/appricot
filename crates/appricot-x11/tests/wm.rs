//! Integration tests for the window-manager side of [`X11Backend`]: popups that map again,
//! configure requests before and after a window is announced, host configures that change
//! nothing, the capture contract, the named pixmap across resizes, size caps, titles,
//! borders, synthetic events, EWMH/ICCCM state and the layout.
//!
//! Like `tests/backend.rs`, every test needs a running X server with the extensions the
//! backend requires and never skips without one. Only one backend may manage the root, so
//! the tests serialize on a mutex, and each test's client destroys its windows on drop.

use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use appricot_core::{
    CaptureBackend, InputSink, MAX_SURFACE_HEIGHT, MAX_SURFACE_WIDTH, Rect, Role, Size,
    SurfaceEvent, SurfaceId,
};
use appricot_x11::X11Backend;
use x11rb::connection::Connection as _;
use x11rb::protocol::Event;
use x11rb::protocol::xproto as x;
use x11rb::protocol::xproto::ConnectionExt as _;
use x11rb::rust_connection::RustConnection;
use x11rb::{COPY_DEPTH_FROM_PARENT, COPY_FROM_PARENT, NONE};

const RUN_HINT: &str = "run inside the dev container: docker compose run --rm dev just test";
const TIMEOUT: Duration = Duration::from_secs(5);

/// Predefined atoms: `ATOM`, `WINDOW`, `WM_NORMAL_HINTS` and its type `WM_SIZE_HINTS`.
const XA_ATOM: u32 = 4;
const XA_WINDOW: u32 = 33;
const XA_WM_NORMAL_HINTS: u32 = 40;
const XA_WM_SIZE_HINTS: u32 = 41;

/// A window's background, as a depth-24 pixel: B=0x30 G=0x20 R=0x10 in memory.
const BACKGROUND: u32 = 0x00_10_20_30;

fn display() -> String {
    let display = std::env::var("DISPLAY").unwrap_or_default();
    assert!(!display.is_empty(), "DISPLAY is not set; {RUN_HINT}");
    display
}

/// Keeps the X server from resetting between the tests' connections (see
/// `tests/backend.rs`).
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

/// Connects a backend, retrying while the previous test's backend still holds the root.
fn connect_backend() -> X11Backend {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        match X11Backend::connect(Some(&display())) {
            Ok(backend) => return backend,
            Err(e) if e.is_takeover_conflict() && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(other) => panic!("the backend connects: {other}"),
        }
    }
}

/// A fresh backend and client pair, with the root slot held for the caller's scope.
fn backend_and_client() -> (MutexGuard<'static, ()>, X11Backend, TestClient) {
    let guard = the_root();
    let backend = connect_backend();
    (guard, backend, TestClient::new())
}

/// Drains the backend until one event matches, and returns everything seen meanwhile.
fn drain_until<F>(backend: &mut X11Backend, matches: F) -> Vec<SurfaceEvent>
where
    F: Fn(&SurfaceEvent) -> bool,
{
    let deadline = Instant::now() + TIMEOUT;
    let mut seen = Vec::new();
    while Instant::now() < deadline {
        let mut batch = Vec::new();
        backend.drain_events(&mut batch).expect("drain_events");
        seen.extend(batch);
        if seen.iter().any(&matches) {
            return seen;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("no matching surface event within {TIMEOUT:?}; saw: {seen:?}");
}

/// Pumps the backend and the client together until the client receives a matching X
/// event. Returns the event and every surface event the backend produced meanwhile.
fn pump_until_client_event<F>(
    backend: &mut X11Backend,
    client: &TestClient,
    matches: F,
) -> (Event, Vec<SurfaceEvent>)
where
    F: Fn(&Event) -> bool,
{
    let deadline = Instant::now() + TIMEOUT;
    let mut seen = Vec::new();
    while Instant::now() < deadline {
        let mut batch = Vec::new();
        backend.drain_events(&mut batch).expect("drain_events");
        seen.extend(batch);
        while let Some(event) = client.conn.poll_for_event().expect("poll_for_event") {
            if matches(&event) {
                // One more drain: whatever the backend did alongside is reported too.
                let mut batch = Vec::new();
                backend.drain_events(&mut batch).expect("drain_events");
                seen.extend(batch);
                return (event, seen);
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("no matching X event within {TIMEOUT:?}; backend saw: {seen:?}");
}

/// A synthetic `ConfigureNotify`, as the backend sends it for a request it did not apply.
fn is_synthetic_configure(event: &Event, window: x::Window) -> bool {
    matches!(event, Event::ConfigureNotify(e) if e.window == window) && event.sent_event()
}

/// The id of the first `Created` in `events` whose role is a popup when `popup`, and a
/// toplevel otherwise.
fn created_id(events: &[SurfaceEvent], popup: bool) -> SurfaceId {
    events
        .iter()
        .find_map(|e| match e {
            SurfaceEvent::Created { id, role, .. }
                if matches!(role, Role::Popup { .. }) == popup =>
            {
                Some(*id)
            }
            _ => None,
        })
        .expect("a Created event")
}

/// A `Resized` of surface `id` to `size`.
fn is_resized(e: &SurfaceEvent, id: SurfaceId, size: Size) -> bool {
    matches!(e, SurfaceEvent::Resized { id: r, size: s } if *r == id && *s == size)
}

fn is_popup_created(e: &SurfaceEvent) -> bool {
    matches!(
        e,
        SurfaceEvent::Created {
            role: Role::Popup { .. },
            ..
        }
    )
}

/// One plain X connection that cleans up after itself.
struct TestClient {
    conn: RustConnection,
    root: x::Window,
    wm_transient_for: u32,
    wm_state: u32,
    wm_class: u32,
    net_wm_name: u32,
    net_supported: u32,
    net_supporting_wm_check: u32,
    net_active_window: u32,
    utf8_string: u32,
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
            wm_transient_for: atom("WM_TRANSIENT_FOR"),
            wm_state: atom("WM_STATE"),
            wm_class: atom("WM_CLASS"),
            net_wm_name: atom("_NET_WM_NAME"),
            net_supported: atom("_NET_SUPPORTED"),
            net_supporting_wm_check: atom("_NET_SUPPORTING_WM_CHECK"),
            net_active_window: atom("_NET_ACTIVE_WINDOW"),
            utf8_string: atom("UTF8_STRING"),
            root,
            conn,
            windows: Vec::new(),
        }
    }

    /// A managed toplevel asking for `width` x `height`, created but not mapped. It hears
    /// its own configure notifications.
    fn toplevel(&mut self, width: u16, height: u16) -> x::Window {
        self.window(width, height, 0, 0, 0, false)
    }

    /// An override-redirect popup at root position `(x, y)`.
    fn popup(&mut self, width: u16, height: u16, x: i16, y: i16) -> x::Window {
        self.window(width, height, x, y, 0, true)
    }

    fn window(
        &mut self,
        width: u16,
        height: u16,
        x: i16,
        y: i16,
        border: u16,
        override_redirect: bool,
    ) -> x::Window {
        let window = self.conn.generate_id().expect("xid");
        let aux = x::CreateWindowAux::new()
            .event_mask(x::EventMask::STRUCTURE_NOTIFY)
            .override_redirect(u32::from(override_redirect))
            .background_pixel(BACKGROUND)
            .border_pixel(0x00_00_ff_00);
        self.conn
            .create_window(
                COPY_DEPTH_FROM_PARENT,
                window,
                self.root,
                x,
                y,
                width,
                height,
                border,
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

    fn set_property(&self, window: x::Window, property: u32, type_: u32, format: u8, data: &[u8]) {
        let len = data.len() / usize::from(format / 8);
        self.conn
            .change_property(
                x::PropMode::REPLACE,
                window,
                property,
                type_,
                format,
                u32::try_from(len).unwrap_or(u32::MAX),
                data,
            )
            .expect("change_property")
            .check()
            .expect("change_property reply");
    }

    fn set_title(&self, window: x::Window, title: &str) {
        self.set_property(
            window,
            self.net_wm_name,
            self.utf8_string,
            8,
            title.as_bytes(),
        );
    }

    fn set_wm_class(&self, window: x::Window, res_name: &str, res_class: &str) {
        let value = format!("{res_name}\0{res_class}\0");
        self.set_property(
            window,
            self.wm_class,
            31, /* STRING */
            8,
            value.as_bytes(),
        );
    }

    fn set_transient_for(&self, window: x::Window, owner: x::Window) {
        self.set_property(
            window,
            self.wm_transient_for,
            XA_WINDOW,
            32,
            &owner.to_ne_bytes(),
        );
    }

    /// `WM_NORMAL_HINTS` with only a minimum size (`PMinSize`).
    fn set_min_size(&self, window: x::Window, width: u32, height: u32) {
        let mut words = [0u32; 18];
        words[0] = 0x10;
        words[5] = width;
        words[6] = height;
        let data: Vec<u8> = words.iter().flat_map(|w| w.to_ne_bytes()).collect();
        self.set_property(window, XA_WM_NORMAL_HINTS, XA_WM_SIZE_HINTS, 32, &data);
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

    /// Asks for a configure the way an app does; under a window manager it becomes a
    /// `ConfigureRequest`.
    fn request_configure(&self, window: x::Window, aux: &x::ConfigureWindowAux) {
        self.conn
            .configure_window(window, aux)
            .expect("configure_window")
            .check()
            .expect("configure_window reply");
        self.conn.flush().expect("flush");
    }

    /// Fills a rectangle of the window with a depth-24 `ZPixmap` image of colour
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

    /// `(x, y, width, height, border_width)`.
    fn geometry(&self, window: x::Window) -> (i16, i16, u16, u16, u16) {
        let geo = self
            .conn
            .get_geometry(window)
            .expect("get_geometry")
            .reply()
            .expect("geometry reply");
        (geo.x, geo.y, geo.width, geo.height, geo.border_width)
    }

    /// A property's 32-bit words; empty when it is absent.
    fn words(&self, window: x::Window, property: u32) -> Vec<u32> {
        let reply = self
            .conn
            .get_property(false, window, property, 0u32, 0, 64)
            .expect("get_property")
            .reply()
            .expect("property reply");
        reply
            .value
            .chunks_exact(4)
            .map(|w| u32::from_ne_bytes([w[0], w[1], w[2], w[3]]))
            .collect()
    }

    /// Maps a managed toplevel and waits for the backend to report it at `width` x
    /// `height`. Everything drained on the way is handed back.
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
        (created_id(&events, false), events)
    }

    /// The pixmaps the backend's connection holds, found by probing its XID range.
    ///
    /// The backend's EWMH check window is its own, so it names the backend's XID base.
    /// Every id the backend allocated since lies a small number of steps above it. A
    /// pixmap is an id `GetGeometry` accepts and `GetWindowAttributes` refuses.
    fn backend_pixmaps(&self) -> Vec<u32> {
        let check = self
            .words(self.root, self.net_supporting_wm_check)
            .first()
            .copied()
            .expect("the backend names its check window");
        let mask = self.conn.setup().resource_id_mask;
        let base = check & !mask;
        let step = mask & mask.wrapping_neg();
        let ids: Vec<u32> = (0..512u32)
            .map(|k| k * step)
            .take_while(|offset| *offset <= mask)
            .map(|offset| base | offset)
            .collect();
        let geometries: Vec<_> = ids
            .iter()
            .map(|&id| self.conn.get_geometry(id).expect("get_geometry"))
            .collect();
        let attributes: Vec<_> = ids
            .iter()
            .map(|&id| {
                self.conn
                    .get_window_attributes(id)
                    .expect("get_window_attributes")
            })
            .collect();
        ids.iter()
            .zip(geometries)
            .zip(attributes)
            .filter_map(|((id, geometry), attrs)| {
                let drawable = geometry.reply().is_ok();
                let window = attrs.reply().is_ok();
                (drawable && !window).then_some(*id)
            })
            .collect()
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

// --- popups -------------------------------------------------------------------------------

#[test]
fn a_popup_mapped_again_is_shown_again() {
    let (_guard, mut backend, mut client) = backend_and_client();
    let parent = client.toplevel(200, 150);
    let (parent_id, _) = client.map_and_find_created(&mut backend, parent, 200, 150);

    // Toolkits create a menu window once and map and unmap it each time it opens.
    let menu = client.popup(100, 20, 30, 40);
    client.set_transient_for(menu, parent);
    let mut ids = Vec::new();
    for _ in 0..3 {
        client.map(menu);
        let events = drain_until(&mut backend, is_popup_created);
        let id = created_id(&events, true);
        let Some(SurfaceEvent::Created {
            role: Role::Popup { parent, .. },
            ..
        }) = events.iter().find(|e| is_popup_created(e))
        else {
            unreachable!("checked in the drain");
        };
        assert_eq!(*parent, parent_id);
        ids.push(id);

        client.unmap(menu);
        drain_until(
            &mut backend,
            |e| matches!(e, SurfaceEvent::Destroyed { id: gone } if *gone == id),
        );
    }
    ids.dedup();
    assert_eq!(ids.len(), 3, "each map is a new surface: {ids:?}");
}

#[test]
fn windows_mapped_before_the_backend_connected_are_adopted_popups_included() {
    let guard = the_root();
    let mut client = TestClient::new();
    // No window manager runs yet, so both map directly.
    let main = client.toplevel(200, 150);
    client.map(main);
    let menu = client.popup(80, 20, 10, 10);
    client.set_transient_for(menu, main);
    client.map(menu);

    let mut backend = connect_backend();
    let events = drain_until(&mut backend, is_popup_created);
    let main_id = created_id(&events, false);
    let Some(SurfaceEvent::Created {
        role: Role::Popup { parent, .. },
        ..
    }) = events.iter().find(|e| is_popup_created(e))
    else {
        unreachable!("checked in the drain");
    };
    assert_eq!(
        *parent, main_id,
        "the adopted popup keeps its transient parent"
    );
    drop(client);
    drop(backend);
    drop(guard);
}

#[test]
fn an_oversized_popup_is_reported_within_the_root_and_the_wire_caps() {
    let (_guard, mut backend, mut client) = backend_and_client();
    let parent = client.toplevel(200, 150);
    let (_, _) = client.map_and_find_created(&mut backend, parent, 200, 150);
    let root = backend.root_size().expect("root_size");

    let wide = client.popup(4000, 3000, 0, 0);
    client.set_transient_for(wide, parent);
    client.map(wide);
    let events = drain_until(&mut backend, is_popup_created);
    let Some(SurfaceEvent::Created {
        id,
        size,
        role: Role::Popup { positioner, .. },
        ..
    }) = events.iter().find(|e| is_popup_created(e))
    else {
        unreachable!("checked in the drain");
    };
    assert!(size.width <= root.width.min(MAX_SURFACE_WIDTH), "{size:?}");
    assert!(
        size.height <= root.height.min(MAX_SURFACE_HEIGHT),
        "{size:?}"
    );
    assert_eq!(
        positioner.size, *size,
        "the positioner carries the same size"
    );

    // The whole reported surface can be captured.
    let buffer = backend
        .capture(*id, Rect::new(0, 0, 256, 256))
        .expect("capture");
    assert_eq!(buffer.size, Size::new(256, 256));
}

#[test]
fn a_bordered_popup_is_placed_and_captured_inside_its_border() {
    let (_guard, mut backend, mut client) = backend_and_client();
    let parent = client.toplevel(200, 150);
    let (_, _) = client.map_and_find_created(&mut backend, parent, 200, 150);

    // The outer corner is at (30, 40); the border is 3 pixels wide.
    let menu = client.window(60, 20, 30, 40, 3, true);
    client.set_transient_for(menu, parent);
    client.map(menu);
    let events = drain_until(&mut backend, is_popup_created);
    let Some(SurfaceEvent::Created {
        id,
        role: Role::Popup { positioner, .. },
        ..
    }) = events.iter().find(|e| is_popup_created(e))
    else {
        unreachable!("checked in the drain");
    };
    assert_eq!(positioner.place(), Rect::new(33, 43, 60, 20));

    client.fill(menu, Rect::new(0, 0, 60, 20), 0x44, 0x55, 0x66);
    let buffer = backend
        .capture(*id, Rect::new(0, 0, 4, 4))
        .expect("capture");
    assert_eq!(
        &buffer.data[..4],
        &[0x44, 0x55, 0x66, 0xff],
        "the first pixel is the window's, not its border's"
    );
}

// --- configure ----------------------------------------------------------------------------

#[test]
fn an_apps_configure_request_is_granted_only_before_its_window_is_announced() {
    let (_guard, mut backend, mut client) = backend_and_client();
    let window = client.toplevel(200, 150);

    // Before the map: the toolkit sizes its window, and the WM grants it.
    client.request_configure(window, &x::ConfigureWindowAux::new().width(320).height(240));
    let deadline = Instant::now() + TIMEOUT;
    while client.geometry(window).2 != 320 {
        assert!(
            Instant::now() < deadline,
            "the pre-map request was not granted"
        );
        let mut batch = Vec::new();
        backend.drain_events(&mut batch).expect("drain_events");
        std::thread::sleep(Duration::from_millis(5));
    }
    let (id, _) = client.map_and_find_created(&mut backend, window, 320, 240);

    // After the announce: reported, not applied; the app hears its unchanged geometry.
    client.request_configure(window, &x::ConfigureWindowAux::new().width(500).height(400));
    let (notify, events) =
        pump_until_client_event(&mut backend, &client, |e| is_synthetic_configure(e, window));
    let Event::ConfigureNotify(notify) = notify else {
        unreachable!("checked in the pump");
    };
    assert_eq!((notify.width, notify.height), (320, 240));
    let asked: Vec<Size> = events
        .iter()
        .filter_map(|e| match e {
            SurfaceEvent::ResizeRequested { id: asked, size } if *asked == id => Some(*size),
            _ => None,
        })
        .collect();
    assert_eq!(asked, vec![Size::new(500, 400)]);
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, SurfaceEvent::Resized { .. })),
        "nothing was resized: {events:?}"
    );
    let (x_pos, y_pos, width, height, _) = client.geometry(window);
    assert_eq!((width, height), (320, 240), "the size was not applied");

    // A move, and a request for the size it already has, are no resize asks, and the move
    // is not applied either.
    for aux in [
        x::ConfigureWindowAux::new().x(77).y(88),
        x::ConfigureWindowAux::new().width(320).height(240),
    ] {
        client.request_configure(window, &aux);
        let (_, events) =
            pump_until_client_event(&mut backend, &client, |e| is_synthetic_configure(e, window));
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, SurfaceEvent::ResizeRequested { .. })),
            "no resize was asked: {events:?}"
        );
    }
    assert_eq!(client.geometry(window), (x_pos, y_pos, 320, 240, 0));
}

#[test]
fn an_apps_restack_request_is_not_granted() {
    let (_guard, mut backend, mut client) = backend_and_client();
    let below = client.toplevel(200, 150);
    let (_, _) = client.map_and_find_created(&mut backend, below, 200, 150);
    // Created later, so it sits above the first.
    let above = client.toplevel(210, 150);
    let (_, _) = client.map_and_find_created(&mut backend, above, 210, 150);

    client.request_configure(
        below,
        &x::ConfigureWindowAux::new().stack_mode(x::StackMode::ABOVE),
    );
    let _ = pump_until_client_event(&mut backend, &client, |e| is_synthetic_configure(e, below));

    let children = client
        .conn
        .query_tree(client.root)
        .expect("query_tree")
        .reply()
        .expect("tree reply")
        .children;
    let position = |w: x::Window| children.iter().position(|c| *c == w).expect("a child");
    assert!(
        position(below) < position(above),
        "the host decides stacking; the app's raise is not applied"
    );
}

#[test]
fn a_host_configure_that_changes_nothing_is_answered() {
    let (_guard, mut backend, mut client) = backend_and_client();
    let window = client.toplevel(200, 150);
    let (id, _) = client.map_and_find_created(&mut backend, window, 200, 150);

    // The size it already has.
    backend
        .configure(id, Size::new(200, 150))
        .expect("configure");
    drain_until(&mut backend, |e| is_resized(e, id, Size::new(200, 150)));

    // Below its minimum: clamped back to the size it has.
    let bounded = client.toplevel(210, 150);
    client.set_min_size(bounded, 300, 200);
    let (bounded_id, _) = client.map_and_find_created(&mut backend, bounded, 300, 200);
    backend
        .configure(bounded_id, Size::new(100, 50))
        .expect("configure");
    drain_until(&mut backend, |e| {
        is_resized(e, bounded_id, Size::new(300, 200))
    });
    assert_eq!(client.geometry(bounded).2, 300);
}

// --- capture ------------------------------------------------------------------------------

#[test]
fn a_capture_past_a_shrunk_window_keeps_the_size_asked_for() {
    let (_guard, mut backend, mut client) = backend_and_client();
    let window = client.toplevel(200, 150);
    let (id, _) = client.map_and_find_created(&mut backend, window, 200, 150);

    backend
        .configure(id, Size::new(100, 80))
        .expect("configure");
    drain_until(
        &mut backend,
        |e| matches!(e, SurfaceEvent::Resized { size, .. } if *size == Size::new(100, 80)),
    );
    client.fill(window, Rect::new(0, 0, 100, 80), 0x11, 0x22, 0x33);

    // A tile planned from the old size: partly outside the window now.
    let partly = backend
        .capture(id, Rect::new(64, 64, 64, 64))
        .expect("capture");
    assert_eq!(partly.size, Size::new(64, 64));
    assert_eq!(partly.stride, 64 * 4);
    assert_eq!(partly.data.len(), 64 * 64 * 4);
    let at = |x: usize, y: usize| &partly.data[y * partly.stride + x * 4..][..4];
    assert_eq!(at(0, 0), &[0x11, 0x22, 0x33, 0xff], "(64, 64) is inside");
    assert_eq!(at(35, 15), &[0x11, 0x22, 0x33, 0xff], "(99, 79) is inside");
    assert_eq!(at(36, 0), &[0, 0, 0, 0], "(100, 64) is past the width");
    assert_eq!(at(0, 16), &[0, 0, 0, 0], "(64, 80) is past the height");

    // A tile wholly outside is all zeros, still of the size asked for.
    let outside = backend
        .capture(id, Rect::new(128, 0, 64, 64))
        .expect("capture");
    assert_eq!(outside.size, Size::new(64, 64));
    assert_eq!(outside.stride, 64 * 4);
    assert_eq!(outside.data, vec![0; 64 * 64 * 4]);
}

#[test]
fn the_named_pixmap_is_freed_when_the_window_resizes() {
    let (_guard, mut backend, mut client) = backend_and_client();
    let window = client.toplevel(200, 150);
    let (id, _) = client.map_and_find_created(&mut backend, window, 200, 150);

    backend
        .capture(id, Rect::new(0, 0, 64, 64))
        .expect("capture");
    let first = client.backend_pixmaps();
    assert_eq!(first.len(), 1, "one named pixmap per captured window");

    for (width, height) in [(220, 160), (240, 170), (260, 180)] {
        backend
            .configure(id, Size::new(width, height))
            .expect("configure");
        drain_until(&mut backend, |e| {
            is_resized(e, id, Size::new(width, height))
        });
        backend
            .capture(id, Rect::new(0, 0, 64, 64))
            .expect("capture");
    }
    let after = client.backend_pixmaps();
    assert_eq!(after.len(), 1, "the old pixmaps were freed: {after:?}");
    assert_ne!(after, first, "the pixmap was named again at the new size");
}

// --- titles -------------------------------------------------------------------------------

#[test]
fn a_title_change_after_map_is_reported_and_so_is_clearing_it() {
    let (_guard, mut backend, mut client) = backend_and_client();
    let window = client.toplevel(200, 150);
    client.set_title(window, "Before");
    let (id, history) = client.map_and_find_created(&mut backend, window, 200, 150);
    let title_of = |e: &SurfaceEvent| match e {
        SurfaceEvent::Metadata { id: m, title, .. } if *m == id => Some(title.as_str().to_owned()),
        _ => None,
    };
    if !history
        .iter()
        .any(|e| title_of(e).as_deref() == Some("Before"))
    {
        drain_until(&mut backend, |e| title_of(e).as_deref() == Some("Before"));
    }

    client.set_title(window, "After");
    drain_until(&mut backend, |e| title_of(e).as_deref() == Some("After"));

    client
        .conn
        .delete_property(window, client.net_wm_name)
        .expect("delete_property")
        .check()
        .expect("delete_property reply");
    drain_until(&mut backend, |e| title_of(e).as_deref() == Some(""));
}

#[test]
fn a_long_utf8_title_cut_inside_a_character_keeps_its_start() {
    let (_guard, mut backend, mut client) = backend_and_client();
    let window = client.toplevel(200, 150);
    // 1 + 2 * 300 = 601 bytes: the backend's read stops inside an alpha.
    let long = format!("x{}", "α".repeat(300));
    client.set_title(window, &long);
    client.set_wm_class(window, "res", "cls");
    let (id, mut events) = client.map_and_find_created(&mut backend, window, 200, 150);
    let is_metadata =
        |e: &SurfaceEvent| matches!(e, SurfaceEvent::Metadata { id: m, .. } if *m == id);
    if !events.iter().any(is_metadata) {
        events.extend(drain_until(&mut backend, is_metadata));
    }
    let title = events
        .iter()
        .find_map(|e| match e {
            SurfaceEvent::Metadata { title, .. } => Some(title.as_str().to_owned()),
            _ => None,
        })
        .expect("a Metadata event");
    assert!(title.len() <= 512, "cut to the cap: {} bytes", title.len());
    assert!(
        title.len() > 500,
        "kept almost all of it: {} bytes",
        title.len()
    );
    assert!(title.starts_with("xα"), "{title:?}");
    assert!(title[1..].chars().all(|c| c == 'α'), "{title:?}");
}

// --- borders, synthetic events, EWMH and ICCCM --------------------------------------------

#[test]
fn a_managed_window_loses_its_border() {
    let (_guard, mut backend, mut client) = backend_and_client();
    let window = client.window(200, 150, 0, 0, 5, false);
    let (_, _) = client.map_and_find_created(&mut backend, window, 200, 150);
    assert_eq!(client.geometry(window).4, 0);
}

#[test]
fn forged_structure_events_do_not_move_the_root_or_drop_a_window() {
    let (_guard, mut backend, mut client) = backend_and_client();
    let root_before = backend.root_size().expect("root_size");
    let window = client.toplevel(200, 150);
    let (id, _) = client.map_and_find_created(&mut backend, window, 200, 150);

    let forged_root = x::ConfigureNotifyEvent {
        response_type: x::CONFIGURE_NOTIFY_EVENT,
        sequence: 0,
        event: client.root,
        window: client.root,
        above_sibling: NONE,
        x: 0,
        y: 0,
        width: 5000,
        height: 5000,
        border_width: 0,
        override_redirect: false,
    };
    client
        .conn
        .send_event(
            false,
            client.root,
            x::EventMask::STRUCTURE_NOTIFY,
            forged_root,
        )
        .expect("send_event")
        .check()
        .expect("send_event reply");
    let forged_destroy = x::DestroyNotifyEvent {
        response_type: x::DESTROY_NOTIFY_EVENT,
        sequence: 0,
        event: client.root,
        window,
    };
    client
        .conn
        .send_event(
            false,
            client.root,
            x::EventMask::SUBSTRUCTURE_NOTIFY,
            forged_destroy,
        )
        .expect("send_event")
        .check()
        .expect("send_event reply");

    // A real event after the forged ones: once it is reported, they were handled.
    let marker = client.toplevel(130, 90);
    let (_, events) = client.map_and_find_created(&mut backend, marker, 130, 90);
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, SurfaceEvent::Destroyed { id: gone } if *gone == id)),
        "a forged DestroyNotify dropped a live window: {events:?}"
    );
    assert_eq!(backend.root_size().expect("root_size"), root_before);
}

#[test]
fn the_ewmh_check_window_names_itself_and_the_supported_hints_are_listed() {
    let (_guard, _backend, client) = backend_and_client();
    let check = client.words(client.root, client.net_supporting_wm_check);
    assert_eq!(check.len(), 1);
    assert_eq!(
        client.words(check[0], client.net_supporting_wm_check),
        check,
        "the check window names itself"
    );
    let supported = client.words(client.root, client.net_supported);
    for atom in [
        client.net_supporting_wm_check,
        client.net_wm_name,
        client.net_active_window,
    ] {
        assert!(supported.contains(&atom), "{atom} in {supported:?}");
    }
    let reply = client
        .conn
        .get_property(false, client.root, client.net_supported, XA_ATOM, 0, 64)
        .expect("get_property")
        .reply()
        .expect("property reply");
    assert_eq!(reply.format, 32);
}

#[test]
fn an_unmapped_toplevel_is_marked_withdrawn() {
    let (_guard, mut backend, mut client) = backend_and_client();
    let window = client.toplevel(200, 150);
    let (id, _) = client.map_and_find_created(&mut backend, window, 200, 150);
    assert_eq!(client.words(window, client.wm_state).first(), Some(&1));

    client.unmap(window);
    drain_until(
        &mut backend,
        |e| matches!(e, SurfaceEvent::Destroyed { id: gone } if *gone == id),
    );
    let deadline = Instant::now() + TIMEOUT;
    while client.words(window, client.wm_state).first() != Some(&0) {
        assert!(Instant::now() < deadline, "WM_STATE never became Withdrawn");
        std::thread::sleep(Duration::from_millis(5));
    }
}

// --- draining and layout ------------------------------------------------------------------

#[test]
fn one_drain_handles_a_bounded_number_of_events() {
    let (_guard, mut backend, mut client) = backend_and_client();
    let window = client.toplevel(200, 150);
    let (id, _) = client.map_and_find_created(&mut backend, window, 200, 150);
    // Settle whatever the map produced.
    let mut settle = Vec::new();
    backend.drain_events(&mut settle).expect("drain_events");

    // 1000 one-pixel draws: one DamageNotify each.
    let gc = client.conn.generate_id().expect("xid");
    client
        .conn
        .create_gc(gc, window, &x::CreateGCAux::new().foreground(0x00ff_ffff))
        .expect("create_gc")
        .check()
        .expect("create_gc reply");
    for i in 0..1000i16 {
        client
            .conn
            .poly_point(
                x::CoordMode::ORIGIN,
                window,
                gc,
                &[x::Point {
                    x: i % 200,
                    y: i / 200,
                }],
            )
            .expect("poly_point");
    }
    // A round trip: the server has handled every draw. The notifications still reach the
    // backend's socket at the server's pace, so the drains below poll until they are all in
    // rather than assume they already are.
    client
        .conn
        .get_input_focus()
        .expect("get_input_focus")
        .reply()
        .expect("focus reply");

    let damaged = |batch: &[SurfaceEvent]| {
        batch
            .iter()
            .filter(|e| matches!(e, SurfaceEvent::Damaged { id: d, .. } if *d == id))
            .count()
    };
    let deadline = Instant::now() + TIMEOUT;
    let mut total = 0;
    while total < 1000 && Instant::now() < deadline {
        let mut batch = Vec::new();
        backend.drain_events(&mut batch).expect("drain_events");
        let n = damaged(&batch);
        assert!(n <= 512, "{n} in one drain");
        total += n;
        if n == 0 {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    // More than one drain's worth arrived, so the cap was exercised: no batch above held more
    // than 512, and whatever a drain left behind stayed queued for the next one.
    assert!(total > 512, "only {total} notifications arrived");
}

#[test]
fn a_second_large_toplevel_does_not_land_on_the_first() {
    let (_guard, mut backend, mut client) = backend_and_client();
    let first = client.toplevel(800, 600);
    let (_, _) = client.map_and_find_created(&mut backend, first, 800, 600);
    let second = client.toplevel(800, 601);
    let (_, _) = client.map_and_find_created(&mut backend, second, 800, 601);
    let (x1, y1, ..) = client.geometry(first);
    let (x2, y2, ..) = client.geometry(second);
    assert_eq!((x1, y1), (0, 0));
    assert_ne!((x2, y2), (0, 0), "overlap is kept small, not total");
}
