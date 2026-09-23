//! The X observer: the application's window inventory and damage, seen from outside.
//!
//! It is one more X client on the display, next to the application and the streamer's window
//! manager. It selects `SubstructureNotify` on the root — any number of clients may, unlike
//! the redirect the window manager holds — so it sees every child of the root created, mapped,
//! moved, unmapped and destroyed, override-redirect popups included. For each mapped window it
//! reads what the spike's inventory asks for (roadmap M1): override-redirect,
//! `WM_TRANSIENT_FOR`, `_NET_WM_WINDOW_TYPE`, `_NET_WM_STATE`, `_MOTIF_WM_HINTS`, `WM_CLASS`,
//! `WM_WINDOW_ROLE` and the title, and it watches those properties for changes.
//!
//! It also creates its own Damage object (report level raw rectangles) on every mapped window,
//! so it logs each rectangle the application draws, whatever the streamer later sends. Damage
//! objects are per client, so this one does not disturb the streamer's.
//!
//! Output, in the run directory: `inventory.jsonl` (one line per event), `damage.jsonl` (one
//! line per rectangle), and `inventory.md` (the tables) at the end.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::fs;
use std::io::{self, BufWriter, Write as _};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use x11rb::connection::Connection as _;
use x11rb::protocol::Event;
use x11rb::protocol::damage::{self, ConnectionExt as _};
use x11rb::protocol::xproto::{
    AtomEnum, ChangeWindowAttributesAux, ConnectionExt as _, EventMask, MapState, Window,
};
use x11rb::rust_connection::RustConnection;

use crate::control::Control;
use crate::json::{JsonObject, string_array};
use crate::mem::wall_us;
use crate::stats::u64_to_f64;

/// What stops the observer.
#[derive(Debug)]
pub enum ObserveError {
    /// No X server to talk to.
    Connect(x11rb::errors::ConnectError),
    /// The connection failed.
    Connection(x11rb::errors::ConnectionError),
    /// A request the observer cannot do without failed.
    Reply(x11rb::errors::ReplyError),
    /// A log file could not be written.
    Io(io::Error),
}

impl std::fmt::Display for ObserveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Connect(e) => write!(f, "cannot connect to the X server: {e}"),
            Self::Connection(e) => write!(f, "the X connection failed: {e}"),
            Self::Reply(e) => write!(f, "an X request failed: {e}"),
            Self::Io(e) => write!(f, "cannot write the logs: {e}"),
        }
    }
}

impl std::error::Error for ObserveError {}

impl From<x11rb::errors::ConnectError> for ObserveError {
    fn from(e: x11rb::errors::ConnectError) -> Self {
        Self::Connect(e)
    }
}
impl From<x11rb::errors::ConnectionError> for ObserveError {
    fn from(e: x11rb::errors::ConnectionError) -> Self {
        Self::Connection(e)
    }
}
impl From<x11rb::errors::ReplyError> for ObserveError {
    fn from(e: x11rb::errors::ReplyError) -> Self {
        Self::Reply(e)
    }
}
impl From<x11rb::errors::ReplyOrIdError> for ObserveError {
    fn from(e: x11rb::errors::ReplyOrIdError) -> Self {
        match e {
            x11rb::errors::ReplyOrIdError::ConnectionError(e) => Self::Connection(e),
            x11rb::errors::ReplyOrIdError::X11Error(e) => {
                Self::Reply(x11rb::errors::ReplyError::X11Error(e))
            }
            x11rb::errors::ReplyOrIdError::IdsExhausted => {
                Self::Connection(x11rb::errors::ConnectionError::UnknownError)
            }
        }
    }
}
impl From<io::Error> for ObserveError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// How the observer runs.
#[derive(Debug, Clone)]
pub struct ObserveConfig {
    /// The run directory.
    pub out_dir: PathBuf,
    /// The display, or `None` for `$DISPLAY`.
    pub display: Option<String>,
    /// Stop after this long, when set; otherwise at the `stop` control file.
    pub duration: Option<Duration>,
}

/// One window of the inventory.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WindowRecord {
    /// Its X id.
    pub id: u32,
    /// Milliseconds from the start to its first map, when it was mapped.
    pub first_map_ms: Option<u64>,
    /// How many times it was mapped.
    pub maps: u32,
    /// Its override-redirect flag at the last map.
    pub override_redirect: bool,
    /// `WM_TRANSIENT_FOR`, when set.
    pub transient_for: Option<u32>,
    /// `_NET_WM_WINDOW_TYPE`, as atom names.
    pub types: Vec<String>,
    /// `_NET_WM_STATE`, as atom names.
    pub states: Vec<String>,
    /// `_MOTIF_WM_HINTS`: flags, functions, decorations, input mode, status.
    pub motif: Option<[u32; 5]>,
    /// `WM_CLASS`, as `instance.class`.
    pub class: String,
    /// `WM_WINDOW_ROLE`.
    pub role: String,
    /// `_NET_WM_NAME`, or `WM_NAME`.
    pub title: String,
    /// Position and size at the last map or configure.
    pub geometry: (i16, i16, u16, u16),
}

#[derive(Debug, Default)]
struct DamageAcc {
    events: u64,
    area: u64,
    full: u64,
}

#[derive(Debug, Clone, Copy)]
struct Atoms {
    net_wm_name: u32,
    net_wm_window_type: u32,
    net_wm_state: u32,
    motif_wm_hints: u32,
    wm_window_role: u32,
}

impl Atoms {
    fn intern(conn: &RustConnection) -> Result<Self, ObserveError> {
        let names: [&[u8]; 5] = [
            b"_NET_WM_NAME",
            b"_NET_WM_WINDOW_TYPE",
            b"_NET_WM_STATE",
            b"_MOTIF_WM_HINTS",
            b"WM_WINDOW_ROLE",
        ];
        let cookies = names
            .iter()
            .map(|n| conn.intern_atom(false, n))
            .collect::<Result<Vec<_>, _>>()?;
        let mut atoms = [0_u32; 5];
        for (slot, cookie) in atoms.iter_mut().zip(cookies) {
            *slot = cookie.reply()?.atom;
        }
        Ok(Self {
            net_wm_name: atoms[0],
            net_wm_window_type: atoms[1],
            net_wm_state: atoms[2],
            motif_wm_hints: atoms[3],
            wm_window_role: atoms[4],
        })
    }
}

struct Observer {
    conn: RustConnection,
    root: Window,
    atoms: Atoms,
    atom_names: HashMap<u32, String>,
    windows: BTreeMap<u32, WindowRecord>,
    damaged: HashMap<u32, u32>,
    damage_by: BTreeMap<(String, u32), DamageAcc>,
    labels: Vec<(String, u64)>,
    started: Instant,
    inventory: BufWriter<fs::File>,
    damage_log: BufWriter<fs::File>,
    x_errors: u64,
}

/// Runs the observer until stopped, then writes `inventory.md`. Returns the windows it saw.
pub fn run(cfg: &ObserveConfig, control: &Control) -> Result<Vec<WindowRecord>, ObserveError> {
    let (conn, screen) = x11rb::connect(cfg.display.as_deref())?;
    let setup_root = &conn.setup().roots[screen];
    let root = setup_root.root;
    let root_size = (setup_root.width_in_pixels, setup_root.height_in_pixels);
    conn.damage_query_version(1, 1)?.reply()?;
    let atoms = Atoms::intern(&conn)?;
    conn.change_window_attributes(
        root,
        &ChangeWindowAttributesAux::new().event_mask(EventMask::SUBSTRUCTURE_NOTIFY),
    )?
    .check()?;

    let mut obs = Observer {
        conn,
        root,
        atoms,
        atom_names: HashMap::new(),
        windows: BTreeMap::new(),
        damaged: HashMap::new(),
        damage_by: BTreeMap::new(),
        labels: vec![(control.mark(), 0)],
        started: Instant::now(),
        inventory: BufWriter::new(fs::File::create(cfg.out_dir.join("inventory.jsonl"))?),
        damage_log: BufWriter::new(fs::File::create(cfg.out_dir.join("damage.jsonl"))?),
        x_errors: 0,
    };
    obs.log(
        JsonObject::new()
            .str("ev", "start")
            .uint("root_width", u64::from(root_size.0))
            .uint("root_height", u64::from(root_size.1)),
    )?;
    obs.scan_existing()?;

    let mut last_control = Instant::now();
    loop {
        let mut handled = 0_u32;
        while let Some(event) = obs.conn.poll_for_event()? {
            obs.handle(&event)?;
            handled += 1;
            if handled >= 4096 {
                break;
            }
        }
        if last_control.elapsed() >= Duration::from_millis(100) {
            last_control = Instant::now();
            let label = control.mark();
            if obs.labels.last().is_none_or(|(l, _)| *l != label) {
                let at = obs.ms();
                obs.log(JsonObject::new().str("ev", "mark").str("label", &label))?;
                obs.labels.push((label, at));
            }
            obs.inventory.flush()?;
            obs.damage_log.flush()?;
            if control.stop_requested() || cfg.duration.is_some_and(|d| obs.started.elapsed() >= d)
            {
                break;
            }
        }
        if handled == 0 {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    obs.log(JsonObject::new().str("ev", "stop"))?;
    obs.inventory.flush()?;
    obs.damage_log.flush()?;
    let summary = obs.summary();
    fs::write(cfg.out_dir.join("inventory.md"), summary)?;
    Ok(obs.windows.into_values().collect())
}

impl Observer {
    fn ms(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    fn label(&self) -> &str {
        self.labels.last().map_or("start", |(l, _)| l.as_str())
    }

    fn log(&mut self, line: JsonObject) -> io::Result<()> {
        let line = line
            .uint("t_us", wall_us())
            .uint("t_ms", self.ms())
            .str("label", self.label())
            .finish();
        writeln!(self.inventory, "{line}")
    }

    /// Records the windows already mapped when the observer starts.
    fn scan_existing(&mut self) -> Result<(), ObserveError> {
        let children = self.conn.query_tree(self.root)?.reply()?.children;
        for window in children {
            let Ok(attrs) = self.conn.get_window_attributes(window)?.reply() else {
                continue;
            };
            if attrs.map_state == MapState::VIEWABLE {
                self.mapped(window, attrs.override_redirect, "existing")?;
            }
        }
        Ok(())
    }

    fn handle(&mut self, event: &Event) -> Result<(), ObserveError> {
        match event {
            Event::CreateNotify(e) if e.parent == self.root => {
                self.log(
                    JsonObject::new()
                        .str("ev", "create")
                        .uint("window", u64::from(e.window))
                        .bool("override_redirect", e.override_redirect)
                        .int("x", i64::from(e.x))
                        .int("y", i64::from(e.y))
                        .uint("width", u64::from(e.width))
                        .uint("height", u64::from(e.height)),
                )?;
            }
            Event::MapNotify(e) if e.event == self.root => {
                self.mapped(e.window, e.override_redirect, "map")?;
            }
            Event::UnmapNotify(e) if e.event == self.root => {
                self.log(
                    JsonObject::new()
                        .str("ev", "unmap")
                        .uint("window", u64::from(e.window)),
                )?;
            }
            Event::DestroyNotify(e) if e.event == self.root => {
                self.damaged.remove(&e.window);
                self.log(
                    JsonObject::new()
                        .str("ev", "destroy")
                        .uint("window", u64::from(e.window)),
                )?;
            }
            Event::ConfigureNotify(e) if e.event == self.root => {
                let geometry = (e.x, e.y, e.width, e.height);
                if let Some(w) = self.windows.get_mut(&e.window)
                    && w.geometry != geometry
                {
                    w.geometry = geometry;
                    self.log(
                        JsonObject::new()
                            .str("ev", "configure")
                            .uint("window", u64::from(e.window))
                            .int("x", i64::from(e.x))
                            .int("y", i64::from(e.y))
                            .uint("width", u64::from(e.width))
                            .uint("height", u64::from(e.height)),
                    )?;
                }
            }
            Event::PropertyNotify(e) if self.windows.contains_key(&e.window) => {
                self.property_changed(e.window, e.atom)?;
            }
            Event::DamageNotify(e) => self.damage(e)?,
            Event::Error(_) => self.x_errors += 1,
            _ => {}
        }
        Ok(())
    }

    fn mapped(
        &mut self,
        window: Window,
        override_redirect: bool,
        ev: &str,
    ) -> Result<(), ObserveError> {
        let at = self.ms();
        let mut record = self.read_window(window, override_redirect);
        let previous = self.windows.get(&window);
        record.maps = previous.map_or(0, |p| p.maps) + 1;
        record.first_map_ms = previous.and_then(|p| p.first_map_ms).or(Some(at));
        if !self.damaged.contains_key(&window) {
            let damage = self.conn.generate_id()?;
            // A window destroyed meanwhile makes this fail; the error arrives as an event.
            self.conn
                .damage_create(damage, window, damage::ReportLevel::RAW_RECTANGLES)?;
            self.conn.change_window_attributes(
                window,
                &ChangeWindowAttributesAux::new().event_mask(EventMask::PROPERTY_CHANGE),
            )?;
            self.damaged.insert(window, damage);
        }
        self.conn.flush()?;
        let line = window_json(ev, &record);
        self.windows.insert(window, record);
        self.log(line)?;
        Ok(())
    }

    fn property_changed(&mut self, window: Window, atom: u32) -> Result<(), ObserveError> {
        let a = self.atoms;
        let watched = [
            u32::from(AtomEnum::WM_TRANSIENT_FOR),
            u32::from(AtomEnum::WM_NAME),
            u32::from(AtomEnum::WM_CLASS),
            a.net_wm_name,
            a.net_wm_window_type,
            a.net_wm_state,
            a.motif_wm_hints,
            a.wm_window_role,
        ];
        if !watched.contains(&atom) {
            return Ok(());
        }
        let or = self
            .windows
            .get(&window)
            .is_some_and(|w| w.override_redirect);
        let mut record = self.read_window(window, or);
        if let Some(old) = self.windows.get(&window) {
            record.maps = old.maps;
            record.first_map_ms = old.first_map_ms;
            if old == &record {
                return Ok(());
            }
        }
        let name = self.atom_name(atom);
        let line = window_json("property", &record).str("property", &name);
        self.windows.insert(window, record);
        self.log(line)?;
        Ok(())
    }

    fn damage(&mut self, e: &damage::NotifyEvent) -> Result<(), ObserveError> {
        let area = u64::from(e.area.width) * u64::from(e.area.height);
        let whole = u64::from(e.geometry.width) * u64::from(e.geometry.height);
        let label = self.label().to_owned();
        let acc = self
            .damage_by
            .entry((label.clone(), e.drawable))
            .or_default();
        acc.events += 1;
        acc.area += area;
        acc.full += u64::from(area >= whole && whole > 0);
        let line = JsonObject::new()
            .uint("t_us", wall_us())
            .uint("t_ms", self.ms())
            .str("label", &label)
            .uint("window", u64::from(e.drawable))
            .int("x", i64::from(e.area.x))
            .int("y", i64::from(e.area.y))
            .uint("width", u64::from(e.area.width))
            .uint("height", u64::from(e.area.height))
            .uint("window_width", u64::from(e.geometry.width))
            .uint("window_height", u64::from(e.geometry.height))
            .finish();
        writeln!(self.damage_log, "{line}")?;
        Ok(())
    }

    fn atom_name(&mut self, atom: u32) -> String {
        if let Some(name) = self.atom_names.get(&atom) {
            return name.clone();
        }
        let name = self
            .conn
            .get_atom_name(atom)
            .ok()
            .and_then(|c| c.reply().ok())
            .map_or_else(
                || format!("atom-{atom}"),
                |r| String::from_utf8_lossy(&r.name).into_owned(),
            );
        self.atom_names.insert(atom, name.clone());
        name
    }

    /// Reads every property the inventory records. A window that is gone reads as empty.
    fn read_window(&mut self, window: Window, override_redirect: bool) -> WindowRecord {
        let a = self.atoms;
        let geometry = self
            .conn
            .get_geometry(window)
            .ok()
            .and_then(|c| c.reply().ok())
            .map_or((0, 0, 0, 0), |g| (g.x, g.y, g.width, g.height));
        let transient = self
            .prop32(window, u32::from(AtomEnum::WM_TRANSIENT_FOR))
            .and_then(|v| v.first().copied());
        let types = self
            .prop32(window, a.net_wm_window_type)
            .unwrap_or_default()
            .into_iter()
            .map(|t| self.atom_name(t))
            .collect();
        let states = self
            .prop32(window, a.net_wm_state)
            .unwrap_or_default()
            .into_iter()
            .map(|t| self.atom_name(t))
            .collect();
        let motif = self.prop32(window, a.motif_wm_hints).and_then(|v| {
            let mut out = [0_u32; 5];
            for (slot, value) in out.iter_mut().zip(v.iter()) {
                *slot = *value;
            }
            (!v.is_empty()).then_some(out)
        });
        let class = self
            .prop8(window, u32::from(AtomEnum::WM_CLASS))
            .map(|v| {
                v.split(|&b| b == 0)
                    .filter(|p| !p.is_empty())
                    .map(|p| String::from_utf8_lossy(p).into_owned())
                    .collect::<Vec<_>>()
                    .join(".")
            })
            .unwrap_or_default();
        let role = self
            .prop8(window, a.wm_window_role)
            .map(|v| String::from_utf8_lossy(&v).into_owned())
            .unwrap_or_default();
        let title = self
            .prop8(window, a.net_wm_name)
            .or_else(|| self.prop8(window, u32::from(AtomEnum::WM_NAME)))
            .map(|v| String::from_utf8_lossy(&v).into_owned())
            .unwrap_or_default();
        WindowRecord {
            id: window,
            first_map_ms: None,
            maps: 0,
            override_redirect,
            transient_for: transient,
            types,
            states,
            motif,
            class,
            role,
            title,
            geometry,
        }
    }

    fn prop_raw(&self, window: Window, atom: u32) -> Option<(u8, Vec<u8>)> {
        let reply = self
            .conn
            .get_property(false, window, atom, AtomEnum::ANY, 0, 1024)
            .ok()?
            .reply()
            .ok()?;
        (reply.format != 0).then_some((reply.format, reply.value))
    }

    fn prop8(&self, window: Window, atom: u32) -> Option<Vec<u8>> {
        self.prop_raw(window, atom)
            .and_then(|(format, value)| (format == 8).then_some(value))
    }

    fn prop32(&self, window: Window, atom: u32) -> Option<Vec<u32>> {
        self.prop_raw(window, atom).and_then(|(format, value)| {
            (format == 32).then(|| {
                value
                    .chunks_exact(4)
                    .map(|c| u32::from_ne_bytes([c[0], c[1], c[2], c[3]]))
                    .collect()
            })
        })
    }

    fn summary(&self) -> String {
        let mut out = String::from(
            "## Window inventory\n\n\
             Every child of the root the application mapped, as the observer saw it. X ids are \
             hex; `OR` is override-redirect.\n\n\
             | Window | First map (s) | Maps | OR | Transient for | Type | State | Motif (flags/functions/decorations) | Class | Role | Title | Size |\n\
             |---|---:|---:|---|---|---|---|---|---|---|---|---|\n",
        );
        for w in self.windows.values() {
            let motif = w.motif.map_or_else(String::new, |m| {
                format!("{:#x}/{:#x}/{:#x}", m[0], m[1], m[2])
            });
            let _ = writeln!(
                out,
                "| {:#x} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {}x{} |",
                w.id,
                w.first_map_ms
                    .map_or_else(String::new, |ms| format!("{:.2}", u64_to_f64(ms) / 1000.0)),
                w.maps,
                if w.override_redirect { "yes" } else { "no" },
                w.transient_for
                    .map_or_else(String::new, |t| format!("{t:#x}")),
                short_types(&w.types),
                short_types(&w.states),
                motif,
                cell(&w.class),
                cell(&w.role),
                cell(&w.title),
                w.geometry.2,
                w.geometry.3,
            );
        }
        self.damage_summary(&mut out);
        let _ = writeln!(out, "\nX errors seen by the observer: {}.", self.x_errors);
        out
    }

    fn damage_summary(&self, out: &mut String) {
        out.push_str(
            "\n## Damage\n\n\
             Every rectangle the application drew, per scenario (report level raw rectangles). \
             A full-window event covers the whole window.\n\n\
             | Scenario | Duration (s) | Events | Events/s | Area (Mpx) | Mean px/event | Full-window events | Windows |\n\
             |---|---:|---:|---:|---:|---:|---:|---:|\n",
        );
        let end = self.ms();
        for (i, (label, start)) in self.labels.iter().enumerate() {
            let stop = self.labels.get(i + 1).map_or(end, |(_, s)| *s);
            let secs = u64_to_f64(stop.saturating_sub(*start)) / 1000.0;
            let (mut events, mut area, mut full, mut windows) = (0_u64, 0_u64, 0_u64, 0_usize);
            for ((l, _), acc) in &self.damage_by {
                if l == label {
                    events += acc.events;
                    area += acc.area;
                    full += acc.full;
                    windows += 1;
                }
            }
            let rate = if secs > 0.0 {
                u64_to_f64(events) / secs
            } else {
                0.0
            };
            let mean = if events > 0 {
                u64_to_f64(area) / u64_to_f64(events)
            } else {
                0.0
            };
            let _ = writeln!(
                out,
                "| {label} | {secs:.1} | {events} | {rate:.1} | {:.2} | {mean:.0} | {full} | {windows} |",
                u64_to_f64(area) / 1e6
            );
        }
    }
}

fn window_json(ev: &str, w: &WindowRecord) -> JsonObject {
    let motif = w.motif.map_or_else(
        || "null".to_owned(),
        |m| crate::json::array(m.iter().map(u32::to_string)),
    );
    JsonObject::new()
        .str("ev", ev)
        .uint("window", u64::from(w.id))
        .bool("override_redirect", w.override_redirect)
        .opt_uint("transient_for", w.transient_for.map(u64::from))
        .raw("types", &string_array(&w.types))
        .raw("states", &string_array(&w.states))
        .raw("motif", &motif)
        .str("class", &w.class)
        .str("role", &w.role)
        .str("title", &w.title)
        .int("x", i64::from(w.geometry.0))
        .int("y", i64::from(w.geometry.1))
        .uint("width", u64::from(w.geometry.2))
        .uint("height", u64::from(w.geometry.3))
}

/// `_NET_WM_WINDOW_TYPE_DIALOG` → `DIALOG`: the tables stay readable.
fn short_types(names: &[String]) -> String {
    names
        .iter()
        .map(|n| {
            n.strip_prefix("_NET_WM_WINDOW_TYPE_")
                .or_else(|| n.strip_prefix("_NET_WM_STATE_"))
                .or_else(|| n.strip_prefix("_KDE_NET_WM_WINDOW_TYPE_"))
                .unwrap_or(n)
                .to_owned()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// A table cell: pipes escaped, cut to 48 characters.
fn cell(text: &str) -> String {
    let cut: String = text.chars().take(48).collect();
    cut.replace('|', "\\|")
}

#[cfg(test)]
mod tests {
    use super::{cell, short_types};

    #[test]
    fn type_names_lose_their_prefix() {
        let names = [
            "_NET_WM_WINDOW_TYPE_DIALOG".to_owned(),
            "_NET_WM_STATE_MODAL".to_owned(),
            "_KDE_NET_WM_WINDOW_TYPE_OVERRIDE".to_owned(),
            "CUSTOM".to_owned(),
        ];
        assert_eq!(short_types(&names), "DIALOG MODAL OVERRIDE CUSTOM");
    }

    #[test]
    fn cells_escape_pipes_and_stay_short() {
        assert_eq!(cell("a|b"), "a\\|b");
        assert_eq!(cell(&"x".repeat(60)).len(), 48);
    }
}
