//! A stand-in application, so the harness can prove itself without the pilot application.
//!
//! It does, on a timeline, the things the spike measures in a real application:
//!
//! - a toplevel with a table that scrolls one row every [`SCROLL_EVERY`] (a whole-window
//!   repaint) and one cell that refreshes every second (a small repaint);
//! - an override-redirect popup, mapped at 1 s and unmapped at 2 s;
//! - a transient dialog (`WM_TRANSIENT_FOR`, `_NET_WM_WINDOW_TYPE_DIALOG`, `_MOTIF_WM_HINTS`)
//!   mapped at 2.5 s.
//!
//! It reports every key it receives, with the keysym its keyboard mapping gives, so a test can
//! check what the streamer's XTEST input actually typed.

use std::io::Write as _;
use std::time::{Duration, Instant};

use x11rb::COPY_DEPTH_FROM_PARENT;
use x11rb::connection::Connection as _;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{
    AtomEnum, ConnectionExt as _, CreateGCAux, CreateWindowAux, EventMask, Gcontext, KeyButMask,
    PropMode, Rectangle, Window, WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

use crate::control::Control;

/// How often the table scrolls.
pub const SCROLL_EVERY: Duration = Duration::from_millis(250);

/// The toplevel's initial size.
pub const TOPLEVEL_SIZE: (u16, u16) = (640, 400);

/// The popup's size.
pub const POPUP_SIZE: (u16, u16) = (160, 120);

/// The dialog's size.
pub const DIALOG_SIZE: (u16, u16) = (300, 200);

const ROW: u16 = 20;

/// One key the application received.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyReport {
    /// The X keycode.
    pub keycode: u8,
    /// The modifier state at the press.
    pub state: u16,
    /// The keysym the keyboard mapping gives for that keycode and state.
    pub keysym: u32,
    /// Press or release.
    pub pressed: bool,
}

/// The windows the application made.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FakeWindows {
    /// The toplevel.
    pub toplevel: Window,
    /// The override-redirect popup.
    pub popup: Window,
    /// The transient dialog.
    pub dialog: Window,
}

/// What stops the application.
#[derive(Debug)]
pub enum FakeAppError {
    /// No X server.
    Connect(x11rb::errors::ConnectError),
    /// The connection failed.
    Connection(x11rb::errors::ConnectionError),
    /// A request failed.
    Reply(x11rb::errors::ReplyError),
    /// An id could not be allocated.
    Id(x11rb::errors::ReplyOrIdError),
}

impl std::fmt::Display for FakeAppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Connect(e) => write!(f, "cannot connect to the X server: {e}"),
            Self::Connection(e) => write!(f, "the X connection failed: {e}"),
            Self::Reply(e) => write!(f, "an X request failed: {e}"),
            Self::Id(e) => write!(f, "an X id could not be allocated: {e}"),
        }
    }
}

impl std::error::Error for FakeAppError {}

impl From<x11rb::errors::ConnectError> for FakeAppError {
    fn from(e: x11rb::errors::ConnectError) -> Self {
        Self::Connect(e)
    }
}
impl From<x11rb::errors::ConnectionError> for FakeAppError {
    fn from(e: x11rb::errors::ConnectionError) -> Self {
        Self::Connection(e)
    }
}
impl From<x11rb::errors::ReplyError> for FakeAppError {
    fn from(e: x11rb::errors::ReplyError) -> Self {
        Self::Reply(e)
    }
}
impl From<x11rb::errors::ReplyOrIdError> for FakeAppError {
    fn from(e: x11rb::errors::ReplyOrIdError) -> Self {
        Self::Id(e)
    }
}

/// How the application runs.
#[derive(Debug, Clone, Default)]
pub struct FakeAppConfig {
    /// The display, or `None` for `$DISPLAY`.
    pub display: Option<String>,
    /// Exit after this long, when set.
    pub duration: Option<Duration>,
    /// Print each key to stdout.
    pub print_keys: bool,
}

struct Keymap {
    min: u8,
    per: u8,
    syms: Vec<u32>,
}

impl Keymap {
    fn fetch(conn: &RustConnection) -> Result<Self, FakeAppError> {
        let setup = conn.setup();
        let (min, max) = (setup.min_keycode, setup.max_keycode);
        let reply = conn.get_keyboard_mapping(min, max - min + 1)?.reply()?;
        Ok(Self {
            min,
            per: reply.keysyms_per_keycode,
            syms: reply.keysyms,
        })
    }

    /// The keysym of `keycode` under `state`: level 3 (index 4) under Mod5, level 2 under
    /// Shift, falling back to the first entry where the chosen one is empty.
    fn lookup(&self, keycode: u8, state: u16) -> u32 {
        let per = usize::from(self.per);
        let Some(row) = usize::from(keycode)
            .checked_sub(usize::from(self.min))
            .map(|r| r * per)
        else {
            return 0;
        };
        let entries = self.syms.get(row..row + per).unwrap_or(&[]);
        let mut index = usize::from(state & u16::from(KeyButMask::SHIFT) != 0);
        if state & u16::from(KeyButMask::MOD5) != 0 {
            index += 4;
        }
        match entries.get(index) {
            Some(&sym) if sym != 0 => sym,
            _ => entries.first().copied().unwrap_or(0),
        }
    }
}

struct App {
    conn: RustConnection,
    root: Window,
    gc: Gcontext,
    windows: FakeWindows,
    size: (u16, u16),
    offset: u16,
    tick: u32,
}

/// Runs the application until `cfg.duration` passes or `control` asks to stop. `on_key` sees
/// every key event.
pub fn run(
    cfg: &FakeAppConfig,
    control: Option<&Control>,
    mut on_key: impl FnMut(KeyReport),
) -> Result<FakeWindows, FakeAppError> {
    let (conn, screen) = x11rb::connect(cfg.display.as_deref())?;
    let root = conn.setup().roots[screen].root;
    let mut keymap = Keymap::fetch(&conn)?;
    let gc = conn.generate_id()?;
    conn.create_gc(gc, root, &CreateGCAux::new().foreground(0))?;
    let mut app = App {
        conn,
        root,
        gc,
        windows: FakeWindows::default(),
        size: TOPLEVEL_SIZE,
        offset: 0,
        tick: 0,
    };
    app.windows.toplevel = app.toplevel()?;
    app.conn.flush()?;

    let started = Instant::now();
    let (mut popup_mapped, mut popup_done, mut dialog_done) = (false, false, false);
    let mut next_scroll = started + SCROLL_EVERY;
    loop {
        while let Some(event) = app.conn.poll_for_event()? {
            match event {
                Event::KeyPress(e) | Event::KeyRelease(e) => {
                    let pressed = matches!(event, Event::KeyPress(_));
                    let report = KeyReport {
                        keycode: e.detail,
                        state: u16::from(e.state),
                        keysym: keymap.lookup(e.detail, u16::from(e.state)),
                        pressed,
                    };
                    if cfg.print_keys {
                        let _ = writeln!(
                            std::io::stdout().lock(),
                            "fake-app: key {} keycode={} state={:#x} keysym={:#x}",
                            if pressed { "press" } else { "release" },
                            report.keycode,
                            report.state,
                            report.keysym
                        );
                    }
                    on_key(report);
                }
                Event::MappingNotify(_) => keymap = Keymap::fetch(&app.conn)?,
                Event::ConfigureNotify(e) if e.window == app.windows.toplevel => {
                    app.size = (e.width, e.height);
                }
                Event::Expose(e) if e.window == app.windows.toplevel && e.count == 0 => {
                    app.paint()?;
                }
                _ => {}
            }
        }
        let t = started.elapsed();
        if !popup_mapped && t >= Duration::from_secs(1) {
            popup_mapped = true;
            app.windows.popup = app.popup()?;
        }
        if popup_mapped && !popup_done && t >= Duration::from_secs(2) {
            popup_done = true;
            app.conn.unmap_window(app.windows.popup)?;
        }
        if !dialog_done && t >= Duration::from_millis(2500) {
            dialog_done = true;
            app.windows.dialog = app.dialog()?;
        }
        if Instant::now() >= next_scroll {
            next_scroll += SCROLL_EVERY;
            app.offset = (app.offset + 1) % 1000;
            app.tick += 1;
            app.paint()?;
        }
        app.conn.flush()?;
        if cfg.duration.is_some_and(|d| t >= d) || control.is_some_and(Control::stop_requested) {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    Ok(app.windows)
}

impl App {
    fn window(
        &self,
        size: (u16, u16),
        at: (i16, i16),
        override_redirect: bool,
    ) -> Result<Window, FakeAppError> {
        let id = self.conn.generate_id()?;
        let events = EventMask::KEY_PRESS
            | EventMask::KEY_RELEASE
            | EventMask::BUTTON_PRESS
            | EventMask::EXPOSURE
            | EventMask::STRUCTURE_NOTIFY;
        self.conn.create_window(
            COPY_DEPTH_FROM_PARENT,
            id,
            self.root,
            at.0,
            at.1,
            size.0,
            size.1,
            0,
            WindowClass::INPUT_OUTPUT,
            x11rb::COPY_FROM_PARENT,
            &CreateWindowAux::new()
                .background_pixel(0x00f0_f0f0)
                .event_mask(events)
                .override_redirect(u32::from(override_redirect)),
        )?;
        Ok(id)
    }

    fn atom(&self, name: &str) -> Result<u32, FakeAppError> {
        Ok(self.conn.intern_atom(false, name.as_bytes())?.reply()?.atom)
    }

    fn name(&self, window: Window, class: &[u8], title: &str) -> Result<(), FakeAppError> {
        let utf8 = self.atom("UTF8_STRING")?;
        let net_wm_name = self.atom("_NET_WM_NAME")?;
        self.conn.change_property8(
            PropMode::REPLACE,
            window,
            AtomEnum::WM_CLASS,
            AtomEnum::STRING,
            class,
        )?;
        self.conn.change_property8(
            PropMode::REPLACE,
            window,
            net_wm_name,
            utf8,
            title.as_bytes(),
        )?;
        Ok(())
    }

    fn toplevel(&self) -> Result<Window, FakeAppError> {
        let window = self.window(TOPLEVEL_SIZE, (0, 0), false)?;
        self.name(window, b"fake-app\0FakeApp\0", "Fake app: table")?;
        let kind = self.atom("_NET_WM_WINDOW_TYPE")?;
        let normal = self.atom("_NET_WM_WINDOW_TYPE_NORMAL")?;
        self.conn
            .change_property32(PropMode::REPLACE, window, kind, AtomEnum::ATOM, &[normal])?;
        self.conn.map_window(window)?;
        Ok(window)
    }

    fn popup(&self) -> Result<Window, FakeAppError> {
        let origin = self
            .conn
            .get_geometry(self.windows.toplevel)?
            .reply()
            .map_or((0, 0), |g| (g.x, g.y));
        let window = self.window(POPUP_SIZE, (origin.0 + 40, origin.1 + 60), true)?;
        self.name(window, b"fake-app\0FakeApp\0", "")?;
        let kind = self.atom("_NET_WM_WINDOW_TYPE")?;
        let menu = self.atom("_NET_WM_WINDOW_TYPE_POPUP_MENU")?;
        self.conn
            .change_property32(PropMode::REPLACE, window, kind, AtomEnum::ATOM, &[menu])?;
        self.conn.map_window(window)?;
        self.fill(
            window,
            Rectangle {
                x: 0,
                y: 0,
                width: POPUP_SIZE.0,
                height: POPUP_SIZE.1,
            },
            0x00ff_ffe0,
        )?;
        Ok(window)
    }

    fn dialog(&self) -> Result<Window, FakeAppError> {
        let window = self.window(DIALOG_SIZE, (0, 0), false)?;
        self.name(window, b"fake-app\0FakeApp\0", "Fake app: dialog")?;
        self.conn.change_property32(
            PropMode::REPLACE,
            window,
            AtomEnum::WM_TRANSIENT_FOR,
            AtomEnum::WINDOW,
            &[self.windows.toplevel],
        )?;
        let kind = self.atom("_NET_WM_WINDOW_TYPE")?;
        let dialog = self.atom("_NET_WM_WINDOW_TYPE_DIALOG")?;
        self.conn
            .change_property32(PropMode::REPLACE, window, kind, AtomEnum::ATOM, &[dialog])?;
        // Flags: decorations given (bit 1); decorations: none.
        let motif = self.atom("_MOTIF_WM_HINTS")?;
        self.conn
            .change_property32(PropMode::REPLACE, window, motif, motif, &[2, 0, 0, 0, 0])?;
        self.conn.map_window(window)?;
        Ok(window)
    }

    fn fill(&self, window: Window, rect: Rectangle, colour: u32) -> Result<(), FakeAppError> {
        self.conn.change_gc(
            self.gc,
            &x11rb::protocol::xproto::ChangeGCAux::new().foreground(colour),
        )?;
        self.conn.poly_fill_rectangle(window, self.gc, &[rect])?;
        Ok(())
    }

    /// Repaints the table: every row (the scroll), then the one refreshing cell.
    fn paint(&self) -> Result<(), FakeAppError> {
        let (width, height) = self.size;
        let rows = height.div_ceil(ROW);
        for row in 0..rows {
            let n = u32::from(row) + u32::from(self.offset);
            let colour = match n % 7 {
                0 => 0x0030_70c0,
                k if k % 2 == 0 => 0x00f0_f0f0,
                _ => 0x00d8_e4f0,
            };
            let y = i16::try_from(row * ROW).unwrap_or(i16::MAX);
            self.fill(
                self.windows.toplevel,
                Rectangle {
                    x: 0,
                    y,
                    width,
                    height: ROW,
                },
                colour,
            )?;
        }
        let cell = if self.tick % 4 < 2 {
            0x00c0_3030
        } else {
            0x0030_c030
        };
        self.fill(
            self.windows.toplevel,
            Rectangle {
                x: 160,
                y: 60,
                width: 80,
                height: ROW - 2,
            },
            cell,
        )?;
        Ok(())
    }
}
