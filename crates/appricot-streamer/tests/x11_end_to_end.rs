//! The streamer against the real X11 backend, end to end.
//!
//! Every other streamer test drives the server with the mock backend. These run the real server
//! (`ServerState` and the router on a loopback port) around a real `X11Backend` on the
//! container's X server. On one side is a WebSocket client playing the host; on the other a test
//! app, a plain X11 connection that creates, paints and resizes windows and reads the input X
//! delivers to them ([`x11::XApp`]). What they pin, each through the whole stack:
//!
//! 1. a painted window reaches the client as `SurfaceNew` and as frames that decode to its
//!    pixels, and a repaint reaches it too;
//! 2. a click lands on the window it names, and keys go to the window the host focused;
//! 3. a host configure resizes the X window and is acked before a frame at the new size, and one
//!    that changes nothing is acked at once and only once;
//! 4. an app's own resize request is asked for and never applied, and a popup that resizes itself
//!    is acked under serial 0 before any frame at its new size (contract C1);
//! 5. a popup mapped, unmapped and mapped again is two surfaces and one `SurfaceGone`;
//! 6. a resume re-announces the window set, then one cursor message, then full redraws from
//!    sequence 1 (C4), and a key held when the socket dropped was released (C9).
//!
//! Like `crates/appricot-x11/tests`, these need the container's X server and panic rather than
//! skip without one. The backend is the one window manager a display can have, so every test
//! takes one lock for its whole run, with a fresh server, backend and app of its own. Waits
//! poll with deadlines; nothing here sleeps for a fixed time to let something happen.

// The shared mock and harness carry helpers this binary does not exercise.
#[allow(dead_code)]
mod common;
mod x11;

use std::time::Duration;

use appricot_core::Size;
use appricot_encode::decode_tile;
use appricot_proto::limits::codec;
use appricot_proto::wire::{
    self, Body, Configure, FocusNotify, Frame, FrameAck, HelloReply, Key, PointerButton,
    PointerMove, SurfaceGoneReason, decode_envelope,
};
use appricot_streamer::config::set_resume_grace_ms;
use appricot_x11::X11Backend;
use futures_util::stream::SplitSink;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::Message;

use common::harness::{
    Client, TestServer, WAIT, connect_with_retry, envelope, hello_offering, read_body, send,
    spawn_server_around,
};
use x11::XApp;

/// Depth-24 TrueColor pixel values, as the X server stores them.
const RED: u32 = 0x00ff_0000;
const GREEN: u32 = 0x0000_ff00;
const BLUE: u32 = 0x0000_00ff;

/// The X11 keysym of `a`, which a browser reports under the code `KeyA`.
const XK_A: u32 = 0x61;

/// The longest one wait for a message may take, however many other messages arrive meanwhile.
const DEADLINE: Duration = Duration::from_secs(10);

/// One test at a time: `X11Backend` takes the display's window-manager rights, which one client
/// alone may hold. A `tokio` mutex, because the guard is held across every await of a test.
static DISPLAY: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

// --- the server and the host's side of the socket -----------------------------------------

/// Connects a backend to the container's X server. The previous test's backend may still hold
/// the display for a moment after its connection closed, so a takeover conflict is retried.
async fn x11_backend() -> X11Backend {
    x11::keep_server_alive();
    let deadline = Instant::now() + WAIT;
    loop {
        match X11Backend::connect(None) {
            Ok(backend) => return backend,
            Err(e) if e.is_takeover_conflict() && Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Err(e) => panic!(
                "the backend connects to the X server: {e}; {}",
                x11::RUN_HINT
            ),
        }
    }
}

/// Serves a fresh `X11Backend` on an ephemeral loopback port.
async fn serve() -> TestServer<X11Backend> {
    spawn_server_around(x11_backend().await).await
}

/// Opens a session offering `codecs`, resuming `resume_serial` when it names one.
async fn open(
    server: &TestServer<X11Backend>,
    codecs: Vec<u32>,
    resume_serial: Option<u32>,
) -> (Host, HelloReply) {
    let mut ws = connect_with_retry(&server.addr).await;
    send(&mut ws, &hello_offering(codecs, resume_serial)).await;
    match read_body(&mut ws).await {
        Body::HelloReply(reply) => (Host::new(ws), reply),
        other => panic!("expected a HelloReply, got {}", describe(&other)),
    }
}

/// Stops the server and waits until its session is over: the backend, and with it the display,
/// is let go for the next test.
async fn end(server: &TestServer<X11Backend>, host: Host) {
    server.state.shutdown();
    tokio::time::timeout(WAIT, server.state.until_over())
        .await
        .expect("the session ends when the server stops");
    drop(host);
}

/// The client's socket, read without pause.
///
/// A reader task takes every message off the socket as it arrives, so the server never waits
/// on a send while the test waits on the X server. The test reads the messages in order from
/// the inbox, and acks every frame as it reads it: the host drew it.
struct Host {
    sink: SplitSink<Client, Message>,
    inbox: mpsc::UnboundedReceiver<Result<Body, String>>,
    reader: JoinHandle<()>,
}

impl Host {
    fn new(ws: Client) -> Self {
        let (sink, mut stream) = ws.split();
        let (tx, inbox) = mpsc::unbounded_channel();
        let reader = tokio::spawn(async move {
            while let Some(Ok(message)) = stream.next().await {
                let body = match message {
                    Message::Binary(bytes) => match decode_envelope(&bytes) {
                        Ok(wire::Envelope { body: Some(body) }) => Ok(body),
                        Ok(_) => Err("the server sent an empty envelope".to_owned()),
                        Err(e) => Err(format!("the server sent an undecodable envelope: {e}")),
                    },
                    Message::Text(_) => Err("the server sent a text frame".to_owned()),
                    Message::Close(_) => return,
                    _ => continue,
                };
                if tx.send(body).is_err() {
                    return;
                }
            }
        });
        Self {
            sink,
            inbox,
            reader,
        }
    }

    async fn send(&mut self, body: Body) {
        self.sink
            .send(Message::Binary(envelope(body).into()))
            .await
            .expect("the client sends");
    }

    /// The next message, or `None` when none arrives before `deadline`. A frame is acked as it
    /// is read.
    async fn next_before(&mut self, deadline: Instant) -> Option<Body> {
        let body = match tokio::time::timeout_at(deadline, self.inbox.recv()).await {
            Ok(Some(Ok(body))) => body,
            Ok(Some(Err(e))) => panic!("{e}"),
            Ok(None) => panic!("the socket closed"),
            Err(_) => return None,
        };
        if let Body::Frame(frame) = &body {
            let (surface_id, sequence) = (frame.surface_id, frame.sequence);
            self.send(Body::FrameAck(FrameAck {
                surface_id,
                sequence,
            }))
            .await;
        }
        Some(body)
    }

    /// The next message, within [`WAIT`].
    async fn next(&mut self) -> Body {
        self.next_before(Instant::now() + WAIT)
            .await
            .unwrap_or_else(|| panic!("no message within {WAIT:?}"))
    }

    /// Reads until `is` accepts a message, and returns it with every message read before it.
    async fn until(&mut self, what: &str, is: impl Fn(&Body) -> bool) -> (Body, Vec<Body>) {
        let deadline = Instant::now() + DEADLINE;
        let mut before = Vec::new();
        loop {
            let Some(body) = self.next_before(deadline).await else {
                panic!("no {what} within {DEADLINE:?}; got {:?}", summary(&before));
            };
            if is(&body) {
                return (body, before);
            }
            if matches!(body, Body::Bye(_) | Body::ServerError(_)) {
                panic!("got {} while waiting for {what}", describe(&body));
            }
            before.push(body);
        }
    }

    /// Drops the socket without a `Bye`, as a lost network does: the session parks.
    async fn drop_socket(self) {
        let Self {
            sink,
            inbox,
            reader,
        } = self;
        reader.abort();
        let _ = reader.await;
        drop((sink, inbox));
    }
}

/// One line for a message: a frame's pixels and a cursor's image are not worth printing.
fn describe(body: &Body) -> String {
    match body {
        Body::Frame(frame) => describe_frame(frame),
        Body::CursorImage(c) => {
            format!("CursorImage(serial {}, {}x{})", c.serial, c.width, c.height)
        }
        other => format!("{other:?}"),
    }
}

/// One line for a frame: its place in the stream and its tiles' rectangles.
fn describe_frame(frame: &Frame) -> String {
    let rects: Vec<_> = tile_rects(frame)
        .map(|r| (r.x, r.y, r.width, r.height))
        .collect();
    format!(
        "Frame(surface {}, sequence {}, full_redraw {}, tiles {rects:?})",
        frame.surface_id, frame.sequence, frame.full_redraw
    )
}

/// One line per message.
fn summary(bodies: &[Body]) -> Vec<String> {
    bodies.iter().map(describe).collect()
}

// --- messages ------------------------------------------------------------------------------

fn wire_size((width, height): (u16, u16)) -> wire::Size {
    wire::Size {
        width: u32::from(width),
        height: u32::from(height),
    }
}

fn model_size((width, height): (u16, u16)) -> Size {
    Size::new(u32::from(width), u32::from(height))
}

fn configure(surface_id: u32, serial: u32, size: (u16, u16)) -> Body {
    Body::Configure(Configure {
        surface_id,
        serial,
        size: Some(wire_size(size)),
    })
}

/// Presses or releases the key that types `a`, as the client sends it.
fn key_a(pressed: bool) -> Body {
    Body::Key(Key {
        keysym: XK_A,
        code: "KeyA".to_owned(),
        pressed,
        modifiers: 0,
    })
}

fn is_frame_of(body: &Body, surface: u32) -> bool {
    matches!(body, Body::Frame(f) if f.surface_id == surface)
}

fn is_ack_of(body: &Body, surface: u32) -> bool {
    matches!(body, Body::ConfigureAck(a) if a.surface_id == surface)
}

fn is_lifecycle(body: &Body) -> bool {
    matches!(body, Body::SurfaceNew(_) | Body::SurfaceGone(_))
}

fn assert_ack(body: &Body, serial: u32, size: (u16, u16)) {
    let Body::ConfigureAck(ack) = body else {
        panic!("expected a ConfigureAck, got {}", describe(body));
    };
    assert_eq!(
        (ack.serial, ack.size),
        (serial, Some(wire_size(size))),
        "the ConfigureAck of surface {}",
        ack.surface_id
    );
}

/// Asserts that no frame of `surface` among `bodies` reaches past `size`.
fn assert_frames_fit(bodies: &[Body], surface: u32, size: (u16, u16)) {
    for body in bodies {
        if let Body::Frame(frame) = body
            && frame.surface_id == surface
        {
            assert!(
                fits(frame, model_size(size)),
                "{} reaches past {size:?} before the new size was acked",
                describe(body)
            );
        }
    }
}

// --- frames --------------------------------------------------------------------------------

/// A tile's rectangle as `(x, y, width, height)`, when it lies inside `size`.
fn inside(rect: wire::Rect, size: Size) -> Option<(usize, usize, usize, usize)> {
    let x = u32::try_from(rect.x).ok()?;
    let y = u32::try_from(rect.y).ok()?;
    let fits =
        x.checked_add(rect.width)? <= size.width && y.checked_add(rect.height)? <= size.height;
    fits.then_some((
        x as usize,
        y as usize,
        rect.width as usize,
        rect.height as usize,
    ))
}

fn tile_rects(frame: &Frame) -> impl Iterator<Item = wire::Rect> + '_ {
    frame
        .tiles
        .iter()
        .map(|tile| tile.rect.expect("every tile names its rectangle"))
}

/// Whether every tile of `frame` lies inside `size`.
fn fits(frame: &Frame, size: Size) -> bool {
    tile_rects(frame).all(|rect| inside(rect, size).is_some())
}

/// Whether the tiles of `frame` lie inside `size` and cover every pixel of it.
fn covers(frame: &Frame, size: Size) -> bool {
    let width = size.width as usize;
    let mut covered = vec![false; width * size.height as usize];
    for rect in tile_rects(frame) {
        let Some((x, y, w, h)) = inside(rect, size) else {
            return false;
        };
        for row in y..y + h {
            covered[row * width + x..][..w].fill(true);
        }
    }
    covered.iter().all(|pixel| *pixel)
}

/// The blue, green and red bytes of a pixel of `colour`, in a `Bgrx8888` buffer.
fn bgr(colour: u32) -> [u8; 3] {
    let [blue, green, red, _] = colour.to_le_bytes();
    [blue, green, red]
}

/// What the host shows of one surface: the frames' tiles, decoded and drawn in place.
struct Canvas {
    size: Size,
    pixels: Vec<u8>,
}

impl Canvas {
    fn new(size: Size) -> Self {
        Self {
            size,
            pixels: vec![0; 4 * size.width as usize * size.height as usize],
        }
    }

    fn draw(&mut self, frame: &Frame) {
        let stride = 4 * self.size.width as usize;
        for tile in &frame.tiles {
            let rect = tile.rect.expect("every tile names its rectangle");
            let (x, y, w, h) = inside(rect, self.size)
                .unwrap_or_else(|| panic!("tile {rect:?} lies outside {:?}", self.size));
            let pixels = decode_tile(tile.codec, &tile.data, Size::new(rect.width, rect.height))
                .expect("every tile decodes");
            for row in 0..h {
                let from = &pixels.data[row * pixels.stride..][..4 * w];
                self.pixels[(y + row) * stride + 4 * x..][..4 * w].copy_from_slice(from);
            }
        }
    }

    /// Whether every pixel shows `colour`.
    fn shows(&self, colour: u32) -> bool {
        let want = bgr(colour);
        self.pixels.chunks_exact(4).all(|pixel| pixel[..3] == want)
    }
}

/// Draws the frames of `surface` until the canvas shows nothing but `colour`.
async fn until_shows(host: &mut Host, surface: u32, canvas: &mut Canvas, colour: u32) {
    let deadline = Instant::now() + DEADLINE;
    while !canvas.shows(colour) {
        assert!(
            Instant::now() < deadline,
            "surface {surface} never showed {colour:#08x}"
        );
        let what = format!("a frame of surface {surface} while it does not show {colour:#08x}");
        let (body, _) = host.until(&what, |body| is_frame_of(body, surface)).await;
        let Body::Frame(frame) = body else {
            unreachable!("until checked the variant");
        };
        canvas.draw(&frame);
    }
}

// --- windows -------------------------------------------------------------------------------

/// Maps toplevel `window` of `size` and reads its announcement; returns its surface id once
/// the window is viewable.
async fn map_toplevel(host: &mut Host, app: &mut XApp, window: u32, size: (u16, u16)) -> u32 {
    app.map(window);
    let (body, _) = host
        .until(
            "the toplevel's SurfaceNew",
            |body| matches!(body, Body::SurfaceNew(m) if m.size == Some(wire_size(size))),
        )
        .await;
    let Body::SurfaceNew(new) = body else {
        unreachable!("until checked the variant");
    };
    assert_eq!(new.role, wire::Role::Toplevel as i32);
    assert_eq!(new.parent_id, None, "a plain toplevel names no parent");
    assert!(new.positioner.is_none(), "a toplevel has no positioner");
    app.wait_viewable(window).await;
    new.surface_id
}

/// Reads the next popup's announcement, with no other window coming or going before it, and
/// returns its surface id.
async fn popup_announced(host: &mut Host, parent: u32, size: (u16, u16)) -> u32 {
    let (body, before) = host
        .until(
            "a popup's SurfaceNew",
            |body| matches!(body, Body::SurfaceNew(m) if m.role == wire::Role::Popup as i32),
        )
        .await;
    assert!(!before.iter().any(is_lifecycle), "{:?}", summary(&before));
    let Body::SurfaceNew(new) = body else {
        unreachable!("until checked the variant");
    };
    assert_eq!(
        new.parent_id,
        Some(parent),
        "WM_TRANSIENT_FOR names the parent"
    );
    assert_eq!(new.size, Some(wire_size(size)));
    let positioner = new.positioner.expect("a popup is placed by a positioner");
    assert_eq!(positioner.size, Some(wire_size(size)));
    new.surface_id
}

/// Holds the resume grace at a test's value; the default comes back when it drops. The knob
/// is process-wide, and the display lock keeps every other test of this binary out meanwhile.
struct Grace;

impl Grace {
    fn set(ms: u32) -> Self {
        set_resume_grace_ms(ms);
        Self
    }
}

impl Drop for Grace {
    fn drop(&mut self) {
        set_resume_grace_ms(0);
    }
}

// --- 1. window to pixels ------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_painted_window_reaches_the_client_as_its_pixels() {
    let _display = DISPLAY.lock().await;
    let server = serve().await;
    let (mut host, reply) = open(&server, vec![codec::QOI, codec::RAW], None).await;
    assert!(reply.codecs.contains(&codec::QOI), "{:?}", reply.codecs);
    let mut app = XApp::connect();

    // Red from its first pixel: the background is red, and the app paints it red.
    let window = app.toplevel(200, 150, RED);
    let id = map_toplevel(&mut host, &mut app, window, (200, 150)).await;
    app.fill(window, (200, 150), RED);

    let size = Size::new(200, 150);
    let (body, _) = host
        .until("the first frame", |body| is_frame_of(body, id))
        .await;
    let Body::Frame(first) = body else {
        unreachable!("until checked the variant");
    };
    assert!(
        first.full_redraw,
        "a new surface's first frame redraws it all"
    );
    assert_eq!(first.sequence, 1);
    assert!(covers(&first, size), "{}", describe_frame(&first));
    assert!(
        first.tiles.iter().all(|tile| tile.codec == codec::QOI),
        "a solid tile goes out in the negotiated QOI"
    );
    let mut canvas = Canvas::new(size);
    canvas.draw(&first);
    until_shows(&mut host, id, &mut canvas, RED).await;

    // The app repaints the window blue, and a later frame shows it.
    app.fill(window, (200, 150), BLUE);
    until_shows(&mut host, id, &mut canvas, BLUE).await;

    end(&server, host).await;
}

// --- 2. click to the right window ---------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_click_reaches_the_window_it_names_and_keys_the_focused_one() {
    let _display = DISPLAY.lock().await;
    let server = serve().await;
    let (mut host, _) = open(&server, vec![], None).await;
    // Two connections, one window each: what one hears, the other does not.
    let mut focused_app = XApp::connect();
    let mut clicked_app = XApp::connect();
    let focused_window = focused_app.toplevel(200, 150, RED);
    let focused = map_toplevel(&mut host, &mut focused_app, focused_window, (200, 150)).await;
    let clicked_window = clicked_app.toplevel(180, 120, GREEN);
    let clicked = map_toplevel(&mut host, &mut clicked_app, clicked_window, (180, 120)).await;

    // The host focuses one window, then clicks into the other at (10, 20).
    host.send(Body::FocusNotify(FocusNotify {
        surface_id: focused,
    }))
    .await;
    host.send(Body::PointerMove(PointerMove {
        surface_id: clicked,
        x: 10,
        y: 20,
    }))
    .await;
    for pressed in [true, false] {
        host.send(Body::PointerButton(PointerButton {
            surface_id: clicked,
            button: 1,
            pressed,
        }))
        .await;
    }
    let (press, _) = clicked_app
        .wait_event("ButtonPress", |e| {
            e.is_input(x11::BUTTON_PRESS, clicked_window)
        })
        .await;
    assert_eq!(press.detail(), 1, "the left button");
    assert_eq!(press.input_at(), (10, 20), "where the host clicked");
    clicked_app
        .wait_event("ButtonRelease", |e| {
            e.is_input(x11::BUTTON_RELEASE, clicked_window)
        })
        .await;

    // Keys typed afterwards go to the window the host focused, not to the one under the
    // pointer.
    host.send(key_a(true)).await;
    host.send(key_a(false)).await;
    let (key, before) = focused_app
        .wait_event("KeyPress", |e| e.is_input(x11::KEY_PRESS, focused_window))
        .await;
    assert!(
        !before
            .iter()
            .any(|e| matches!(e.code(), x11::BUTTON_PRESS | x11::BUTTON_RELEASE)),
        "the focused window heard the click: {before:?}"
    );
    let (release, _) = focused_app
        .wait_event("KeyRelease", |e| {
            e.is_input(x11::KEY_RELEASE, focused_window)
        })
        .await;
    assert_eq!(release.detail(), key.detail(), "the same key goes up");
    let heard = clicked_app.events_now();
    assert!(
        !heard
            .iter()
            .any(|e| matches!(e.code(), x11::KEY_PRESS | x11::KEY_RELEASE)),
        "the clicked window heard the keys: {heard:?}"
    );

    end(&server, host).await;
}

// --- 3. host configure --------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_host_configure_resizes_the_window_and_is_acked_before_a_frame_at_its_size() {
    let _display = DISPLAY.lock().await;
    let server = serve().await;
    let (mut host, _) = open(&server, vec![], None).await;
    let mut app = XApp::connect();
    let window = app.toplevel(200, 150, RED);
    let id = map_toplevel(&mut host, &mut app, window, (200, 150)).await;
    host.until("the first frame", |body| is_frame_of(body, id))
        .await;

    // The host proposes 300x200 under its serial 5. The X window takes it; the ack comes
    // first, then a frame of the whole new size.
    host.send(configure(id, 5, (300, 200))).await;
    let (ack, before) = host
        .until("ConfigureAck(5)", |body| is_ack_of(body, id))
        .await;
    assert_ack(&ack, 5, (300, 200));
    assert_frames_fit(&before, id, (200, 150));
    let (body, _) = host
        .until("a frame at the new size", |body| is_frame_of(body, id))
        .await;
    let Body::Frame(frame) = body else {
        unreachable!("until checked the variant");
    };
    assert!(
        covers(&frame, Size::new(300, 200)),
        "{}",
        describe_frame(&frame)
    );
    let geometry = app.geometry(window);
    assert_eq!((geometry.width, geometry.height), (300, 200));

    // A configure of the size the window has is acked at once.
    host.send(configure(id, 6, (300, 200))).await;
    let (ack, _) = host
        .until("ConfigureAck(6)", |body| is_ack_of(body, id))
        .await;
    assert_ack(&ack, 6, (300, 200));

    end(&server, host).await;
}

/// Found by this suite: the streamer used to hand a configure that core had already acked at
/// once to the backend all the same (`apply_configure` in src/session.rs). `X11Backend::configure`
/// answers an unchanged size with a `Resized` of its own (C1), and when that echo reached core
/// after the next configure was proposed, core took it for the app's clamp of that configure and
/// acked it with the old size. A no-op configure now never reaches the backend.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_configure_that_changes_nothing_is_answered_once() {
    let _display = DISPLAY.lock().await;
    let server = serve().await;
    let (mut host, _) = open(&server, vec![], None).await;
    let mut app = XApp::connect();
    let window = app.toplevel(200, 150, RED);
    let id = map_toplevel(&mut host, &mut app, window, (200, 150)).await;

    // Each round: a configure of the size the window has, and right behind it one of a new
    // size. The first is acked at once with the size it names; the second with the size the
    // window took. Nothing may answer the second for the first.
    //
    // The app holds the server and queues a backlog of property changes first. The backend
    // can serve the first configure only once the grab is over, and its next drain then works
    // through the backlog before it reports anything, the echo included; the streamer has
    // proposed the second configure long before. So the race goes the same way every time,
    // on an idle machine as on a loaded one.
    let mut size = (200, 150);
    for (round, next) in (0u32..).zip([(260, 180), (300, 200)].repeat(3)) {
        let serial = 10 + 2 * round;
        app.retitle_in_a_grab(window, "busy", 3_000);
        host.send(configure(id, serial, size)).await;
        host.send(configure(id, serial + 1, next)).await;
        let (ack, _) = host
            .until("the no-op's ConfigureAck", |body| is_ack_of(body, id))
            .await;
        assert_ack(&ack, serial, size);
        let (ack, _) = host
            .until("the resize's ConfigureAck", |body| is_ack_of(body, id))
            .await;
        assert_ack(&ack, serial + 1, next);
        app.sync();
        size = next;
    }

    end(&server, host).await;
}

// --- 4. the app resizes itself (C1) -------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_apps_own_resize_request_is_asked_for_and_not_applied() {
    let _display = DISPLAY.lock().await;
    let server = serve().await;
    let (mut host, _) = open(&server, vec![], None).await;
    let mut app = XApp::connect();
    let window = app.toplevel(200, 150, RED);
    let id = map_toplevel(&mut host, &mut app, window, (200, 150)).await;
    let placed = app.geometry(window);

    // After the announcement the host decides the size: the app's request becomes a question.
    app.resize(window, 400, 300);
    let (ask, before) = host
        .until(
            "ResizeAsk",
            |body| matches!(body, Body::ResizeAsk(m) if m.surface_id == id),
        )
        .await;
    let Body::ResizeAsk(ask) = ask else {
        unreachable!("until checked the variant");
    };
    assert_eq!(ask.size, Some(wire_size((400, 300))));
    assert!(
        !before.iter().any(|body| is_ack_of(body, id)),
        "nothing was resized: {:?}",
        summary(&before)
    );

    // The app hears its unchanged geometry, and that is what it keeps.
    let (notify, _) = app
        .wait_event("the window manager's ConfigureNotify", |e| {
            e.is_synthetic() && e.configured().is_some_and(|(w, ..)| w == window)
        })
        .await;
    assert_eq!(notify.configured(), Some((window, 200, 150)));
    let now = app.geometry(window);
    assert_eq!(
        (now.x, now.y, now.width, now.height),
        (placed.x, placed.y, 200, 150),
        "neither the size nor the place changed"
    );

    end(&server, host).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_popup_that_resizes_itself_is_acked_under_serial_0_before_a_frame_at_its_size() {
    let _display = DISPLAY.lock().await;
    let server = serve().await;
    let (mut host, _) = open(&server, vec![], None).await;
    let mut app = XApp::connect();
    let window = app.toplevel(200, 150, RED);
    let parent = map_toplevel(&mut host, &mut app, window, (200, 150)).await;
    let menu = app.popup(window, (20, 30), (100, 40), BLUE);
    app.map(menu);
    let id = popup_announced(&mut host, parent, (100, 40)).await;
    let (body, _) = host
        .until("the popup's first frame", |body| is_frame_of(body, id))
        .await;
    let Body::Frame(first) = body else {
        unreachable!("until checked the variant");
    };
    assert!(
        covers(&first, Size::new(100, 40)),
        "{}",
        describe_frame(&first)
    );

    // An override-redirect window resizes itself with nobody to ask: the size is a fact the
    // host learns under serial 0, before any frame at it.
    app.resize(menu, 160, 60);
    let (ack, before) = host
        .until("the popup's ConfigureAck", |body| is_ack_of(body, id))
        .await;
    assert_ack(&ack, 0, (160, 60));
    assert_frames_fit(&before, id, (100, 40));
    assert!(
        !before
            .iter()
            .any(|body| matches!(body, Body::ResizeAsk(m) if m.surface_id == id)),
        "a size the popup took is no question: {:?}",
        summary(&before)
    );
    let (body, _) = host
        .until("a frame at the new size", |body| is_frame_of(body, id))
        .await;
    let Body::Frame(frame) = body else {
        unreachable!("until checked the variant");
    };
    assert!(
        covers(&frame, Size::new(160, 60)),
        "{}",
        describe_frame(&frame)
    );

    end(&server, host).await;
}

// --- 5. popup lifecycle -------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_popup_mapped_unmapped_and_mapped_again_is_two_surfaces() {
    let _display = DISPLAY.lock().await;
    let server = serve().await;
    let (mut host, _) = open(&server, vec![], None).await;
    let mut app = XApp::connect();
    let window = app.toplevel(200, 150, RED);
    let parent = map_toplevel(&mut host, &mut app, window, (200, 150)).await;
    // Toolkits create a menu window once and map it each time the menu opens.
    let menu = app.popup(window, (30, 40), (90, 30), BLUE);

    app.map(menu);
    let first = popup_announced(&mut host, parent, (90, 30)).await;
    app.unmap(menu);
    let (gone, before) = host
        .until("SurfaceGone", |body| matches!(body, Body::SurfaceGone(_)))
        .await;
    assert!(!before.iter().any(is_lifecycle), "{:?}", summary(&before));
    let Body::SurfaceGone(gone) = gone else {
        unreachable!("until checked the variant");
    };
    assert_eq!(gone.surface_id, first);
    assert_eq!(gone.reason, SurfaceGoneReason::GoneAppClosed as i32);
    app.map(menu);
    let second = popup_announced(&mut host, parent, (90, 30)).await;
    assert!(
        second > first,
        "each map is a new surface: {first} then {second}"
    );

    // A sentinel from the app: its toplevel's title changes. Nothing came or went between the
    // second announcement and it, so the whole story was two SurfaceNew and one SurfaceGone.
    app.set_title(window, "sentinel");
    let (_, before) = host
        .until("the sentinel title", |body| {
            matches!(body, Body::SurfaceMetadata(m)
                if m.surface_id == parent && m.title.as_deref() == Some("sentinel"))
        })
        .await;
    assert!(!before.iter().any(is_lifecycle), "{:?}", summary(&before));

    end(&server, host).await;
}

// --- 6. resume (C4, C9) -------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_resume_reannounces_then_one_cursor_then_redraws_and_the_held_key_was_released() {
    let _display = DISPLAY.lock().await;
    let _grace = Grace::set(3_000);
    let server = serve().await;
    let (mut host, reply) = open(&server, vec![], None).await;
    assert_eq!(
        reply.resume_grace_ms,
        Some(3_000),
        "the reply names the grace honoured"
    );
    let serial = reply.resume_serial.expect("a session can be resumed");
    let mut app = XApp::connect();
    let first_window = app.toplevel(200, 150, RED);
    let first = map_toplevel(&mut host, &mut app, first_window, (200, 150)).await;
    let second_window = app.toplevel(180, 120, GREEN);
    let second = map_toplevel(&mut host, &mut app, second_window, (180, 120)).await;

    // The host focuses the first window and holds a key down in it.
    host.send(Body::FocusNotify(FocusNotify { surface_id: first }))
        .await;
    host.send(key_a(true)).await;
    let (press, _) = app
        .wait_event("KeyPress", |e| e.is_input(x11::KEY_PRESS, first_window))
        .await;

    // The socket drops with the key still down. The server lets the key go (C9).
    host.drop_socket().await;
    let (release, _) = app
        .wait_event("KeyRelease", |e| e.is_input(x11::KEY_RELEASE, first_window))
        .await;
    assert_eq!(release.detail(), press.detail(), "the held key goes up");
    assert!(
        !app.keys_down().contains(&press.detail()),
        "X reports the key down still"
    );

    // Within the grace, the client comes back naming its serial.
    let (mut host, reply) = open(&server, vec![], Some(serial)).await;
    assert!(reply.resumed, "the parked session resumes within the grace");
    expect_resync(
        &mut host,
        &[(first, (200, 150), RED), (second, (180, 120), GREEN)],
    )
    .await;

    end(&server, host).await;
}

/// Reads the resynchronisation that follows `HelloReply(resumed)`, strictly in the order
/// v0.md §7 fixes (contract C4): `SurfaceNew` for every window as it stands, in creation order;
/// exactly one `CursorImage` or `CursorGone`; then one frame per window, each a full redraw at
/// sequence 1 that shows the window as it is. `windows` lists `(surface, size, colour)`.
async fn expect_resync(host: &mut Host, windows: &[(u32, (u16, u16), u32)]) {
    for &(id, size, _) in windows {
        match host.next().await {
            Body::SurfaceNew(new) => {
                assert_eq!((new.surface_id, new.size), (id, Some(wire_size(size))));
                assert_eq!(new.role, wire::Role::Toplevel as i32);
            }
            other => panic!("expected SurfaceNew({id}), got {}", describe(&other)),
        }
    }
    let cursor = host.next().await;
    assert!(
        matches!(cursor, Body::CursorImage(_) | Body::CursorGone(_)),
        "one cursor message follows the window set, got {}",
        describe(&cursor)
    );
    let mut redrawn = Vec::new();
    for _ in windows {
        let body = host.next().await;
        let Body::Frame(frame) = &body else {
            panic!("expected a full redraw, got {}", describe(&body));
        };
        assert!(
            frame.full_redraw && frame.sequence == 1,
            "{}",
            describe(&body)
        );
        let &(_, size, colour) = windows
            .iter()
            .find(|(id, ..)| *id == frame.surface_id)
            .unwrap_or_else(|| panic!("{} names no re-announced window", describe(&body)));
        assert!(covers(frame, model_size(size)), "{}", describe(&body));
        let mut canvas = Canvas::new(model_size(size));
        canvas.draw(frame);
        assert!(canvas.shows(colour), "the redraw shows {colour:#08x}");
        redrawn.push(frame.surface_id);
    }
    redrawn.sort_unstable();
    let mut ids: Vec<u32> = windows.iter().map(|(id, ..)| *id).collect();
    ids.sort_unstable();
    assert_eq!(redrawn, ids, "one full redraw per window");
}
