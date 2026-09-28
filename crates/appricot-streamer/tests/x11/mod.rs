//! A minimal X11 client: the streamed "app" of the end-to-end tests.
//!
//! It speaks the core X11 protocol itself, over the display's unix socket. The streamer crate
//! does not depend on x11rb, and these tests add no dependency, so the few requests they need
//! are written out here: windows, two properties, a solid fill, a configure, and the round trips
//! that read state back. It reads the events X delivers to its windows — keys, buttons and
//! configure notifications — the way the real app would.
//!
//! Every request is checked: an X error fails the test at once, naming the major opcode of the
//! request that caused it. Requests and replies are little-endian ('l' in the connection
//! setup), whatever the host's order.
//!
//! The requests and their byte layouts follow Appendix B, "Protocol Encoding", of the X Window
//! System Protocol, X11R7.7
//! (<https://xorg.freedesktop.org/archive/X11R7.7/doc/xproto/x11protocol.html>, checked
//! 2026-09-23), and these tests prove them against Xvfb.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::sync::OnceLock;
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// How long one wait on the X server may take before the test fails.
pub const TIMEOUT: Duration = Duration::from_secs(5);

/// What a test run without a display is told.
pub const RUN_HINT: &str = "run inside the dev container: docker compose run --rm dev just test";

// Core request opcodes.
const CREATE_WINDOW: u8 = 1;
const CHANGE_WINDOW_ATTRIBUTES: u8 = 2;
const GET_WINDOW_ATTRIBUTES: u8 = 3;
const DESTROY_WINDOW: u8 = 4;
const MAP_WINDOW: u8 = 8;
const UNMAP_WINDOW: u8 = 10;
const CONFIGURE_WINDOW: u8 = 12;
const GET_GEOMETRY: u8 = 14;
const CHANGE_PROPERTY: u8 = 18;
const GRAB_SERVER: u8 = 36;
const UNGRAB_SERVER: u8 = 37;
const GET_INPUT_FOCUS: u8 = 43;
const QUERY_KEYMAP: u8 = 44;
const CREATE_PIXMAP: u8 = 53;
const CREATE_GC: u8 = 55;
const FREE_GC: u8 = 60;
const POLY_FILL_RECTANGLE: u8 = 70;
const CREATE_CURSOR: u8 = 93;

// Event codes.
/// `KeyPress`.
pub const KEY_PRESS: u8 = 2;
/// `KeyRelease`.
pub const KEY_RELEASE: u8 = 3;
/// `ButtonPress`.
pub const BUTTON_PRESS: u8 = 4;
/// `ButtonRelease`.
pub const BUTTON_RELEASE: u8 = 5;
/// `ConfigureNotify`.
pub const CONFIGURE_NOTIFY: u8 = 22;
/// `GenericEvent`: the one event longer than 32 bytes. Nothing here selects one.
const GENERIC_EVENT: u8 = 35;

// The events every toplevel of the app selects: keys, buttons, and its own structure (the
// window manager's synthetic `ConfigureNotify` included).
const KEY_PRESS_MASK: u32 = 0x0000_0001;
const KEY_RELEASE_MASK: u32 = 0x0000_0002;
const BUTTON_PRESS_MASK: u32 = 0x0000_0004;
const BUTTON_RELEASE_MASK: u32 = 0x0000_0008;
const STRUCTURE_NOTIFY_MASK: u32 = 0x0002_0000;
const APP_EVENTS: u32 = KEY_PRESS_MASK
    | KEY_RELEASE_MASK
    | BUTTON_PRESS_MASK
    | BUTTON_RELEASE_MASK
    | STRUCTURE_NOTIFY_MASK;

// Predefined atoms: the protocol fixes their values.
const XA_STRING: u32 = 31;
const XA_WINDOW: u32 = 33;
const XA_WM_NAME: u32 = 39;
const XA_WM_TRANSIENT_FOR: u32 = 68;

/// `GetWindowAttributes`' map state of a window that is mapped and all of whose ancestors are.
const VIEWABLE: u8 = 2;

/// Xvfb without `-noreset` resets itself when its last client disconnects, and a reset wipes
/// the next backend's display from under it. One connection lives for the whole binary; the
/// container's Xvfb runs with `-noreset` anyway (docker/dev/entrypoint.sh).
static KEEPALIVE: OnceLock<UnixStream> = OnceLock::new();

/// Opens the keepalive connection unless it is open already.
pub fn keep_server_alive() {
    KEEPALIVE.get_or_init(|| {
        let mut stream = connect_socket();
        let _ = setup(&mut stream);
        stream
    });
}

/// The display's unix socket, from `$DISPLAY`: the container's Xvfb listens on nothing else
/// (`-nolisten tcp`).
fn socket_path() -> String {
    let display = std::env::var("DISPLAY").unwrap_or_default();
    assert!(!display.is_empty(), "DISPLAY is not set; {RUN_HINT}");
    let local = display.strip_prefix(':').unwrap_or_else(|| {
        panic!("DISPLAY={display:?} names no local display; these tests use the unix socket")
    });
    let number = local.split('.').next().unwrap_or(local);
    format!("/tmp/.X11-unix/X{number}")
}

fn connect_socket() -> UnixStream {
    let path = socket_path();
    UnixStream::connect(&path)
        .unwrap_or_else(|e| panic!("cannot reach the X server at {path}: {e}; {RUN_HINT}"))
}

/// What the connection setup tells a client.
struct Setup {
    root: u32,
    id_base: u32,
    id_mask: u32,
}

/// Runs the connection setup: little-endian, protocol 11.0, and no authorisation, because the
/// container's Xvfb runs without an auth file.
fn setup(stream: &mut UnixStream) -> Setup {
    stream
        .write_all(&[b'l', 0, 11, 0, 0, 0, 0, 0, 0, 0, 0, 0])
        .expect("the X server takes the setup");
    let mut head = [0u8; 8];
    stream
        .read_exact(&mut head)
        .expect("the X server answers the setup");
    let mut body = vec![0u8; 4 * usize::from(u16_at(&head, 6))];
    stream
        .read_exact(&mut body)
        .expect("the X server sends the whole setup");
    if head[0] != 1 {
        let reason = body.get(..usize::from(head[1])).unwrap_or_default();
        panic!(
            "the X server refused the connection: {}",
            String::from_utf8_lossy(reason)
        );
    }
    let vendor = usize::from(u16_at(&body, 16)).next_multiple_of(4);
    let formats = usize::from(body[21]);
    let first_screen = 32 + vendor + 8 * formats;
    Setup {
        root: u32_at(&body, first_screen),
        id_base: u32_at(&body, 4),
        id_mask: u32_at(&body, 8),
    }
}

/// Reads whole packets off the socket until it closes: 32 bytes each, more for a reply or a
/// generic event, whose length field counts the 4-byte units past the first 32.
fn read_packets(mut stream: UnixStream, out: &mpsc::Sender<Vec<u8>>) {
    loop {
        let mut packet = vec![0u8; 32];
        if stream.read_exact(&mut packet).is_err() {
            return;
        }
        if packet[0] == 1 || packet[0] & 0x7f == GENERIC_EVENT {
            let extra = 4 * u32_at(&packet, 4) as usize;
            packet.resize(32 + extra, 0);
            if stream.read_exact(&mut packet[32..]).is_err() {
                return;
            }
        }
        if out.send(packet).is_err() {
            return;
        }
    }
}

/// One X event, as the server sent it.
#[derive(Clone, PartialEq, Eq)]
pub struct XEvent(Vec<u8>);

impl XEvent {
    /// The event code, without the synthetic bit.
    pub fn code(&self) -> u8 {
        self.0[0] & 0x7f
    }

    /// True when a client sent it with `SendEvent`, not the server.
    pub fn is_synthetic(&self) -> bool {
        self.0[0] & 0x80 != 0
    }

    /// A key event's keycode, a button event's button.
    pub fn detail(&self) -> u8 {
        self.0[1]
    }

    /// The window a key or button event was reported to.
    pub fn input_window(&self) -> u32 {
        u32_at(&self.0, 12)
    }

    /// Where a key or button event happened, in its window's coordinates.
    pub fn input_at(&self) -> (i16, i16) {
        (i16_at(&self.0, 24), i16_at(&self.0, 26))
    }

    /// A `ConfigureNotify`'s window and size.
    pub fn configured(&self) -> Option<(u32, u16, u16)> {
        (self.code() == CONFIGURE_NOTIFY)
            .then(|| (u32_at(&self.0, 8), u16_at(&self.0, 20), u16_at(&self.0, 22)))
    }

    /// True for a key or button event of `code` reported to `window`.
    pub fn is_input(&self, code: u8, window: u32) -> bool {
        self.code() == code && self.input_window() == window
    }
}

impl std::fmt::Debug for XEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.code() {
            KEY_PRESS | KEY_RELEASE | BUTTON_PRESS | BUTTON_RELEASE => write!(
                f,
                "input(code {}, detail {}, window {:#x}, at {:?})",
                self.code(),
                self.detail(),
                self.input_window(),
                self.input_at()
            ),
            CONFIGURE_NOTIFY => write!(
                f,
                "ConfigureNotify({:?}, synthetic {})",
                self.configured(),
                self.is_synthetic()
            ),
            code => write!(f, "event({code})"),
        }
    }
}

/// A request's body, built field by field in the protocol's order.
#[derive(Default)]
struct Fields(Vec<u8>);

impl Fields {
    fn u8(mut self, value: u8) -> Self {
        self.0.push(value);
        self
    }

    fn u16(mut self, value: u16) -> Self {
        self.0.extend_from_slice(&value.to_le_bytes());
        self
    }

    fn i16(mut self, value: i16) -> Self {
        self.0.extend_from_slice(&value.to_le_bytes());
        self
    }

    fn u32(mut self, value: u32) -> Self {
        self.0.extend_from_slice(&value.to_le_bytes());
        self
    }

    fn bytes(mut self, value: &[u8]) -> Self {
        self.0.extend_from_slice(value);
        self
    }
}

/// A window's position and size, as `GetGeometry` reads them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Geometry {
    /// Its outer corner on the root.
    pub x: i16,
    /// Its outer corner on the root.
    pub y: i16,
    /// Its inside width.
    pub width: u16,
    /// Its inside height.
    pub height: u16,
}

/// The test app: one X connection, the windows it made, and the events X sent it.
///
/// Dropping it destroys its windows and waits until the server has: the next test's backend
/// adopts every mapped window it finds at connect, and must find none of these.
pub struct XApp {
    stream: UnixStream,
    packets: mpsc::Receiver<Vec<u8>>,
    /// The sequence number of the last request sent; the first is 1.
    sequence: u16,
    root: u32,
    id_base: u32,
    id_mask: u32,
    /// The distance between two resource ids: the lowest bit of the mask.
    id_step: u32,
    /// How many resource ids were handed out.
    ids: u32,
    /// Events read while waiting for a reply, oldest first.
    events: VecDeque<XEvent>,
    windows: Vec<u32>,
}

impl XApp {
    /// Connects to `$DISPLAY`.
    pub fn connect() -> Self {
        let mut stream = connect_socket();
        let setup = setup(&mut stream);
        let reader = stream.try_clone().expect("the X socket clones");
        let (tx, packets) = mpsc::channel();
        std::thread::Builder::new()
            .name("x-app-reader".into())
            .spawn(move || read_packets(reader, &tx))
            .expect("the reader thread spawns");
        Self {
            stream,
            packets,
            sequence: 0,
            root: setup.root,
            id_base: setup.id_base,
            id_mask: setup.id_mask,
            id_step: setup.id_mask & setup.id_mask.wrapping_neg(),
            ids: 0,
            events: VecDeque::new(),
            windows: Vec::new(),
        }
    }

    /// Writes one request and returns its sequence number.
    fn try_send(&mut self, opcode: u8, data: u8, fields: Fields) -> std::io::Result<u16> {
        let mut request = vec![opcode, data, 0, 0];
        request.extend(fields.0);
        while !request.len().is_multiple_of(4) {
            request.push(0);
        }
        let words = u16::try_from(request.len() / 4).expect("every request here is small");
        request[2..4].copy_from_slice(&words.to_le_bytes());
        self.stream.write_all(&request)?;
        self.sequence = self.sequence.wrapping_add(1);
        Ok(self.sequence)
    }

    fn send(&mut self, opcode: u8, data: u8, fields: Fields) -> u16 {
        self.try_send(opcode, data, fields)
            .expect("the X server takes the request")
    }

    /// Sends a request that has no reply, and waits until the server has run it.
    fn checked(&mut self, opcode: u8, data: u8, fields: Fields) {
        self.send(opcode, data, fields);
        self.sync();
    }

    /// Waits for the reply to request `sequence`. Events that arrive first are kept; an
    /// error fails the test, whichever request it answers: requests run in order, and every
    /// one before this was checked.
    fn reply(&mut self, sequence: u16, what: &str) -> Vec<u8> {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let packet = self
                .packets
                .recv_timeout(left)
                .unwrap_or_else(|_| panic!("no reply to {what} within {TIMEOUT:?}"));
            match packet[0] {
                0 => panic!(
                    "X error {} (major opcode {}, bad value {:#x}) at or before {what}",
                    packet[1],
                    packet[10],
                    u32_at(&packet, 4)
                ),
                1 if u16_at(&packet, 2) == sequence => return packet,
                1 => {} // a reply nobody waits for: every request with one is waited on
                _ => self.events.push_back(XEvent(packet)),
            }
        }
    }

    /// A round trip: every request sent before it has run, and every event the server
    /// generated for this client before it is queued here.
    pub fn sync(&mut self) {
        let sequence = self.send(GET_INPUT_FOCUS, 0, Fields::default());
        self.reply(sequence, "GetInputFocus");
    }

    fn new_id(&mut self) -> u32 {
        self.ids += 1;
        let offset = self.ids.checked_mul(self.id_step).expect("ids left");
        assert!(
            offset <= self.id_mask,
            "this client ran out of resource ids"
        );
        self.id_base | offset
    }

    /// Builds a `side`×`side` cursor out of two blank depth-1 pixmaps and returns its id.
    /// No font is involved — the container's X server ships no font catalogue — and any
    /// cursor object serves: what the streamer hears is that the display's cursor changed,
    /// not what it looks like.
    pub fn blank_cursor(&mut self, side: u16) -> u32 {
        let source = self.new_id();
        self.checked(
            CREATE_PIXMAP,
            1, // depth
            Fields::default()
                .u32(source)
                .u32(self.root)
                .u16(side)
                .u16(side),
        );
        let mask = self.new_id();
        self.checked(
            CREATE_PIXMAP,
            1,
            Fields::default()
                .u32(mask)
                .u32(self.root)
                .u16(side)
                .u16(side),
        );
        let cursor = self.new_id();
        self.checked(
            CREATE_CURSOR,
            0,
            Fields::default()
                .u32(cursor)
                .u32(source)
                .u32(mask)
                .u16(0)
                .u16(0)
                .u16(0) // foreground black
                .u16(0xffff)
                .u16(0xffff)
                .u16(0xffff) // background white
                .u16(side - 1)
                .u16(side - 1), // the hotspot, inside the source
        );
        cursor
    }

    /// Defines `cursor` on the root: the display's cursor changes, which is what the
    /// streamer's XFixes notification reports.
    pub fn define_cursor(&mut self, cursor: u32) {
        // The root's cursor attribute: bit 14 of the value mask.
        self.checked(
            CHANGE_WINDOW_ATTRIBUTES,
            0,
            Fields::default().u32(self.root).u32(1 << 14).u32(cursor),
        );
    }

    fn create_window(
        &mut self,
        (x, y): (i16, i16),
        (width, height): (u16, u16),
        colour: u32,
        override_redirect: bool,
    ) -> u32 {
        let window = self.new_id();
        // background-pixel (0x2), override-redirect (0x200), event-mask (0x800), in bit order.
        let fields = Fields::default()
            .u32(window)
            .u32(self.root)
            .i16(x)
            .i16(y)
            .u16(width)
            .u16(height)
            .u16(0) // border width
            .u16(1) // InputOutput
            .u32(0) // the parent's visual
            .u32(0x0000_0a02)
            .u32(colour)
            .u32(u32::from(override_redirect))
            .u32(APP_EVENTS);
        self.checked(CREATE_WINDOW, 0, fields);
        self.windows.push(window);
        window
    }

    /// A toplevel of `width` x `height` whose background is `colour`, a depth-24 pixel
    /// value. It is not mapped yet.
    pub fn toplevel(&mut self, width: u16, height: u16, colour: u32) -> u32 {
        self.create_window((0, 0), (width, height), colour, false)
    }

    /// An override-redirect popup at root position `at`, `WM_TRANSIENT_FOR` naming `parent`.
    /// It is not mapped yet.
    pub fn popup(&mut self, parent: u32, at: (i16, i16), size: (u16, u16), colour: u32) -> u32 {
        let popup = self.create_window(at, size, colour, true);
        self.change_property(
            popup,
            XA_WM_TRANSIENT_FOR,
            XA_WINDOW,
            32,
            &parent.to_le_bytes(),
        );
        popup
    }

    fn change_property(&mut self, window: u32, property: u32, kind: u32, format: u8, data: &[u8]) {
        let fields = property_fields(window, property, kind, format, data);
        self.checked(CHANGE_PROPERTY, 0, fields); // mode Replace
    }

    /// Sets `WM_NAME`, a Latin-1 title.
    pub fn set_title(&mut self, window: u32, title: &str) {
        self.change_property(window, XA_WM_NAME, XA_STRING, 8, title.as_bytes());
    }

    /// Grabs the server, and returns once the grab is in force, having written `times`
    /// changes of `WM_NAME` to `title` and the ungrab behind it, unanswered: [`XApp::sync`]
    /// waits for them later.
    ///
    /// Until the server has run all of them, no other client's request runs. When it has,
    /// the window manager finds every `PropertyNotify` queued at once and reads the window's
    /// state back for each, one round trip apiece: a backlog that keeps the backend busy for a
    /// while, and anything it is asked meanwhile is answered only after the ungrab.
    pub fn retitle_in_a_grab(&mut self, window: u32, title: &str, times: usize) {
        self.send(GRAB_SERVER, 0, Fields::default());
        self.sync();
        for _ in 0..times {
            let fields = property_fields(window, XA_WM_NAME, XA_STRING, 8, title.as_bytes());
            self.send(CHANGE_PROPERTY, 0, fields);
        }
        self.send(UNGRAB_SERVER, 0, Fields::default());
    }

    /// Maps `window`: a toplevel asks the window manager, a popup maps at once.
    pub fn map(&mut self, window: u32) {
        self.checked(MAP_WINDOW, 0, Fields::default().u32(window));
    }

    /// Unmaps `window`.
    pub fn unmap(&mut self, window: u32) {
        self.checked(UNMAP_WINDOW, 0, Fields::default().u32(window));
    }

    /// Asks for a new size, the way an app does: a managed toplevel's request goes to the
    /// window manager, a popup's is applied at once.
    pub fn resize(&mut self, window: u32, width: u32, height: u32) {
        let fields = Fields::default()
            .u32(window)
            .u16(0x0004 | 0x0008) // width, height
            .u16(0)
            .u32(width)
            .u32(height);
        self.checked(CONFIGURE_WINDOW, 0, fields);
    }

    /// Fills the whole of `window`'s `width` x `height` with `colour`.
    pub fn fill(&mut self, window: u32, (width, height): (u16, u16), colour: u32) {
        let gc = self.new_id();
        let fields = Fields::default()
            .u32(gc)
            .u32(window)
            .u32(0x0000_0004) // foreground
            .u32(colour);
        self.send(CREATE_GC, 0, fields);
        let fields = Fields::default()
            .u32(window)
            .u32(gc)
            .i16(0)
            .i16(0)
            .u16(width)
            .u16(height);
        self.send(POLY_FILL_RECTANGLE, 0, fields);
        self.checked(FREE_GC, 0, Fields::default().u32(gc));
    }

    /// `window`'s geometry now.
    pub fn geometry(&mut self, window: u32) -> Geometry {
        let sequence = self.send(GET_GEOMETRY, 0, Fields::default().u32(window));
        let reply = self.reply(sequence, "GetGeometry");
        Geometry {
            x: i16_at(&reply, 12),
            y: i16_at(&reply, 14),
            width: u16_at(&reply, 16),
            height: u16_at(&reply, 18),
        }
    }

    /// Waits until `window` is viewable: input aimed at it before then is lost.
    pub async fn wait_viewable(&mut self, window: u32) {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let sequence = self.send(GET_WINDOW_ATTRIBUTES, 0, Fields::default().u32(window));
            if self.reply(sequence, "GetWindowAttributes")[26] == VIEWABLE {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "window {window:#x} never became viewable"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    /// The keycodes the server reports down.
    pub fn keys_down(&mut self) -> Vec<u8> {
        let sequence = self.send(QUERY_KEYMAP, 0, Fields::default());
        let reply = self.reply(sequence, "QueryKeymap");
        let mut down = Vec::new();
        for (byte, bits) in (0u8..).zip(&reply[8..40]) {
            for bit in 0..8u8 {
                if bits & (1 << bit) != 0 {
                    down.push(byte * 8 + bit);
                }
            }
        }
        down
    }

    /// Moves every packet already read into the event queue, without waiting.
    fn take_ready(&mut self) {
        while let Ok(packet) = self.packets.try_recv() {
            match packet[0] {
                0 => panic!(
                    "X error {} (major opcode {}, bad value {:#x})",
                    packet[1],
                    packet[10],
                    u32_at(&packet, 4)
                ),
                1 => {}
                _ => self.events.push_back(XEvent(packet)),
            }
        }
    }

    /// Waits for the first event `matches` accepts, and returns it with the events read
    /// before it, which are consumed too. It yields while it waits, so the server runs.
    pub async fn wait_event(
        &mut self,
        what: &str,
        matches: impl Fn(&XEvent) -> bool,
    ) -> (XEvent, Vec<XEvent>) {
        let deadline = Instant::now() + TIMEOUT;
        let mut before = Vec::new();
        loop {
            self.take_ready();
            while let Some(event) = self.events.pop_front() {
                if matches(&event) {
                    return (event, before);
                }
                before.push(event);
            }
            assert!(
                Instant::now() < deadline,
                "no X event {what} within {TIMEOUT:?}; got {before:?}"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    /// Every event the server generated for this client so far, after a round trip.
    pub fn events_now(&mut self) -> Vec<XEvent> {
        self.sync();
        self.take_ready();
        self.events.drain(..).collect()
    }
}

impl Drop for XApp {
    fn drop(&mut self) {
        // Never panics: the test may be unwinding already. Errors are ignored; the window
        // may be gone (a popup's parent destroyed first takes its child with it).
        for window in std::mem::take(&mut self.windows) {
            let _ = self.try_send(DESTROY_WINDOW, 0, Fields::default().u32(window));
        }
        if let Ok(sequence) = self.try_send(GET_INPUT_FOCUS, 0, Fields::default()) {
            let deadline = Instant::now() + TIMEOUT;
            while let Ok(packet) = self
                .packets
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            {
                if packet[0] == 1 && u16_at(&packet, 2) == sequence {
                    break;
                }
            }
        }
        let _ = self.stream.shutdown(Shutdown::Both);
    }
}

/// A `ChangeProperty` request's fields, replacing `property` of `window` with `data`.
fn property_fields(window: u32, property: u32, kind: u32, format: u8, data: &[u8]) -> Fields {
    let units = u32::try_from(data.len() / usize::from(format / 8)).expect("a small property");
    Fields::default()
        .u32(window)
        .u32(property)
        .u32(kind)
        .u8(format)
        .bytes(&[0, 0, 0])
        .u32(units)
        .bytes(data)
}

fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn i16_at(bytes: &[u8], at: usize) -> i16 {
    i16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}
