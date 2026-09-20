//! Integration tests: the M2 clipboard row against the real X server (docs/roadmap.md M2:
//! "Text clipboard in both directions, only as the host's policy allows, and a clipboard
//! write to the user only inside a user gesture").
//!
//! The test client is an ordinary X client playing the streamed application: it asks the
//! CLIPBOARD selection the backend owns, exactly as a toolkit does on paste. What is
//! asserted is the M2 behaviour on the X side of the wire:
//!
//! - after [`appricot_core::InputSink::clipboard_set`], the backend serves the owned text to
//!   an app's `SelectionRequest` as `UTF8_STRING` and as `STRING`/`TEXT` (Latin-1), and the
//!   served property is the app's to read;
//! - a paste while the backend holds no text is refused on X (`SelectionNotify` naming no
//!   property) and reported once as [`appricot_core::SurfaceEvent::ClipboardRequested`] —
//!   the host's policy is the only thing that can answer it;
//! - `COMPOUND_TEXT`, `MULTIPLE` and `INCR` are refused (`None` property) without disturbing
//!   the text the host set.
//!
//! As `tests/backend.rs`: a running X server is required and never skipped past, the tests
//! serialize on the root slot (`SubstructureRedirect` is exclusive), and each test destroys
//! the windows it made.

use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use appricot_core::{CaptureBackend, InputSink, Role, Size, SurfaceEvent};
use appricot_x11::X11Backend;
use x11rb::connection::Connection as _;
use x11rb::protocol::Event;
use x11rb::protocol::xproto as x;
use x11rb::protocol::xproto::ConnectionExt as _;
use x11rb::rust_connection::RustConnection;
use x11rb::{COPY_DEPTH_FROM_PARENT, COPY_FROM_PARENT, CURRENT_TIME, NONE};

const RUN_HINT: &str = "run inside the dev container: docker compose run --rm dev just test";
const TIMEOUT: Duration = Duration::from_secs(5);

fn display() -> String {
    let display = std::env::var("DISPLAY").unwrap_or_default();
    assert!(!display.is_empty(), "DISPLAY is not set; {RUN_HINT}");
    display
}

/// Xvfb without `-noreset` restarts itself when its last client disconnects, and a
/// restart resets whatever connects next. One connection lives for the whole binary.
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

/// One plain X connection that cleans up after itself, playing the streamed app.
struct TestClient {
    conn: RustConnection,
    root: x::Window,
    utf8_string: u32,
    clipboard: u32,
    string: u32,
    text: u32,
    compound_text: u32,
    multiple: u32,
    incr: u32,
    /// A private atom the selection replies land in.
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
            utf8_string: atom("UTF8_STRING"),
            clipboard: atom("CLIPBOARD"),
            string: atom("STRING"),
            text: atom("TEXT"),
            compound_text: atom("COMPOUND_TEXT"),
            multiple: atom("MULTIPLE"),
            incr: atom("INCR"),
            selection_property: atom("APPRICOT_TEST_CLIPBOARD_REPLY"),
            root,
            conn,
            windows: Vec::new(),
        }
    }

    /// A managed toplevel asking for `width` x `height`, created but not mapped.
    fn toplevel(&mut self, width: u16, height: u16) -> x::Window {
        let window = self.conn.generate_id().expect("xid");
        let aux = x::CreateWindowAux::new()
            .override_redirect(0)
            .background_pixel(0x00_10_20_30);
        self.conn
            .create_window(
                COPY_DEPTH_FROM_PARENT,
                window,
                self.root,
                0,
                0,
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

    fn map(&self, window: x::Window) {
        self.conn
            .map_window(window)
            .expect("map")
            .check()
            .expect("map reply");
        self.conn.flush().expect("flush");
    }

    /// Maps a toplevel and waits until the backend reports it as a surface, so the requestor
    /// window is a settled member of the session the backend manages.
    fn map_and_settle(&self, backend: &mut X11Backend, window: x::Window, width: u32, height: u32) {
        self.map(window);
        let deadline = Instant::now() + TIMEOUT;
        while Instant::now() < deadline {
            let mut batch = Vec::new();
            backend.drain_events(&mut batch).expect("drain_events");
            if batch.iter().any(|e| {
                matches!(
                    e,
                    SurfaceEvent::Created {
                        role: Role::Toplevel,
                        size,
                        ..
                    } if *size == Size::new(width, height)
                )
            }) {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("the backend never reported the toplevel within {TIMEOUT:?}");
    }

    /// Asks the CLIPBOARD selection for `target`, as a toolkit does on paste.
    fn paste(&self, target: u32) {
        self.conn
            .convert_selection(
                self.windows[0],
                self.clipboard,
                target,
                self.selection_property,
                CURRENT_TIME,
            )
            .expect("convert_selection")
            .check()
            .expect("convert_selection reply");
        self.conn.flush().expect("flush");
    }

    /// The reply property's bytes, or empty when the property is gone.
    fn reply_property(&self) -> Vec<u8> {
        self.conn
            .get_property(false, self.windows[0], self.selection_property, NONE, 0, 64)
            .expect("get_property")
            .reply()
            .expect("property reply")
            .value
    }

    /// Blanks the reply property, so a refusal cannot ride on a stale earlier reply.
    fn clear_reply_property(&self) {
        self.conn
            .change_property(
                x::PropMode::REPLACE,
                self.windows[0],
                self.selection_property,
                31u32, /* STRING: any valid type; the value is empty either way */
                8,
                0,
                &[],
            )
            .expect("change_property")
            .check()
            .expect("change_property reply");
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
/// The guard must outlive the test: only one backend at a time may manage the root, and the
/// previous test's backend was dropped a moment ago — the server releases its grip on the
/// root asynchronously, so a `NotWindowManager` right after is a race, not a verdict.
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

/// Pumps the backend and the test client together until the app's `SelectionNotify` arrives,
/// and returns it with every surface event drained on the way. The backend only answers a
/// selection request while `drain_events` runs; nothing else would wake it.
fn pump_until_selection_reply(
    backend: &mut X11Backend,
    client: &TestClient,
) -> (x::SelectionNotifyEvent, Vec<SurfaceEvent>) {
    let deadline = Instant::now() + TIMEOUT;
    let mut seen = Vec::new();
    while Instant::now() < deadline {
        let mut batch = Vec::new();
        backend.drain_events(&mut batch).expect("drain_events");
        seen.extend(batch);
        if let Some(event) = client.conn.poll_for_event().expect("poll_for_event")
            && let Event::SelectionNotify(notify) = event
        {
            return (notify, seen);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("no SelectionNotify within {TIMEOUT:?}; surface events seen: {seen:?}");
}

/// What the backend's Latin-1 encoding of `text` must be: every char up to U+00FF as its
/// byte, everything else as `?` (the documented `STRING`/`TEXT` shape).
fn expected_latin1(text: &str) -> Vec<u8> {
    text.chars()
        .map(|c| if u32::from(c) <= 0xff { c as u8 } else { b'?' })
        .collect()
}

#[test]
fn the_owned_clipboard_is_served_as_utf8_string_and_as_string_and_text() {
    let (_guard, mut backend, mut client) = backend_and_client();
    let window = client.toplevel(200, 150);
    client.map_and_settle(&mut backend, window, 200, 150);

    // The host allowed a paste: the app asks in three target spellings of plain text.
    backend.clipboard_set("café").expect("clipboard_set");

    for target in [client.utf8_string, client.string, client.text] {
        client.paste(target);
        let (notify, _) = pump_until_selection_reply(&mut backend, &client);
        assert_eq!(
            notify.target, target,
            "the notify answers the target that was asked"
        );
        assert_eq!(
            notify.property, client.selection_property,
            "the paste is served into the app's property"
        );
        let reply = client.reply_property();
        if target == client.utf8_string {
            assert_eq!(
                reply,
                "café".as_bytes(),
                "UTF8_STRING carries the UTF-8 bytes"
            );
        } else {
            // STRING and TEXT are the Latin-1 shape: é is one 0xE9 byte, not two.
            assert_eq!(
                reply,
                expected_latin1("café"),
                "STRING/TEXT carry the Latin-1 encoding"
            );
        }
    }
}

#[test]
fn a_paste_with_no_text_is_refused_on_x_and_reported_once_to_the_host() {
    let (_guard, mut backend, mut client) = backend_and_client();
    let window = client.toplevel(200, 150);
    client.map_and_settle(&mut backend, window, 200, 150);

    // The app pastes; the backend holds nothing (the host set no clipboard this session).
    client.paste(client.utf8_string);
    let (notify, drained) = pump_until_selection_reply(&mut backend, &client);

    // The X answer is a refusal: the notify names no property and none was written.
    assert_eq!(notify.target, client.utf8_string);
    assert_eq!(
        notify.property, NONE,
        "a paste with no host text is refused, not half-served"
    );
    assert!(
        client.reply_property().is_empty(),
        "a refused paste writes nothing"
    );

    // And the refusal is exactly one ask to the host: the host's policy decides any answer.
    assert_eq!(
        drained
            .iter()
            .filter(|e| matches!(e, SurfaceEvent::ClipboardRequested))
            .count(),
        1,
        "one paste, one ClipboardRequested, in the drain that answered it: {drained:?}"
    );
}

#[test]
fn compound_text_multiple_and_incr_are_refused_without_disturbing_the_text() {
    let (_guard, mut backend, mut client) = backend_and_client();
    let window = client.toplevel(200, 150);
    client.map_and_settle(&mut backend, window, 200, 150);

    backend
        .clipboard_set("stable apricot text")
        .expect("clipboard_set");

    for target in [client.compound_text, client.multiple, client.incr] {
        client.clear_reply_property();
        if target == client.multiple {
            // A well-formed MULTIPLE: the property names target/property atom pairs. The
            // backend refuses MULTIPLE itself, pairs or no pairs.
            let pairs: Vec<u8> = [client.utf8_string, client.selection_property]
                .iter()
                .flat_map(|atom| atom.to_ne_bytes())
                .collect();
            client
                .conn
                .change_property(
                    x::PropMode::REPLACE,
                    window,
                    client.selection_property,
                    4u32, /* ATOM */
                    32,
                    2,
                    &pairs,
                )
                .expect("change_property")
                .check()
                .expect("change_property reply");
        }
        let before = client.reply_property();
        client.paste(target);
        let (notify, drained) = pump_until_selection_reply(&mut backend, &client);
        assert_eq!(
            notify.property, NONE,
            "target {} is refused with a propertyless notify",
            notify.target
        );
        assert_eq!(
            client.reply_property(),
            before,
            "a refused target writes nothing of its own"
        );
        assert!(
            !drained
                .iter()
                .any(|e| matches!(e, SurfaceEvent::ClipboardRequested)),
            "a refused conversion target is no host ask: {drained:?}"
        );
    }

    // The refusals cost nothing: the owned text is still served whole to the next paste.
    client.paste(client.utf8_string);
    let (notify, _) = pump_until_selection_reply(&mut backend, &client);
    assert_eq!(notify.property, client.selection_property);
    assert_eq!(
        client.reply_property(),
        b"stable apricot text",
        "the owned text survived every refusal"
    );
}
