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
//!   the text the host set;
//! - an app copy (the client taking the selection) makes the backend fetch the new owner's
//!   `UTF8_STRING` and report it as [`appricot_core::SurfaceEvent::ClipboardText`]: the
//!   same text from the same owner is not re-fetched, a host paste re-arms the fetch, a
//!   text over `MAX_CLIPBOARD_BYTES` or a type that is not `UTF8_STRING` reports nothing,
//!   and an owner that never answers costs the backend its fetch deadline and nothing else.
//!
//! As `tests/backend.rs`: a running X server is required and never skipped past, the tests
//! serialize on the root slot (`SubstructureRedirect` is exclusive), and each test destroys
//! the windows it made.

use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use appricot_core::{CaptureBackend, InputSink, MAX_CLIPBOARD_BYTES, Role, Size, SurfaceEvent};
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
    /// The text a `copy` armed, served to the backend's fetch.
    copied: Option<Vec<u8>>,
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
            copied: None,
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

    /// The app copies: takes the CLIPBOARD selection on its window, ready to serve `text`.
    fn copy(&mut self, text: &[u8]) {
        self.conn
            .set_selection_owner(self.windows[0], self.clipboard, CURRENT_TIME)
            .expect("set_selection_owner")
            .check()
            .expect("set_selection_owner reply");
        self.conn.flush().expect("flush");
        self.copied = Some(text.to_vec());
    }

    /// Answers one `SelectionRequest` the backend's fetch made, as a selection owner must:
    /// the text written to the requestor's named property (when `serve` says so), then the
    /// `SelectionNotify`.
    fn answer_fetch(&self, request: &x::SelectionRequestEvent, serve: bool, type_: u32) {
        let property = if request.property == NONE {
            request.target
        } else {
            request.property
        };
        if serve {
            let data = self.copied.clone().unwrap_or_default();
            self.conn
                .change_property(
                    x::PropMode::REPLACE,
                    request.requestor,
                    property,
                    type_,
                    8,
                    u32::try_from(data.len()).expect("small text"),
                    &data,
                )
                .expect("change_property on the requestor")
                .check()
                .expect("change_property reply");
        }
        let notify = x::SelectionNotifyEvent {
            response_type: x::SELECTION_NOTIFY_EVENT,
            sequence: 0,
            requestor: request.requestor,
            selection: request.selection,
            target: request.target,
            property: if serve { property } else { NONE },
            time: request.time,
        };
        self.conn
            .send_event(false, request.requestor, x::EventMask::NO_EVENT, notify)
            .expect("send_event")
            .check()
            .expect("send_event reply");
        self.conn.flush().expect("flush");
    }

    /// Pumps backend and client together until the backend reports a `ClipboardText`, and
    /// returns it with every other surface event drained on the way. The client answers
    /// the backend's fetch itself, playing the app's toolkit.
    fn pump_until_clipboard_text(
        &self,
        backend: &mut X11Backend,
    ) -> Option<(String, Vec<SurfaceEvent>)> {
        let deadline = Instant::now() + TIMEOUT;
        let mut seen = Vec::new();
        while Instant::now() < deadline {
            let mut batch = Vec::new();
            backend.drain_events(&mut batch).expect("drain_events");
            seen.extend(batch);
            while let Some(event) = self.conn.poll_for_event().expect("poll_for_event") {
                if let Event::SelectionRequest(request) = event {
                    // The backend's own fetch: serve the copied text as UTF8_STRING.
                    self.answer_fetch(&request, true, self.utf8_string);
                }
            }
            let found = seen.iter().find_map(|e| match e {
                SurfaceEvent::ClipboardText { text } => Some(text.clone()),
                _ => None,
            });
            if let Some(text) = found {
                return Some((text, seen));
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        None
    }

    /// Pumps both sides for `wait`, answering nothing, and returns every surface event seen.
    /// An owner that never answers is the hostile shape: the backend must stay quiet and
    /// alive, whatever it queued.
    fn pump_silently(&self, backend: &mut X11Backend, wait: Duration) -> Vec<SurfaceEvent> {
        let deadline = Instant::now() + wait;
        let mut seen = Vec::new();
        while Instant::now() < deadline {
            let mut batch = Vec::new();
            backend.drain_events(&mut batch).expect("drain_events");
            seen.extend(batch);
            let _ = self.conn.poll_for_event();
            std::thread::sleep(Duration::from_millis(5));
        }
        seen
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

// -------------------------------------------------------------------------------------------
// App to host: the app copies, the backend fetches
// -------------------------------------------------------------------------------------------

/// How long a silent owner is pumped past the fetch deadline before the test believes it:
/// the backend's own 1 s timeout plus slack.
const PAST_DEADLINE: Duration = Duration::from_millis(1_300);

#[test]
fn an_app_copy_is_fetched_and_reported_once_as_clipboard_text() {
    let (_guard, mut backend, mut client) = backend_and_client();
    let window = client.toplevel(200, 150);
    client.map_and_settle(&mut backend, window, 200, 150);

    // The app copies Greek text; the backend asks the new owner and reports what it got.
    client.copy("αντιγραμμένο στην εφαρμογή".as_bytes());
    let (text, _) = client
        .pump_until_clipboard_text(&mut backend)
        .expect("the copy reaches the host");
    assert_eq!(text, "αντιγραμμένο στην εφαρμογή");

    // The same text copied again by the same flow is a new fetch, but the SESSION the
    // backend reports to keeps the not-twice rule; here only that exactly one event came
    // per copy is asserted (the dedup itself is appricot-core's, tested there).
    client.copy("δεύτερο αντίγραφο".as_bytes());
    let (second, _) = client
        .pump_until_clipboard_text(&mut backend)
        .expect("the second copy reaches the host too");
    assert_eq!(second, "δεύτερο αντίγραφο");
}

#[test]
fn a_host_paste_re_arms_the_fetch_so_the_next_copy_reports_again() {
    let (_guard, mut backend, mut client) = backend_and_client();
    let window = client.toplevel(200, 150);
    client.map_and_settle(&mut backend, window, 200, 150);

    client.copy("same".as_bytes());
    let (first, _) = client
        .pump_until_clipboard_text(&mut backend)
        .expect("the copy reaches the host");
    assert_eq!(first, "same");

    // The host pastes: the backend owns the selection again and serves the app.
    backend.clipboard_set("pasted").expect("clipboard_set");
    client.paste(client.utf8_string);
    let (notify, _) = pump_until_selection_reply(&mut backend, &client);
    assert_eq!(
        notify.property, client.selection_property,
        "the paste is served"
    );

    // The app copies the very same text: the fetch re-triggers, and the event leaves again
    // (the session's not-twice memory was reset by the paste; the fetch itself is fresh).
    client.copy("same".as_bytes());
    let (again, _) = client
        .pump_until_clipboard_text(&mut backend)
        .expect("the copy after a paste reaches the host");
    assert_eq!(again, "same");
}

#[test]
fn a_text_over_the_cap_and_a_non_utf8_type_report_nothing() {
    let (_guard, mut backend, mut client) = backend_and_client();
    let window = client.toplevel(200, 150);
    client.map_and_settle(&mut backend, window, 200, 150);

    // MAX_CLIPBOARD_BYTES + 1 bytes: the wire cannot carry it, so no event, no hang.
    let oversized = vec![b'x'; MAX_CLIPBOARD_BYTES + 1];
    client.copy(&oversized);
    assert!(
        client.pump_until_clipboard_text(&mut backend).is_none(),
        "an over-cap text is fetched and reported as nothing"
    );

    // The same copy answered in a type that is not UTF8_STRING (an INCR, say) is nothing.
    client.copy(b"not utf8 typed");
    let deadline = Instant::now() + TIMEOUT;
    let mut answered = false;
    while Instant::now() < deadline {
        let mut batch = Vec::new();
        backend.drain_events(&mut batch).expect("drain_events");
        assert!(
            !batch
                .iter()
                .any(|e| matches!(e, SurfaceEvent::ClipboardText { .. })),
            "a wrongly typed fetch reports nothing: {batch:?}"
        );
        while let Some(event) = client.conn.poll_for_event().expect("poll_for_event") {
            if let Event::SelectionRequest(request) = event {
                client.answer_fetch(&request, true, client.string);
                answered = true;
            }
        }
        if answered {
            // The wrongly typed answer is in; one more drain proves it changed nothing.
            let mut after = Vec::new();
            backend.drain_events(&mut after).expect("drain_events");
            assert!(
                !after
                    .iter()
                    .any(|e| matches!(e, SurfaceEvent::ClipboardText { .. })),
                "the wrongly typed answer is dropped: {after:?}"
            );
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("the backend never asked for the typed copy within {TIMEOUT:?}");
}

#[test]
fn a_silent_owner_costs_the_deadline_and_nothing_else() {
    let (_guard, mut backend, mut client) = backend_and_client();
    let window = client.toplevel(200, 150);
    client.map_and_settle(&mut backend, window, 200, 150);

    // The app takes the selection and never answers the fetch.
    client.copy(b"never served");
    let seen = client.pump_silently(&mut backend, PAST_DEADLINE);
    assert!(
        !seen
            .iter()
            .any(|e| matches!(e, SurfaceEvent::ClipboardText { .. })),
        "a fetch past its deadline reports nothing: {seen:?}"
    );

    // The backend is alive: the very next real copy is fetched and reported whole.
    client.copy("after the silence".as_bytes());
    let (text, _) = client
        .pump_until_clipboard_text(&mut backend)
        .expect("the backend still fetches after giving one up");
    assert_eq!(text, "after the silence");
}
