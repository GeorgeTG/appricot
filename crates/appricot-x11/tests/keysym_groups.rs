//! Integration tests: keysym delivery beyond the keymap's first two columns — a second
//! keyboard group (`setxkbmap us,gr`), the AltGr columns of a single-group layout
//! (`setxkbmap gr`), the dead-key accents of Greek, and the refusal of a keysym the
//! layout does not know (`us`).
//!
//! Every test runs against its own X server: it spawns `Xvfb` on a private display (the
//! server picks the number through `-displayfd`), sets the layout on it with `setxkbmap`,
//! and connects the backend there, so the shared `:99` display the other tests serialize
//! on keeps its keymap untouched. The test client is a small X client: it owns one window,
//! reads the key events X delivers, and recomputes the keysym of each from the server's
//! own keymap — refetched on every `MappingNotify`, so a keycode rebound for a press is
//! read with its rebound row. What is asserted is the delivered keysym sequence; composing
//! a dead key with its base into one glyph is the streamed toolkit's job, not the
//! backend's.

use std::io::Read as _;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use appricot_core::{
    CaptureBackend, InputSink, KeyCode, KeyEvent, Keysym, PressState, Role, Size, SurfaceEvent,
};
use appricot_x11::{BackendError, X11Backend};
use x11rb::connection::Connection as _;
use x11rb::protocol::Event;
use x11rb::protocol::xproto as x;
use x11rb::protocol::xproto::ConnectionExt as _;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;
use x11rb::{COPY_DEPTH_FROM_PARENT, COPY_FROM_PARENT};

const RUN_HINT: &str = "run inside the dev container: docker compose run --rm dev just test";
const TIMEOUT: Duration = Duration::from_secs(10);

// X11 keysyms (keysymdef.h). The Greek ones are the legacy named keysyms a Greek layout
// lists, which is what the app receives however the backend reached the character.
const XK_AT: u32 = 0x0040;
const XK_EUROSIGN: u32 = 0x20ac;
const XK_GREEK_ALPHA: u32 = 0x07e1;
const XK_GREEK_BETA: u32 = 0x07e2;
const XK_GREEK_GAMMA: u32 = 0x07e3;
const XK_GREEK_CAPITAL_ALPHA: u32 = 0x07c1;
const XK_GREEK_CAPITAL_BETA: u32 = 0x07c2;
const XK_GREEK_CAPITAL_GAMMA: u32 = 0x07c3;
const XK_GREEK_OMEGA: u32 = 0x07f9;
const XK_GREEK_SIGMA: u32 = 0x07f2;
const XK_GREEK_FINAL_SIGMA: u32 = 0x07f3;
const XK_GREEK_EPSILON: u32 = 0x07e5;
const XK_GREEK_ETA: u32 = 0x07e7;
const XK_GREEK_IOTA: u32 = 0x07e9;
const XK_GREEK_OMICRON: u32 = 0x07ef;
const XK_GREEK_UPSILON: u32 = 0x07f5;
const XK_DEAD_ACUTE: u32 = 0xfe51;
const XK_DEAD_DIAERESIS: u32 = 0xfe57;

/// The X11 Unicode keysym of a character: `0x01000000 + codepoint`.
const fn unicode(codepoint: u32) -> u32 {
    0x0100_0000 + codepoint
}

/// One private X server with a layout set on it, dead when the test is done.
struct PrivateDisplay {
    display: String,
    server: Child,
}

impl PrivateDisplay {
    /// Spawns `Xvfb` (it picks a free display number and prints it), waits for it to
    /// answer, and sets `layout` on it.
    fn start(layout: &str) -> Self {
        let mut server = Command::new("Xvfb")
            .args([
                "-displayfd",
                "1",
                "-screen",
                "0",
                "1400x900x24",
                "-nolisten",
                "tcp",
                // Without -noreset the server forgets the layout the moment the last
                // setxkbmap connection closes.
                "-noreset",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap_or_else(|e| panic!("spawn Xvfb ({e}); {RUN_HINT}"));
        let number = {
            let mut text = String::new();
            let mut pipe = server.stdout.take().expect("Xvfb stdout");
            let deadline = Instant::now() + TIMEOUT;
            loop {
                let mut byte = [0u8; 1];
                match pipe.read(&mut byte) {
                    Ok(0) => panic!("Xvfb wrote no display number; {RUN_HINT}"),
                    Ok(_) if byte[0] == b'\n' => break,
                    Ok(_) => text.push(byte[0] as char),
                    Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(e) => panic!("read Xvfb's display number: {e}"),
                }
                assert!(Instant::now() < deadline, "Xvfb never named its display");
            }
            text.trim().to_owned()
        };
        assert!(!number.is_empty(), "Xvfb named no display");
        let display = format!(":{number}");
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if x11rb::connect(Some(&display)).is_ok() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "Xvfb on {display} never answered; {RUN_HINT}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let status = Command::new("setxkbmap")
            .arg("-display")
            .arg(&display)
            .arg(layout)
            .status()
            .unwrap_or_else(|e| panic!("run setxkbmap ({e}); {RUN_HINT}"));
        assert!(status.success(), "setxkbmap {layout} failed on {display}");
        Self { display, server }
    }
}

impl Drop for PrivateDisplay {
    fn drop(&mut self) {
        let _ = self.server.kill();
        let _ = self.server.wait();
    }
}

/// One X client with one window: it collects the key events X delivers to the window and
/// names each press's keysym from the server's keymap, which it refetches whenever the
/// server says it changed.
struct KeysClient {
    conn: RustConnection,
    min_keycode: u8,
    max_keycode: u8,
    width: usize,
    /// The server's keyboard map as this client last read it.
    map: Vec<Vec<u32>>,
    /// The keysyms of the presses the client has seen, modifier keys aside.
    delivered: Vec<u32>,
}

impl KeysClient {
    /// Connects, maps one window that takes key events, and waits for the backend to
    /// report and focus it.
    fn new(display: &str, backend: &mut X11Backend) -> Self {
        let (conn, screen) =
            x11rb::connect(Some(display)).unwrap_or_else(|e| panic!("test client: {e}"));
        let root = conn.setup().roots[screen].root;
        let window = conn.generate_id().expect("xid");
        conn.create_window(
            COPY_DEPTH_FROM_PARENT,
            window,
            root,
            0,
            0,
            200,
            150,
            0,
            x::WindowClass::INPUT_OUTPUT,
            COPY_FROM_PARENT,
            &x::CreateWindowAux::new()
                .event_mask(x::EventMask::KEY_PRESS | x::EventMask::KEY_RELEASE)
                .background_pixel(0x00_10_20_30),
        )
        .expect("create_window")
        .check()
        .expect("create_window reply");
        conn.map_window(window)
            .expect("map")
            .check()
            .expect("map reply");
        conn.flush().expect("flush");

        let setup = conn.setup();
        let min = setup.min_keycode;
        let max = setup.max_keycode;
        let reply = conn
            .get_keyboard_mapping(min, max - min + 1)
            .expect("get_keyboard_mapping")
            .reply()
            .expect("keyboard mapping");
        let width = usize::from(reply.keysyms_per_keycode).max(1);
        let map: Vec<Vec<u32>> = reply.keysyms.chunks(width).map(<[_]>::to_vec).collect();

        // The window, mapped and reported by the backend, then focused.
        let size = Size::new(200, 150);
        let deadline = Instant::now() + TIMEOUT;
        let mut id = None;
        while Instant::now() < deadline {
            let mut batch = Vec::new();
            backend.drain_events(&mut batch).expect("drain_events");
            for event in batch {
                if let SurfaceEvent::Created {
                    role: Role::Toplevel,
                    size: event_size,
                    id: found,
                    ..
                } = event
                    && event_size == size
                {
                    id = Some(found);
                }
            }
            if id.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let id = id.expect("the window was reported");
        backend.focus(id).expect("focus");

        let mut client = Self {
            conn,
            min_keycode: min,
            max_keycode: max,
            width,
            map,
            delivered: Vec::new(),
        };
        client.discard_events();
        client
    }

    /// Reads the server's keyboard map again.
    fn refetch_map(&mut self) {
        let reply = self
            .conn
            .get_keyboard_mapping(self.min_keycode, self.max_keycode - self.min_keycode + 1)
            .expect("get_keyboard_mapping")
            .reply()
            .expect("keyboard mapping");
        self.map = reply
            .keysyms
            .chunks(self.width)
            .map(<[_]>::to_vec)
            .collect();
    }

    /// The whole keyboard map as it stands now, for the rebind-hygiene assertion.
    fn current_map(&mut self) -> Vec<Vec<u32>> {
        self.refetch_map();
        self.map.clone()
    }

    /// Throws away every event already queued.
    fn discard_events(&mut self) {
        self.conn.sync().expect("sync");
        while self.pump_one().is_some() {}
        self.delivered.clear();
    }

    /// Handles one queued event, if any.
    fn pump_one(&mut self) -> Option<()> {
        let event = self.conn.poll_for_event().expect("poll_for_event")?;
        match event {
            // The rebound rows must be read as the server holds them, not as an old copy.
            Event::MappingNotify(_) => self.refetch_map(),
            Event::KeyPress(press) => {
                if let Some(keysym) = self.keysym_of(&press)
                    && !is_modifier_keysym(keysym)
                {
                    self.delivered.push(keysym);
                }
            }
            _ => {}
        }
        Some(())
    }

    /// The keysym of a key press as any core client computes it: the column the modifier
    /// state names in the server's own map. The layouts here put every matrix keysym in
    /// one of those columns.
    fn keysym_of(&self, press: &x::KeyPressEvent) -> Option<u32> {
        let row = self
            .map
            .get(usize::from(press.detail.checked_sub(self.min_keycode)?))?;
        let shift = press.state.contains(x::KeyButMask::SHIFT);
        let level3 = press.state.contains(x::KeyButMask::MOD5);
        // Columns 0 and 1 are the Shift pair; the AltGr levels of the active group sit
        // past the repeated group columns (measured: column 4 on these layouts).
        let column = if level3 {
            usize::from(shift) + 4
        } else {
            usize::from(shift)
        };
        let keysym = *row.get(column).unwrap_or(&0);
        (keysym != 0).then_some(keysym)
    }
}

/// True for the keysyms of keys held around a press (Shift, the level-3 shift): their key
/// events are choreography, not typed characters.
fn is_modifier_keysym(keysym: u32) -> bool {
    matches!(keysym, 0xfe03 | 0xff7e | 0xffe1..=0xffee)
}

/// Types `keys` (press, then release) and waits until exactly `expected` was delivered.
fn type_and_expect(
    backend: &mut X11Backend,
    client: &mut KeysClient,
    keys: &[(u32, &str)],
    expected: &[u32],
) {
    for (keysym, code) in keys {
        // Each key is pumped as it is typed: a rebound keycode's row is read while the
        // rebind still holds, not after a later release restored it. The sync is the
        // barrier after which everything the server generated for the stroke is here.
        backend
            .key(KeyEvent {
                keysym: Keysym(*keysym),
                code: KeyCode::new(code),
                state: PressState::Pressed,
            })
            .unwrap_or_else(|e| panic!("press {keysym:#06x}: {e}"));
        std::thread::sleep(Duration::from_millis(5));
        client.conn.sync().expect("sync");
        while client.pump_one().is_some() {}
        backend
            .key(KeyEvent {
                keysym: Keysym(*keysym),
                code: KeyCode::new(code),
                state: PressState::Released,
            })
            .unwrap_or_else(|e| panic!("release {keysym:#06x}: {e}"));
        std::thread::sleep(Duration::from_millis(5));
        client.conn.sync().expect("sync");
        while client.pump_one().is_some() {}
    }
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let mut batch = Vec::new();
        backend.drain_events(&mut batch).expect("drain_events");
        while client.pump_one().is_some() {}
        if client.delivered == expected {
            client.delivered.clear();
            return;
        }
        // Nothing else may arrive either: a longer prefix means a wrong key was typed.
        assert!(
            client.delivered.len() <= expected.len() && Instant::now() <= deadline,
            "delivered {:?} (so far), expected {expected:?}",
            client.delivered
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Presses one keysym and expects the delivery to fail with `KeysymUnavailable`: refused,
/// never typed as another key.
fn refuse(backend: &mut X11Backend, keysym: u32, code: &str) {
    assert!(
        matches!(
            backend.key(KeyEvent {
                keysym: Keysym(keysym),
                code: KeyCode::new(code),
                state: PressState::Pressed,
            }),
            Err(BackendError::KeysymUnavailable(refused)) if refused == keysym
        ),
        "keysym {keysym:#06x} must be refused"
    );
}

#[test]
#[allow(clippy::too_many_lines)] // One test walks the whole four-group matrix; splitting it would hide the sequence.
fn us_plus_gr_delivers_all_four_groups() {
    let display = PrivateDisplay::start("us,gr");
    let mut backend = X11Backend::connect(Some(&display.display)).expect("backend connects");
    let mut client = KeysClient::new(&display.display, &mut backend);
    let pristine_map = client.map.clone();

    // Group 1 of the US layout: Latin lower case, capitals, digits.
    type_and_expect(
        &mut backend,
        &mut client,
        &[(0x61, "KeyA"), (0x62, "KeyB"), (0x63, "KeyC")],
        &[0x61, 0x62, 0x63],
    );
    type_and_expect(
        &mut backend,
        &mut client,
        &[(0x41, "KeyA"), (0x42, "KeyB"), (0x43, "KeyC")],
        &[0x41, 0x42, 0x43],
    );
    type_and_expect(
        &mut backend,
        &mut client,
        &[(0x31, "Digit1"), (0x32, "Digit2"), (0x33, "Digit3")],
        &[0x31, 0x32, 0x33],
    );

    // Group 2: the Greek letters, lower case and capitals, the final sigma included.
    type_and_expect(
        &mut backend,
        &mut client,
        &[
            (unicode(0x03b1), "KeyA"),
            (unicode(0x03b2), "KeyB"),
            (unicode(0x03b3), "KeyC"),
        ],
        &[XK_GREEK_ALPHA, XK_GREEK_BETA, XK_GREEK_GAMMA],
    );
    type_and_expect(
        &mut backend,
        &mut client,
        &[
            (unicode(0x0391), "KeyA"),
            (unicode(0x0392), "KeyB"),
            (unicode(0x0393), "KeyC"),
        ],
        &[
            XK_GREEK_CAPITAL_ALPHA,
            XK_GREEK_CAPITAL_BETA,
            XK_GREEK_CAPITAL_GAMMA,
        ],
    );
    type_and_expect(
        &mut backend,
        &mut client,
        &[
            (unicode(0x03c9), "KeyV"),
            (unicode(0x03c3), "KeyS"),
            (unicode(0x03c2), "KeyS"),
        ],
        &[XK_GREEK_OMEGA, XK_GREEK_SIGMA, XK_GREEK_FINAL_SIGMA],
    );

    // The accented vowels: a dead acute, then the base vowel. The dialytika is a dead
    // diaeresis; the two-mark vowel is both dead keys, diaeresis first, then the base.
    type_and_expect(
        &mut backend,
        &mut client,
        &[
            (unicode(0x03ac), "KeyA"),
            (unicode(0x03ad), "KeyE"),
            (unicode(0x03ae), "KeyH"),
            (unicode(0x03af), "KeyJ"),
            (unicode(0x03cc), "KeyO"),
            (unicode(0x03cd), "KeyY"),
            (unicode(0x03ce), "KeyV"),
        ],
        &[
            XK_DEAD_ACUTE,
            XK_GREEK_ALPHA, //
            XK_DEAD_ACUTE,
            XK_GREEK_EPSILON, //
            XK_DEAD_ACUTE,
            XK_GREEK_ETA, //
            XK_DEAD_ACUTE,
            XK_GREEK_IOTA, //
            XK_DEAD_ACUTE,
            XK_GREEK_OMICRON, //
            XK_DEAD_ACUTE,
            XK_GREEK_UPSILON, //
            XK_DEAD_ACUTE,
            XK_GREEK_OMEGA,
        ],
    );
    type_and_expect(
        &mut backend,
        &mut client,
        &[(unicode(0x03ca), "KeyJ"), (unicode(0x0390), "KeyJ")],
        &[
            XK_DEAD_DIAERESIS,
            XK_GREEK_IOTA, //
            XK_DEAD_DIAERESIS,
            XK_DEAD_ACUTE,
            XK_GREEK_IOTA,
        ],
    );

    // AltGr: the Euro sign and the at.
    type_and_expect(
        &mut backend,
        &mut client,
        &[(unicode(0x20ac), "KeyE"), (XK_AT, "Digit2")],
        &[XK_EUROSIGN, XK_AT],
    );

    // Every rebind was undone: the keyboard map is what it was before any typing. The
    // sync waits out the server's queue, so the last restore's MappingNotify is read too.
    client.conn.sync().expect("sync");
    while client.pump_one().is_some() {}
    assert_eq!(
        client.current_map(),
        pristine_map,
        "a rebind was left in the keymap"
    );
}

#[test]
fn gr_delivers_greek_accents_and_altgr() {
    let display = PrivateDisplay::start("gr");
    let mut backend = X11Backend::connect(Some(&display.display)).expect("backend connects");
    let mut client = KeysClient::new(&display.display, &mut backend);

    // The Greek group is the first: letters direct, capitals through Shift.
    type_and_expect(
        &mut backend,
        &mut client,
        &[
            (unicode(0x03b1), "KeyA"),
            (unicode(0x03b2), "KeyB"),
            (unicode(0x03c9), "KeyV"),
            (unicode(0x03c3), "KeyS"),
            (unicode(0x03c2), "KeyS"),
        ],
        &[
            XK_GREEK_ALPHA,
            XK_GREEK_BETA,
            XK_GREEK_OMEGA,
            XK_GREEK_SIGMA,
            XK_GREEK_FINAL_SIGMA,
        ],
    );
    type_and_expect(
        &mut backend,
        &mut client,
        &[(unicode(0x0391), "KeyA"), (unicode(0x03a9), "KeyV")],
        &[XK_GREEK_CAPITAL_ALPHA, 0x07d9], // Greek_OMEGA
    );

    // The accented vowels: the layout's own dead keys, on the first two columns.
    type_and_expect(
        &mut backend,
        &mut client,
        &[(unicode(0x03ac), "KeyA"), (unicode(0x03af), "KeyJ")],
        &[
            XK_DEAD_ACUTE,
            XK_GREEK_ALPHA, //
            XK_DEAD_ACUTE,
            XK_GREEK_IOTA,
        ],
    );
    type_and_expect(
        &mut backend,
        &mut client,
        &[(unicode(0x03ca), "KeyJ"), (unicode(0x0390), "KeyJ")],
        &[
            XK_DEAD_DIAERESIS,
            XK_GREEK_IOTA, //
            XK_DEAD_DIAERESIS,
            XK_DEAD_ACUTE,
            XK_GREEK_IOTA,
        ],
    );

    // The Euro sign sits at the single group's AltGr level: the level-3 key is held
    // around the press. The at is the digit row's shifted column.
    type_and_expect(
        &mut backend,
        &mut client,
        &[(unicode(0x20ac), "KeyE"), (XK_AT, "Digit2")],
        &[XK_EUROSIGN, XK_AT],
    );

    // Latin is this layout's second... nowhere: the Greek layout holds no Latin letters,
    // and a keysym no column produces is refused, never typed as another key.
    refuse(&mut backend, 0x61, "KeyA");
}

#[test]
fn us_delivers_latin_and_refuses_greek() {
    let display = PrivateDisplay::start("us");
    let mut backend = X11Backend::connect(Some(&display.display)).expect("backend connects");
    let mut client = KeysClient::new(&display.display, &mut backend);

    type_and_expect(
        &mut backend,
        &mut client,
        &[
            (0x61, "KeyA"),
            (0x62, "KeyB"),
            (0x41, "KeyA"),
            (0x31, "Digit1"),
            (XK_AT, "Digit2"),
        ],
        &[0x61, 0x62, 0x41, 0x31, XK_AT],
    );

    // No Greek, no Euro, no accents: nothing in the keymap types them, so the named
    // error is the answer and the session goes on.
    refuse(&mut backend, unicode(0x03b1), "KeyA");
    refuse(&mut backend, unicode(0x03c9), "KeyV");
    refuse(&mut backend, unicode(0x20ac), "KeyE");
    refuse(&mut backend, unicode(0x03ac), "KeyA");

    // The refusals typed nothing.
    type_and_expect(&mut backend, &mut client, &[(0x63, "KeyC")], &[0x63]);
}
