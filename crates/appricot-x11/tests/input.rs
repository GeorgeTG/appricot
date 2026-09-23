//! Integration tests: the input side of [`X11Backend`] against the container's X server —
//! focus, pointer routing across several windows, the wheel, the modifier model, keysyms,
//! the release of held input, and the clipboard's `TARGETS` and `TIMESTAMP`.
//!
//! The test client plays the streamed application: it owns the windows and reads the key,
//! button and motion events X delivers to them. As `tests/backend.rs`: a running X server
//! is required and never skipped past, the tests serialize on the root slot
//! (`SubstructureRedirect` is exclusive), and each test destroys the windows it made.

use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use appricot_core::{
    CaptureBackend, InputSink, KeyCode, KeyEvent, Keysym, Point, PointerButton, PressState, Role,
    Size, SurfaceEvent, SurfaceId,
};
use appricot_x11::{BackendError, X11Backend};
use x11rb::connection::Connection as _;
use x11rb::protocol::Event;
use x11rb::protocol::xproto as x;
use x11rb::protocol::xproto::ConnectionExt as _;
use x11rb::protocol::xtest::ConnectionExt as _;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;
use x11rb::{COPY_DEPTH_FROM_PARENT, COPY_FROM_PARENT, CURRENT_TIME};

const RUN_HINT: &str = "run inside the dev container: docker compose run --rm dev just test";
const TIMEOUT: Duration = Duration::from_secs(5);

// X11 keysyms (keysymdef.h).
const XK_SHIFT_L: u32 = 0xffe1;
const XK_CONTROL_L: u32 = 0xffe3;
const XK_CAPS_LOCK: u32 = 0xffe5;
const XK_ALT_R: u32 = 0xffea;
const XK_END: u32 = 0xff57;
const XK_A: u32 = 0x61;
const XK_B: u32 = 0x62;
const XK_C: u32 = 0x63;
const XK_2: u32 = 0x32;
const XK_7: u32 = 0x37;
const XK_SLASH: u32 = 0x2f;
const XK_GREEK_ALPHA: u32 = 0x07e1;
const XK_GREEK_ALPHA_CAPITAL: u32 = 0x07c1;

/// The X11 Unicode keysym of a character: `0x01000000 + codepoint`.
const fn unicode(codepoint: u32) -> u32 {
    0x0100_0000 + codepoint
}

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

/// Connects a backend, retrying while the previous test's backend still holds the root:
/// the server releases it asynchronously.
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
    let client = TestClient::new();
    (guard, backend, client)
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

/// Everything the windows of a test get: keys, buttons and motion.
fn input_mask() -> x::EventMask {
    x::EventMask::KEY_PRESS
        | x::EventMask::KEY_RELEASE
        | x::EventMask::BUTTON_PRESS
        | x::EventMask::BUTTON_RELEASE
        | x::EventMask::POINTER_MOTION
}

/// One plain X connection that cleans up after itself.
struct TestClient {
    conn: RustConnection,
    root: x::Window,
    clipboard: u32,
    utf8_string: u32,
    targets: u32,
    timestamp: u32,
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
            clipboard: atom("CLIPBOARD"),
            utf8_string: atom("UTF8_STRING"),
            targets: atom("TARGETS"),
            timestamp: atom("TIMESTAMP"),
            selection_property: atom("APPRICOT_TEST_INPUT_REPLY"),
            root,
            conn,
            windows: Vec::new(),
        }
    }

    /// A managed toplevel of `width` x `height` that takes key, button and motion events,
    /// mapped and reported by the backend.
    fn mapped_toplevel(
        &mut self,
        backend: &mut X11Backend,
        width: u16,
        height: u16,
    ) -> (x::Window, SurfaceId) {
        let window = self.conn.generate_id().expect("xid");
        let aux = x::CreateWindowAux::new()
            .event_mask(input_mask())
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
        self.conn
            .map_window(window)
            .expect("map")
            .check()
            .expect("map reply");
        self.conn.flush().expect("flush");
        let size = Size::new(u32::from(width), u32::from(height));
        let events = drain_until(
            backend,
            |e| matches!(e, SurfaceEvent::Created { role: Role::Toplevel, size: s, .. } if *s == size),
        );
        let id = events
            .iter()
            .rev()
            .find_map(|e| match e {
                SurfaceEvent::Created { id, size: s, .. } if *s == size => Some(*id),
                _ => None,
            })
            .expect("a Created event");
        // The map is done once the window is viewable; input before that is lost.
        let deadline = Instant::now() + TIMEOUT;
        while Instant::now() < deadline {
            let attrs = self
                .conn
                .get_window_attributes(window)
                .expect("attributes")
                .reply()
                .expect("attributes reply");
            if attrs.map_state == x::MapState::VIEWABLE {
                return (window, id);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("window {window} never became viewable");
    }

    /// The first server event matching `matches`, within the deadline; everything before
    /// it is handed back too.
    fn events_until<F>(&self, matches: F) -> Vec<Event>
    where
        F: Fn(&Event) -> bool,
    {
        let deadline = Instant::now() + TIMEOUT;
        let mut seen = Vec::new();
        while Instant::now() < deadline {
            if let Some(event) = self.conn.poll_for_event().expect("poll_for_event") {
                let done = matches(&event);
                seen.push(event);
                if done {
                    return seen;
                }
            } else {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        panic!(
            "no matching X event within {TIMEOUT:?}; saw {} events",
            seen.len()
        );
    }

    /// Throws away every event already queued.
    fn discard_events(&self) {
        self.conn.sync().expect("sync");
        while self
            .conn
            .poll_for_event()
            .expect("poll_for_event")
            .is_some()
        {}
    }

    /// The lowest keycode whose column 0 or 1 lists `keysym`.
    fn keycode_of(&self, keysym: u32) -> u8 {
        let setup = self.conn.setup();
        let (min, max) = (setup.min_keycode, setup.max_keycode);
        let map = self
            .conn
            .get_keyboard_mapping(min, max - min + 1)
            .expect("get_keyboard_mapping")
            .reply()
            .expect("keyboard mapping");
        let per = usize::from(map.keysyms_per_keycode);
        map.keysyms
            .chunks(per)
            .zip(min..=max)
            .find(|(syms, _)| syms.iter().take(2).any(|s| *s == keysym))
            .map_or_else(
                || panic!("no keycode for keysym {keysym:#x}"),
                |(_, keycode)| keycode,
            )
    }

    /// The keycodes the server reports down.
    fn keys_down(&self) -> Vec<u8> {
        let keys = self
            .conn
            .query_keymap()
            .expect("query_keymap")
            .reply()
            .expect("keymap reply")
            .keys;
        let mut down = Vec::new();
        for (byte, bits) in (0u8..).zip(keys) {
            for bit in 0..8u8 {
                if bits & (1 << bit) != 0 {
                    down.push(byte * 8 + bit);
                }
            }
        }
        down
    }

    /// The pointer's modifier and button state.
    fn pointer_mask(&self) -> x::KeyButMask {
        self.conn
            .query_pointer(self.root)
            .expect("query_pointer")
            .reply()
            .expect("pointer reply")
            .mask
    }

    /// Waits until the server's key and button state satisfies `done`: XTEST events go
    /// through the server's input queue, a moment after the request.
    fn wait_for_state<F>(&self, what: &str, done: F)
    where
        F: Fn(&Self) -> bool,
    {
        let deadline = Instant::now() + TIMEOUT;
        while Instant::now() < deadline {
            if done(self) {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("{what}: not reached within {TIMEOUT:?}");
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

/// A key event as the streamer builds it from the wire.
fn key(keysym: u32, code: &str, state: PressState) -> KeyEvent {
    KeyEvent {
        keysym: Keysym(keysym),
        code: KeyCode::new(code),
        state,
    }
}

/// Presses and releases one key.
fn tap(backend: &mut X11Backend, keysym: u32, code: &str) {
    backend
        .key(key(keysym, code, PressState::Pressed))
        .expect("key press");
    backend
        .key(key(keysym, code, PressState::Released))
        .expect("key release");
}

/// The key events among `events`, as `(pressed, keycode, window, state)`.
fn key_events(events: &[Event]) -> Vec<(bool, u8, x::Window, x::KeyButMask)> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::KeyPress(k) => Some((true, k.detail, k.event, k.state)),
            Event::KeyRelease(k) => Some((false, k.detail, k.event, k.state)),
            _ => None,
        })
        .collect()
}

#[test]
fn keys_follow_the_host_focus_not_the_pointer() {
    let (_guard, mut backend, mut client) = backend_and_client();
    let (a, a_id) = client.mapped_toplevel(&mut backend, 200, 150);
    let (b, b_id) = client.mapped_toplevel(&mut backend, 210, 150);

    // A key before any focus goes nowhere.
    tap(&mut backend, XK_B, "KeyB");

    backend.focus(a_id).expect("focus A");
    // The user hovers and scrolls over B without clicking, and keeps typing in A.
    backend
        .pointer_motion(b_id, Point::new(20, 20))
        .expect("motion over B");
    backend
        .pointer_axis(b_id, Point::new(0, 1))
        .expect("wheel over B");
    tap(&mut backend, XK_A, "KeyA");

    let key_a = client.keycode_of(XK_A);
    let events = client.events_until(|e| matches!(e, Event::KeyRelease(k) if k.detail == key_a));
    let keys = key_events(&events);
    assert!(
        keys.iter().all(|(_, _, window, _)| *window == a),
        "every key goes to A, the focused window: {keys:?}"
    );
    assert!(!keys.iter().any(|(_, _, window, _)| *window == b));
    assert_eq!(
        keys.first()
            .map(|(pressed, keycode, _, _)| (*pressed, *keycode)),
        Some((true, key_a)),
        "the key typed before any focus was dropped: {keys:?}"
    );
}

#[test]
fn a_click_lands_on_its_window_whatever_mapped_or_restacked_above_it() {
    let (_guard, mut backend, mut client) = backend_and_client();
    // 800x600 windows do not fit side by side in the 1400x900 root: they overlap.
    let (win_a, a_id) = client.mapped_toplevel(&mut backend, 800, 600);
    let (win_b, _) = client.mapped_toplevel(&mut backend, 801, 600);
    backend
        .pointer_motion(a_id, Point::new(100, 100))
        .expect("motion over A");
    client.events_until(|e| matches!(e, Event::MotionNotify(m) if m.event == win_a));

    // A new window maps over A while the pointer stays on A's canvas: the next motion
    // must still reach A.
    let (win_c, _) = client.mapped_toplevel(&mut backend, 802, 600);
    backend
        .pointer_motion(a_id, Point::new(120, 120))
        .expect("motion over A again");
    let events = client.events_until(|e| matches!(e, Event::MotionNotify(m) if m.event != win_b));
    let Some(Event::MotionNotify(motion)) = events.last() else {
        unreachable!("checked in the wait");
    };
    assert_eq!(
        motion.event, win_a,
        "motion went to the window mapped on top"
    );
    assert_eq!((motion.event_x, motion.event_y), (120, 120));

    // The app raises B itself, and another window maps on top: a click with no motion
    // before it still lands on A.
    client
        .conn
        .configure_window(
            win_b,
            &x::ConfigureWindowAux::new().stack_mode(x::StackMode::ABOVE),
        )
        .expect("raise B")
        .check()
        .expect("raise B reply");
    client.conn.flush().expect("flush");
    let mut drained = Vec::new();
    backend.drain_events(&mut drained).expect("drain");
    let (win_d, _) = client.mapped_toplevel(&mut backend, 803, 600);
    client.discard_events();
    backend
        .pointer_button(a_id, PointerButton::Left, PressState::Pressed)
        .expect("press");
    backend
        .pointer_button(a_id, PointerButton::Left, PressState::Released)
        .expect("release");
    let events = client.events_until(|e| matches!(e, Event::ButtonRelease(_)));
    let presses: Vec<x::Window> = events
        .iter()
        .filter_map(|e| match e {
            Event::ButtonPress(press) => Some(press.event),
            _ => None,
        })
        .collect();
    assert_eq!(
        presses,
        vec![win_a],
        "B is {win_b}, C is {win_c}, D is {win_d}"
    );
}

#[test]
fn a_press_with_no_motion_before_it_lands_inside_its_own_window() {
    let (_guard, mut backend, mut client) = backend_and_client();
    let (a, a_id) = client.mapped_toplevel(&mut backend, 200, 150);
    let (b, b_id) = client.mapped_toplevel(&mut backend, 220, 160);
    backend
        .pointer_motion(a_id, Point::new(50, 50))
        .expect("motion over A");
    client.events_until(|e| matches!(e, Event::MotionNotify(m) if m.event == a));

    // A touch tap on B: a press with no motion before it.
    backend
        .pointer_button(b_id, PointerButton::Left, PressState::Pressed)
        .expect("press");
    backend
        .pointer_button(b_id, PointerButton::Left, PressState::Released)
        .expect("release");
    let events = client.events_until(|e| matches!(e, Event::ButtonPress(_)));
    let Some(Event::ButtonPress(press)) = events.last() else {
        unreachable!("checked in the wait");
    };
    assert_eq!(press.event, b, "the tap on B clicked A");
    assert_eq!((press.event_x, press.event_y), (110, 80), "B's middle");
    assert_eq!(press.detail, 1);
    client.events_until(|e| matches!(e, Event::ButtonRelease(r) if r.event == b));
}

#[test]
fn pointer_motion_is_surface_local_and_clamped_to_the_surface() {
    let (_guard, mut backend, mut client) = backend_and_client();
    let (_a, _) = client.mapped_toplevel(&mut backend, 200, 150);
    let (b, b_id) = client.mapped_toplevel(&mut backend, 220, 160);

    backend
        .pointer_motion(b_id, Point::new(30, 40))
        .expect("motion");
    let events = client.events_until(|e| matches!(e, Event::MotionNotify(m) if m.event == b));
    let Some(Event::MotionNotify(motion)) = events.last() else {
        unreachable!("checked in the wait");
    };
    assert_eq!((motion.event_x, motion.event_y), (30, 40));

    backend
        .pointer_motion(b_id, Point::new(-10, 500))
        .expect("motion outside");
    let events = client
        .events_until(|e| matches!(e, Event::MotionNotify(m) if m.event == b && m.event_y != 40));
    let Some(Event::MotionNotify(motion)) = events.last() else {
        unreachable!("checked in the wait");
    };
    assert_eq!(
        (motion.event_x, motion.event_y),
        (0, 159),
        "clamped to B's edge"
    );
}

#[test]
fn the_wheel_maps_to_buttons_four_to_seven_and_is_bounded() {
    let (_guard, mut backend, mut client) = backend_and_client();
    let (a, a_id) = client.mapped_toplevel(&mut backend, 200, 150);
    backend
        .pointer_motion(a_id, Point::new(10, 10))
        .expect("motion");

    let presses_until = |client: &TestClient, last: u8| -> Vec<u8> {
        client
            .events_until(|e| matches!(e, Event::ButtonPress(p) if p.detail == last))
            .iter()
            .filter_map(|e| match e {
                Event::ButtonPress(p) if p.event == a => Some(p.detail),
                _ => None,
            })
            .collect()
    };
    for (steps, button) in [((0, -1), 4u8), ((0, 1), 5), ((-1, 0), 6), ((1, 0), 7)] {
        backend
            .pointer_axis(a_id, Point::new(steps.0, steps.1))
            .expect("axis");
        assert_eq!(
            presses_until(&client, button),
            vec![button],
            "steps {steps:?}"
        );
    }

    // A hostile step count is cut to 64 per axis, and returns at once.
    let started = Instant::now();
    backend
        .pointer_axis(a_id, Point::new(0, i32::MAX))
        .expect("huge axis");
    assert!(started.elapsed() < Duration::from_secs(2), "the call hung");
    backend
        .pointer_axis(a_id, Point::new(1, 0))
        .expect("sentinel");
    let presses = presses_until(&client, 7);
    assert_eq!(presses.len(), 65, "64 steps down, then the sentinel");
    assert!(
        presses[..64].iter().all(|button| *button == 5),
        "{presses:?}"
    );
}

#[test]
fn blur_releases_every_held_key_and_button() {
    let (_guard, mut backend, mut client) = backend_and_client();
    let (a, a_id) = client.mapped_toplevel(&mut backend, 200, 150);
    backend.focus(a_id).expect("focus");
    backend
        .key(key(XK_SHIFT_L, "ShiftLeft", PressState::Pressed))
        .expect("shift");
    backend
        .key(key(0x41, "KeyA", PressState::Pressed))
        .expect("A");
    backend
        .key(key(XK_CONTROL_L, "ControlLeft", PressState::Pressed))
        .expect("ctrl");
    backend
        .pointer_motion(a_id, Point::new(10, 10))
        .expect("motion");
    backend
        .pointer_button(a_id, PointerButton::Left, PressState::Pressed)
        .expect("button");
    let (shift, key_a, control) = (
        client.keycode_of(XK_SHIFT_L),
        client.keycode_of(XK_A),
        client.keycode_of(XK_CONTROL_L),
    );
    client.wait_for_state("keys and button down", |c| {
        let down = c.keys_down();
        [shift, key_a, control].iter().all(|k| down.contains(k))
            && c.pointer_mask().contains(x::KeyButMask::BUTTON1)
    });

    backend.blur().expect("blur");
    let events = client.events_until(|e| matches!(e, Event::ButtonRelease(_)));
    let released: Vec<u8> = key_events(&events)
        .into_iter()
        .filter(|(pressed, _, window, _)| !pressed && *window == a)
        .map(|(_, keycode, _, _)| keycode)
        .collect();
    for keycode in [key_a, control, shift] {
        assert!(
            released.contains(&keycode),
            "{keycode} released: {released:?}"
        );
    }
    client.wait_for_state("nothing down", |c| {
        c.keys_down().is_empty() && !c.pointer_mask().contains(x::KeyButMask::BUTTON1)
    });

    // The host's focus survives the blur: the next key goes back to A.
    client.discard_events();
    tap(&mut backend, XK_A, "KeyA");
    let events = client.events_until(|e| matches!(e, Event::KeyRelease(_)));
    assert!(
        key_events(&events)
            .iter()
            .any(|(pressed, keycode, window, _)| *pressed && *keycode == key_a && *window == a)
    );
}

#[test]
fn a_new_backend_releases_what_an_old_one_left_down() {
    let _guard = the_root();
    let client = TestClient::new();
    // Another XTEST client — a streamer that died — left a key and a button down.
    let key_c = client.keycode_of(XK_C);
    for (kind, detail) in [(2u8, key_c), (4u8, 3u8)] {
        client
            .conn
            .xtest_fake_input(kind, detail, CURRENT_TIME, client.root, 0, 0, 0)
            .expect("xtest")
            .check()
            .expect("xtest reply");
    }
    client.wait_for_state("left down", |c| {
        c.keys_down().contains(&key_c) && c.pointer_mask().contains(x::KeyButMask::BUTTON3)
    });

    let _backend = connect_backend();
    client.wait_for_state("released at connect", |c| {
        !c.keys_down().contains(&key_c) && !c.pointer_mask().contains(x::KeyButMask::BUTTON3)
    });
}

#[test]
fn caps_lock_is_consumed_and_the_keysym_decides_the_case() {
    let (_guard, mut backend, mut client) = backend_and_client();
    let (a, a_id) = client.mapped_toplevel(&mut backend, 200, 150);
    backend.focus(a_id).expect("focus");
    let (key_a, caps) = (client.keycode_of(XK_A), client.keycode_of(XK_CAPS_LOCK));

    tap(&mut backend, XK_CAPS_LOCK, "CapsLock");
    // The browser applied Caps Lock already: 'A' arrives.
    tap(&mut backend, 0x41, "KeyA");
    let events = client.events_until(|e| matches!(e, Event::KeyRelease(k) if k.detail == key_a));
    let keys = key_events(&events);
    assert!(
        !keys.iter().any(|(_, keycode, _, _)| *keycode == caps),
        "{keys:?}"
    );
    let (_, _, window, state) = keys
        .iter()
        .find(|(pressed, keycode, _, _)| *pressed && *keycode == key_a)
        .expect("a press of the A key");
    assert_eq!(*window, a);
    assert!(state.contains(x::KeyButMask::SHIFT), "{state:?}");
    assert!(!state.contains(x::KeyButMask::LOCK), "{state:?}");
    assert!(!client.pointer_mask().contains(x::KeyButMask::LOCK));
}

#[test]
fn a_held_shift_neither_corrupts_a_symbol_nor_is_lost_to_one() {
    let (_guard, mut backend, mut client) = backend_and_client();
    let (_a, a_id) = client.mapped_toplevel(&mut backend, 200, 150);
    backend.focus(a_id).expect("focus");
    let (shift, slash, seven, end, key_a) = (
        client.keycode_of(XK_SHIFT_L),
        client.keycode_of(XK_SLASH),
        client.keycode_of(XK_7),
        client.keycode_of(XK_END),
        client.keycode_of(XK_A),
    );

    // German Shift+7 is '/': the app must see '/', not Shift+slash ('?'). The key comes
    // up reported as '7' after Shift went up in the browser.
    backend
        .key(key(XK_SHIFT_L, "ShiftLeft", PressState::Pressed))
        .expect("shift");
    backend
        .key(key(XK_SLASH, "Digit7", PressState::Pressed))
        .expect("slash");
    backend
        .key(key(XK_7, "Digit7", PressState::Released))
        .expect("7 up");
    let events = client.events_until(|e| matches!(e, Event::KeyRelease(k) if k.detail == slash));
    let keys = key_events(&events);
    let (_, _, _, state) = keys
        .iter()
        .find(|(pressed, keycode, _, _)| *pressed && *keycode == slash)
        .expect("a slash press");
    assert!(!state.contains(x::KeyButMask::SHIFT), "{state:?}");
    assert!(
        !keys.iter().any(|(_, keycode, _, _)| *keycode == seven),
        "{keys:?}"
    );

    // Still holding Shift: a capital, then End. End must arrive with Shift.
    tap(&mut backend, 0x41, "KeyA");
    backend
        .key(key(XK_END, "End", PressState::Pressed))
        .expect("end");
    let events = client.events_until(|e| matches!(e, Event::KeyPress(k) if k.detail == end));
    let keys = key_events(&events);
    let (_, _, _, a_state) = keys
        .iter()
        .find(|(pressed, keycode, _, _)| *pressed && *keycode == key_a)
        .expect("an A press");
    assert!(a_state.contains(x::KeyButMask::SHIFT));
    let (_, _, _, end_state) = keys.last().expect("the End press");
    assert!(end_state.contains(x::KeyButMask::SHIFT), "{end_state:?}");
    assert!(client.keys_down().contains(&shift));
    backend.blur().expect("blur");
}

#[test]
fn windows_altgr_types_text_and_ctrl_letter_stays_a_shortcut() {
    let (_guard, mut backend, mut client) = backend_and_client();
    let (_a, a_id) = client.mapped_toplevel(&mut backend, 200, 150);
    backend.focus(a_id).expect("focus");
    let (two, key_c) = (client.keycode_of(XK_2), client.keycode_of(XK_C));

    // Windows reports AltGr+Q on a German layout as ControlLeft, AltRight, then '@'.
    backend
        .key(key(XK_CONTROL_L, "ControlLeft", PressState::Pressed))
        .expect("ctrl");
    backend
        .key(key(XK_ALT_R, "AltRight", PressState::Pressed))
        .expect("altgr");
    tap(&mut backend, 0x40, "KeyQ");
    let events = client.events_until(|e| matches!(e, Event::KeyRelease(k) if k.detail == two));
    let (_, _, _, state) = key_events(&events)
        .into_iter()
        .find(|(pressed, keycode, _, _)| *pressed && *keycode == two)
        .expect("an '@' press");
    assert!(state.contains(x::KeyButMask::SHIFT), "{state:?}");
    assert!(!state.contains(x::KeyButMask::CONTROL), "{state:?}");
    assert!(!state.contains(x::KeyButMask::MOD1), "{state:?}");
    backend
        .key(key(XK_ALT_R, "AltRight", PressState::Released))
        .expect("altgr up");

    // Control is still held: Ctrl+C is a shortcut, and keeps Control.
    tap(&mut backend, XK_C, "KeyC");
    let events = client.events_until(|e| matches!(e, Event::KeyRelease(k) if k.detail == key_c));
    let (_, _, _, state) = key_events(&events)
        .into_iter()
        .find(|(pressed, keycode, _, _)| *pressed && *keycode == key_c)
        .expect("a C press");
    assert!(state.contains(x::KeyButMask::CONTROL), "{state:?}");
    backend.blur().expect("blur");
}

/// Borrows two keycodes the keymap leaves empty and gives them back on drop.
struct SpareKeycodes<'a> {
    conn: &'a RustConnection,
    first: u8,
    per: u8,
}

impl<'a> SpareKeycodes<'a> {
    /// Maps two adjacent empty keycodes to `lower_a/upper_a` and `lower_b/upper_b`.
    fn map(conn: &'a RustConnection, syms: [(u32, u32); 2]) -> (Self, [u8; 2]) {
        let setup = conn.setup();
        let (min, max) = (setup.min_keycode, setup.max_keycode);
        let map = conn
            .get_keyboard_mapping(min, max - min + 1)
            .expect("get_keyboard_mapping")
            .reply()
            .expect("keyboard mapping");
        let per = map.keysyms_per_keycode;
        let empty: Vec<bool> = map
            .keysyms
            .chunks(usize::from(per))
            .map(|syms| syms.iter().all(|s| *s == 0))
            .collect();
        let first = (min..max)
            .rev()
            .find(|k| empty[usize::from(k - min)] && empty[usize::from(k - min) + 1])
            .expect("two adjacent empty keycodes");
        let mut keysyms = vec![0u32; usize::from(per) * 2];
        for (i, (lower, upper)) in syms.iter().enumerate() {
            keysyms[i * usize::from(per)] = *lower;
            keysyms[i * usize::from(per) + 1] = *upper;
        }
        conn.change_keyboard_mapping(2, first, per, &keysyms)
            .expect("change_keyboard_mapping")
            .check()
            .expect("change_keyboard_mapping reply");
        conn.flush().expect("flush");
        (Self { conn, first, per }, [first, first + 1])
    }
}

impl Drop for SpareKeycodes<'_> {
    fn drop(&mut self) {
        let empty = vec![0u32; usize::from(self.per) * 2];
        let _ = self
            .conn
            .change_keyboard_mapping(2, self.first, self.per, &empty);
        let _ = self.conn.flush();
    }
}

#[test]
fn unicode_keysyms_resolve_directly_or_through_the_greek_names() {
    let (_guard, mut backend, mut client) = backend_and_client();
    let (a, a_id) = client.mapped_toplevel(&mut backend, 200, 150);
    backend.focus(a_id).expect("focus");
    // A Greek layout lists alpha under its legacy name; a Vietnamese one lists 'ơ'
    // (U+01A1) under its Unicode keysym.
    let (_spare, [greek, ohorn]) = SpareKeycodes::map(
        &client.conn,
        [
            (XK_GREEK_ALPHA, XK_GREEK_ALPHA_CAPITAL),
            (unicode(0x01a1), unicode(0x01a0)),
        ],
    );
    // The backend rereads the keymap on MappingNotify, which it sees while draining.
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let mut events = Vec::new();
        backend.drain_events(&mut events).expect("drain");
        match backend.key(key(unicode(0x03b1), "KeyA", PressState::Pressed)) {
            Ok(()) => break,
            Err(BackendError::KeysymUnavailable(_)) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(other) => panic!("alpha did not resolve: {other}"),
        }
    }
    backend
        .key(key(unicode(0x03b1), "KeyA", PressState::Released))
        .expect("alpha up");
    tap(&mut backend, unicode(0x01a1), "KeyO");

    let events = client.events_until(|e| matches!(e, Event::KeyRelease(k) if k.detail == ohorn));
    let presses: Vec<(u8, x::Window)> = key_events(&events)
        .into_iter()
        .filter(|(pressed, _, _, _)| *pressed)
        .map(|(_, keycode, window, _)| (keycode, window))
        .collect();
    assert_eq!(presses, vec![(greek, a), (ohorn, a)]);

    // A character no key produces is refused, never typed as the legacy keysym its low
    // bits spell: U+FF0D is not XK_Return.
    assert!(matches!(
        backend.key(key(unicode(0xff0d), "Minus", PressState::Pressed)),
        Err(BackendError::KeysymUnavailable(sym)) if sym == unicode(0xff0d)
    ));
}

#[test]
fn clipboard_targets_are_atoms_in_format_32_with_timestamp() {
    let (_guard, mut backend, mut client) = backend_and_client();
    let (window, _) = client.mapped_toplevel(&mut backend, 200, 150);
    backend.clipboard_set("hello").expect("clipboard_set");

    let convert = |client: &TestClient, backend: &mut X11Backend, target: u32| {
        let property = client.selection_property;
        client
            .conn
            .delete_property(window, property)
            .expect("delete_property")
            .check()
            .expect("delete reply");
        client
            .conn
            .convert_selection(window, client.clipboard, target, property, CURRENT_TIME)
            .expect("convert_selection")
            .check()
            .expect("convert_selection reply");
        client.conn.flush().expect("flush");
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let mut batch = Vec::new();
            backend.drain_events(&mut batch).expect("drain");
            if let Some(Event::SelectionNotify(n)) =
                client.conn.poll_for_event().expect("poll_for_event")
            {
                assert_eq!(n.property, property, "the conversion was refused");
                break;
            }
            assert!(Instant::now() < deadline, "no SelectionNotify");
            std::thread::sleep(Duration::from_millis(5));
        }
        client
            .conn
            .get_property(false, window, property, 0u32, 0, 64)
            .expect("get_property")
            .reply()
            .expect("property reply")
    };

    let reply = convert(&client, &mut backend, client.targets);
    assert_eq!(reply.type_, 4, "type ATOM");
    assert_eq!(reply.format, 32);
    let targets: Vec<u32> = reply.value32().expect("format 32").collect();
    for wanted in [client.targets, client.timestamp, client.utf8_string] {
        assert!(targets.contains(&wanted), "{wanted} in {targets:?}");
    }

    let reply = convert(&client, &mut backend, client.timestamp);
    assert_eq!(reply.type_, 19, "type INTEGER");
    assert_eq!(reply.format, 32);
    let stamp: Vec<u32> = reply.value32().expect("format 32").collect();
    assert_eq!(stamp.len(), 1);
    assert_ne!(stamp[0], 0, "the time the backend took the selection");
}
