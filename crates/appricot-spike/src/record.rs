//! The wire recorder: a host that records instead of drawing.
//!
//! It opens a session on the streamer as a browser client would — the same `Hello`, the same
//! acks — and keeps, for every surface, a canvas it paints the decoded tiles into. It honours
//! `ResizeAsk` by default, as the demo host does, so the application's windows reach the size
//! they ask for. Everything that arrives is logged with the scenario label in force, and:
//!
//! - `snapshots/<label>-s<surface>.png`: one PNG per live surface, when a `snap-<label>`
//!   control file or a script `snap` asks, periodically with `--snap-every`, and once more at
//!   the end (`final`). The canvas is exactly what a host would show, so a snapshot is the
//!   visual proof the spike needs for each environment knob;
//! - `tiles.bin` (with `--tiles`): every tile decoded, for the codec bench ([`crate::tiles`]);
//! - `wire.jsonl` and, at the end, `wire.md`: frames, tiles, bytes and codecs per scenario.
//!
//! An input script ([`crate::script`]) can drive the application meanwhile.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::io::{self, BufWriter, Write as _};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use appricot_core::Size;
use appricot_encode::decode_tile;
use appricot_proto::wire::{
    Body, Bye, ByeReason, Configure, Envelope, FocusNotify, FrameAck, Hello, Key, PointerAxis,
    PointerButton, PointerMove, Role, decode_envelope, encode_envelope,
};
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;

use crate::control::Control;
use crate::json::JsonObject;
use crate::mem::wall_us;
use crate::script::{RoleKind, Script, Step};
use crate::stats::{kib, u64_to_f64};
use crate::tiles::{TileRecord, TileWriter};

/// How long the recorder keeps trying to reach a streamer that is still starting.
pub const CONNECT_PATIENCE: Duration = Duration::from_secs(30);

/// How the recorder runs.
#[derive(Debug, Clone)]
pub struct RecordConfig {
    /// The run directory.
    pub out_dir: PathBuf,
    /// The streamer's session URL, `ws://127.0.0.1:<port>/session`.
    pub url: String,
    /// The stream token.
    pub token: Vec<u8>,
    /// The codecs offered beyond RAW (wire ids).
    pub codecs: Vec<u32>,
    /// Whether to write `tiles.bin`.
    pub save_tiles: bool,
    /// The input script, if any.
    pub script: Option<Script>,
    /// Snapshot every live surface this often, when set.
    pub snapshot_every: Option<Duration>,
    /// Stop after this long, when set.
    pub duration: Option<Duration>,
    /// Answer `ResizeAsk` with a `Configure` of the asked size.
    pub honour_resize_ask: bool,
}

/// What stops the recorder.
#[derive(Debug)]
pub enum RecordError {
    /// The streamer could not be reached.
    Connect(String),
    /// The session did not open.
    Handshake(String),
    /// The socket failed. Boxed: the socket error is large, and this is every function's `Err`.
    Socket(Box<tokio_tungstenite::tungstenite::Error>),
    /// A message could not be encoded.
    Encode(appricot_proto::wire::EncodeError),
    /// A log, snapshot or tile file could not be written.
    Io(io::Error),
    /// The script failed.
    Script {
        /// Its line.
        line: usize,
        /// Why.
        message: String,
    },
}

impl std::fmt::Display for RecordError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Connect(e) => write!(f, "cannot reach the streamer: {e}"),
            Self::Handshake(e) => write!(f, "the session did not open: {e}"),
            Self::Socket(e) => write!(f, "the socket failed: {e}"),
            Self::Encode(e) => write!(f, "a message did not encode: {e:?}"),
            Self::Io(e) => write!(f, "cannot write the recording: {e}"),
            Self::Script { line, message } => write!(f, "script line {line}: {message}"),
        }
    }
}

impl std::error::Error for RecordError {}

impl From<io::Error> for RecordError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<tokio_tungstenite::tungstenite::Error> for RecordError {
    fn from(e: tokio_tungstenite::tungstenite::Error) -> Self {
        Self::Socket(Box::new(e))
    }
}

/// What the recorder saw, in short.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecordSummary {
    /// Surfaces announced, by role.
    pub toplevels: usize,
    /// Popups announced.
    pub popups: usize,
    /// Frames received.
    pub frames: u64,
    /// Tiles received.
    pub tiles: u64,
    /// Payload bytes received in tiles.
    pub wire_bytes: u64,
    /// The snapshot files written.
    pub snapshots: Vec<PathBuf>,
    /// The Bye the server sent, if it sent one.
    pub bye: Option<String>,
}

struct Surface {
    role: RoleKind,
    parent: Option<u32>,
    title: String,
    size: (u32, u32),
    canvas: Vec<u8>,
    live: bool,
}

impl Surface {
    fn resize(&mut self, width: u32, height: u32) {
        if (width, height) == self.size {
            return;
        }
        let mut canvas = vec![0_u8; px(width) * px(height) * 4];
        let (keep_w, keep_h) = (px(width.min(self.size.0)), px(height.min(self.size.1)));
        for row in 0..keep_h {
            let from = row * px(self.size.0) * 4;
            let to = row * px(width) * 4;
            canvas[to..to + keep_w * 4].copy_from_slice(&self.canvas[from..from + keep_w * 4]);
        }
        self.canvas = canvas;
        self.size = (width, height);
    }

    /// Paints `pixels` (BGRX, `w` by `h`) at `(x, y)`, clipped to the canvas.
    fn blit(&mut self, x: i32, y: i32, w: u32, h: u32, pixels: &[u8]) {
        let (cw, ch) = (i64::from(self.size.0), i64::from(self.size.1));
        for row in 0..i64::from(h) {
            let ty = i64::from(y) + row;
            if !(0..ch).contains(&ty) {
                continue;
            }
            let x0 = i64::from(x).max(0);
            let x1 = (i64::from(x) + i64::from(w)).min(cw);
            if x0 >= x1 {
                continue;
            }
            let src = usize::try_from(row * i64::from(w) + (x0 - i64::from(x))).unwrap_or(0) * 4;
            let dst = usize::try_from(ty * cw + x0).unwrap_or(0) * 4;
            let len = usize::try_from(x1 - x0).unwrap_or(0) * 4;
            self.canvas[dst..dst + len].copy_from_slice(&pixels[src..src + len]);
        }
    }
}

fn px(v: u32) -> usize {
    usize::try_from(v).unwrap_or(0)
}

#[derive(Default)]
struct LabelStats {
    frames: u64,
    full_redraws: u64,
    tiles: u64,
    wire_bytes: u64,
    raw_bytes: u64,
    raw_tiles: u64,
    qoi_tiles: u64,
    decode_us: f64,
}

struct Recorder {
    control: Control,
    out_dir: PathBuf,
    label: String,
    labels: Vec<String>,
    stats: BTreeMap<String, LabelStats>,
    surfaces: BTreeMap<u32, Surface>,
    order: Vec<u32>,
    popups_announced: usize,
    target: Option<u32>,
    next_serial: u32,
    honour_resize_ask: bool,
    log: BufWriter<fs::File>,
    tiles: Option<TileWriter>,
    started: Instant,
    summary: RecordSummary,
    stop: bool,
}

/// Runs a recording session until stopped, then writes `wire.md`.
pub async fn run(cfg: RecordConfig, control: Control) -> Result<RecordSummary, RecordError> {
    fs::create_dir_all(cfg.out_dir.join("snapshots"))?;
    let mut rec = Recorder {
        label: control.mark(),
        labels: Vec::new(),
        control,
        out_dir: cfg.out_dir.clone(),
        stats: BTreeMap::new(),
        surfaces: BTreeMap::new(),
        order: Vec::new(),
        popups_announced: 0,
        target: None,
        next_serial: 1,
        honour_resize_ask: cfg.honour_resize_ask,
        log: BufWriter::new(fs::File::create(cfg.out_dir.join("wire.jsonl"))?),
        tiles: if cfg.save_tiles {
            Some(TileWriter::create(&cfg.out_dir.join("tiles.bin"))?)
        } else {
            None
        },
        started: Instant::now(),
        summary: RecordSummary::default(),
        stop: false,
    };
    rec.labels.push(rec.label.clone());

    let ws = connect(&cfg.url).await?;
    let (mut sink, mut stream) = ws.split();
    let hello = Body::Hello(Hello {
        protocol_version: u32::from(appricot_proto::PROTOCOL_VERSION),
        client_name: "appricot-spike".to_owned(),
        stream_token: cfg.token.clone(),
        codecs: cfg.codecs.clone(),
        resume_serial: None,
    });
    sink.send(message(hello)?).await?;
    rec.handshake(&mut stream).await?;

    let mut runner = cfg.script.map(Runner::new);
    let mut ticker = tokio::time::interval(Duration::from_millis(20));
    let mut last_control = Instant::now();
    let mut last_snapshot = Instant::now();
    while !rec.stop {
        let mut replies = Vec::new();
        tokio::select! {
            next = stream.next() => match next {
                Some(Ok(Message::Binary(bytes))) => replies = rec.receive(&bytes)?,
                Some(Ok(Message::Close(_))) | None => rec.ended("the socket closed")?,
                Some(Err(e)) => rec.ended(&format!("the socket failed: {e}"))?,
                Some(Ok(_)) => {}
            },
            _ = ticker.tick() => {}
        }
        if let Some(runner) = runner.as_mut() {
            replies.extend(runner.advance(&mut rec)?);
        }
        for body in replies {
            sink.send(message(body)?).await?;
        }
        if last_control.elapsed() >= Duration::from_millis(100) {
            last_control = Instant::now();
            rec.poll_control()?;
            if cfg.duration.is_some_and(|d| rec.started.elapsed() >= d) {
                rec.stop = true;
            }
            if let Some(every) = cfg.snapshot_every
                && last_snapshot.elapsed() >= every
            {
                last_snapshot = Instant::now();
                let label = format!("{}-t{}", rec.label, rec.started.elapsed().as_secs());
                rec.snapshot(&label)?;
            }
        }
    }
    if rec.summary.bye.is_none() {
        let bye = Body::Bye(Bye {
            reason: ByeReason::ByePeerClosed.into(),
            text: "spike recording over".to_owned(),
        });
        let _ = sink.send(message(bye)?).await;
        let _ = sink.close().await;
    }
    rec.finish()
}

async fn connect(
    url: &str,
) -> Result<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    RecordError,
> {
    let deadline = Instant::now() + CONNECT_PATIENCE;
    loop {
        match tokio_tungstenite::connect_async(url).await {
            Ok((ws, _)) => return Ok(ws),
            Err(_) if Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            Err(e) => return Err(RecordError::Connect(e.to_string())),
        }
    }
}

fn message(body: Body) -> Result<Message, RecordError> {
    let bytes = encode_envelope(&Envelope { body: Some(body) }).map_err(RecordError::Encode)?;
    Ok(Message::Binary(bytes.into()))
}

impl Recorder {
    fn ms(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    fn log(&mut self, line: JsonObject) -> io::Result<()> {
        let line = line
            .uint("t_us", wall_us())
            .uint("t_ms", self.ms())
            .str("label", &self.label)
            .finish();
        writeln!(self.log, "{line}")
    }

    fn set_label(&mut self, label: String) -> io::Result<()> {
        if label != self.label {
            self.label = label;
            if !self.labels.contains(&self.label) {
                self.labels.push(self.label.clone());
            }
            self.log(JsonObject::new().str("ev", "mark"))?;
        }
        Ok(())
    }

    async fn handshake<S>(&mut self, stream: &mut S) -> Result<(), RecordError>
    where
        S: futures_util::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>>
            + Unpin,
    {
        let first = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                match stream.next().await {
                    Some(Ok(Message::Binary(bytes))) => return Ok(bytes),
                    Some(Ok(_)) => {}
                    Some(Err(e)) => return Err(e.into()),
                    None => return Err(RecordError::Handshake("the socket closed".into())),
                }
            }
        })
        .await
        .map_err(|_| RecordError::Handshake("no HelloReply within 10 s".into()))??;
        match decode_envelope(&first).ok().and_then(|e| e.body) {
            Some(Body::HelloReply(reply)) => {
                self.log(
                    JsonObject::new()
                        .str("ev", "hello_reply")
                        .str("session_id", &reply.session_id)
                        .uint("max_frame_credits", u64::from(reply.max_frame_credits))
                        .raw(
                            "codecs",
                            &crate::json::array(reply.codecs.iter().map(u32::to_string)),
                        ),
                )?;
                Ok(())
            }
            Some(Body::Bye(bye)) => Err(RecordError::Handshake(format!(
                "refused with Bye {}: {}",
                bye.reason, bye.text
            ))),
            _ => Err(RecordError::Handshake(
                "the first message was not a HelloReply".into(),
            )),
        }
    }

    fn ended(&mut self, why: &str) -> io::Result<()> {
        self.log(JsonObject::new().str("ev", "closed").str("why", why))?;
        self.stop = true;
        Ok(())
    }

    fn poll_control(&mut self) -> Result<(), RecordError> {
        let label = self.control.mark();
        self.set_label(label)?;
        for label in self.control.take_snap_requests() {
            self.snapshot(&label)?;
        }
        if self.control.stop_requested() {
            self.stop = true;
        }
        self.log.flush()?;
        if let Some(tiles) = self.tiles.as_mut() {
            tiles.flush()?;
        }
        Ok(())
    }

    /// Handles one message from the streamer; returns what to send back.
    fn receive(&mut self, bytes: &[u8]) -> Result<Vec<Body>, RecordError> {
        let Ok(Envelope { body: Some(body) }) = decode_envelope(bytes) else {
            self.log(JsonObject::new().str("ev", "undecodable"))?;
            return Ok(Vec::new());
        };
        let mut replies = Vec::new();
        match body {
            Body::SurfaceNew(m) => self.surface_new(&m)?,
            Body::SurfaceGone(m) => {
                if let Some(s) = self.surfaces.get_mut(&m.surface_id) {
                    s.live = false;
                }
                self.log(
                    JsonObject::new()
                        .str("ev", "surface_gone")
                        .uint("surface", u64::from(m.surface_id))
                        .int("reason", i64::from(m.reason)),
                )?;
            }
            Body::SurfaceMetadata(m) => {
                if let (Some(s), Some(title)) = (self.surfaces.get_mut(&m.surface_id), &m.title) {
                    title.clone_into(&mut s.title);
                }
                self.log(
                    JsonObject::new()
                        .str("ev", "surface_metadata")
                        .uint("surface", u64::from(m.surface_id))
                        .opt_str("title", m.title.as_deref())
                        .opt_str("app_id", m.app_id.as_deref()),
                )?;
            }
            Body::ResizeAsk(m) => {
                let size = m.size.unwrap_or_default();
                self.log(
                    JsonObject::new()
                        .str("ev", "resize_ask")
                        .uint("surface", u64::from(m.surface_id))
                        .uint("width", u64::from(size.width))
                        .uint("height", u64::from(size.height)),
                )?;
                if self.honour_resize_ask {
                    let serial = self.next_serial;
                    self.next_serial += 1;
                    replies.push(Body::Configure(Configure {
                        surface_id: m.surface_id,
                        serial,
                        size: Some(size),
                    }));
                }
            }
            Body::ConfigureAck(m) => {
                let size = m.size.unwrap_or_default();
                if let Some(s) = self.surfaces.get_mut(&m.surface_id) {
                    s.resize(size.width, size.height);
                }
                self.log(
                    JsonObject::new()
                        .str("ev", "configure_ack")
                        .uint("surface", u64::from(m.surface_id))
                        .uint("serial", u64::from(m.serial))
                        .uint("width", u64::from(size.width))
                        .uint("height", u64::from(size.height)),
                )?;
            }
            Body::Frame(frame) => replies.push(self.frame(&frame)?),
            other => self.other(&other)?,
        }
        Ok(replies)
    }

    fn other(&mut self, body: &Body) -> Result<(), RecordError> {
        match body {
            Body::FocusAsk(m) => self.log(
                JsonObject::new()
                    .str("ev", "focus_ask")
                    .uint("surface", u64::from(m.surface_id)),
            )?,
            Body::CursorImage(m) => self.log(
                JsonObject::new()
                    .str("ev", "cursor_image")
                    .uint("serial", u64::from(m.serial))
                    .uint("width", u64::from(m.width))
                    .uint("height", u64::from(m.height)),
            )?,
            Body::CursorGone(_) => self.log(JsonObject::new().str("ev", "cursor_gone"))?,
            Body::ClipboardAsk(_) => self.log(JsonObject::new().str("ev", "clipboard_ask"))?,
            Body::ServerError(e) => self.log(
                JsonObject::new()
                    .str("ev", "server_error")
                    .uint("code", u64::from(e.code))
                    .str("text", &e.text),
            )?,
            Body::Bye(bye) => {
                self.summary.bye = Some(format!("{}: {}", bye.reason, bye.text));
                self.log(
                    JsonObject::new()
                        .str("ev", "bye")
                        .int("reason", i64::from(bye.reason))
                        .str("text", &bye.text),
                )?;
                self.stop = true;
            }
            _ => self.log(JsonObject::new().str("ev", "unexpected"))?,
        }
        Ok(())
    }

    fn surface_new(&mut self, m: &appricot_proto::wire::SurfaceNew) -> Result<(), RecordError> {
        let role = if m.role == i32::from(Role::Popup) {
            self.popups_announced += 1;
            self.summary.popups += 1;
            RoleKind::Popup
        } else {
            self.summary.toplevels += 1;
            RoleKind::Toplevel
        };
        let size = m.size.unwrap_or_default();
        let mut surface = Surface {
            role,
            parent: m.parent_id,
            title: m.title.clone(),
            size: (0, 0),
            canvas: Vec::new(),
            live: true,
        };
        surface.resize(size.width, size.height);
        if !self.surfaces.contains_key(&m.surface_id) {
            self.order.push(m.surface_id);
        }
        self.surfaces.insert(m.surface_id, surface);
        let positioner = m.positioner.as_ref().map(|p| {
            let rect = p.anchor_rect.unwrap_or_default();
            let offset = p.offset.unwrap_or_default();
            JsonObject::new()
                .int("anchor_x", i64::from(rect.x))
                .int("anchor_y", i64::from(rect.y))
                .uint("anchor_width", u64::from(rect.width))
                .uint("anchor_height", u64::from(rect.height))
                .int("anchor", i64::from(p.anchor))
                .int("gravity", i64::from(p.gravity))
                .int("offset_x", i64::from(offset.x))
                .int("offset_y", i64::from(offset.y))
                .finish()
        });
        self.log(
            JsonObject::new()
                .str("ev", "surface_new")
                .uint("surface", u64::from(m.surface_id))
                .str(
                    "role",
                    if role == RoleKind::Popup {
                        "popup"
                    } else {
                        "toplevel"
                    },
                )
                .opt_uint("parent", m.parent_id.map(u64::from))
                .uint("width", u64::from(size.width))
                .uint("height", u64::from(size.height))
                .str("title", &m.title)
                .str("app_id", &m.app_id)
                .uint("scale_120ths", u64::from(m.scale_120ths))
                .raw("positioner", positioner.as_deref().unwrap_or("null")),
        )?;
        Ok(())
    }

    fn frame(&mut self, frame: &appricot_proto::wire::Frame) -> Result<Body, RecordError> {
        let started = Instant::now();
        let (mut wire, mut raw, mut raw_tiles, mut qoi_tiles, mut bad) = (0_u64, 0, 0, 0, 0_u64);
        for tile in &frame.tiles {
            let rect = tile.rect.unwrap_or_default();
            wire += u64::try_from(tile.data.len()).unwrap_or(u64::MAX);
            raw += u64::from(rect.width) * u64::from(rect.height) * 4;
            if tile.codec == appricot_proto::limits::codec::QOI {
                qoi_tiles += 1;
            } else {
                raw_tiles += 1;
            }
            let Ok(pixels) =
                decode_tile(tile.codec, &tile.data, Size::new(rect.width, rect.height))
            else {
                bad += 1;
                continue;
            };
            if let Some(s) = self.surfaces.get_mut(&frame.surface_id) {
                s.blit(rect.x, rect.y, rect.width, rect.height, &pixels.data);
            }
            if let Some(out) = self.tiles.as_mut() {
                out.write(&TileRecord {
                    label: self.label.clone(),
                    surface: frame.surface_id,
                    sequence: frame.sequence,
                    x: rect.x,
                    y: rect.y,
                    width: rect.width,
                    height: rect.height,
                    pixels: pixels.data,
                })?;
            }
        }
        let decode_us = started.elapsed().as_secs_f64() * 1e6;
        let tiles = u64::try_from(frame.tiles.len()).unwrap_or(u64::MAX);
        let stats = self.stats.entry(self.label.clone()).or_default();
        stats.frames += 1;
        stats.full_redraws += u64::from(frame.full_redraw);
        stats.tiles += tiles;
        stats.wire_bytes += wire;
        stats.raw_bytes += raw;
        stats.raw_tiles += raw_tiles;
        stats.qoi_tiles += qoi_tiles;
        stats.decode_us += decode_us;
        self.summary.frames += 1;
        self.summary.tiles += tiles;
        self.summary.wire_bytes += wire;
        self.log(
            JsonObject::new()
                .str("ev", "frame")
                .uint("surface", u64::from(frame.surface_id))
                .uint("sequence", u64::from(frame.sequence))
                .bool("full_redraw", frame.full_redraw)
                .uint("tiles", tiles)
                .uint("wire_bytes", wire)
                .uint("raw_bytes", raw)
                .uint("qoi_tiles", qoi_tiles)
                .uint("undecodable_tiles", bad)
                .float("decode_us", decode_us),
        )?;
        Ok(Body::FrameAck(FrameAck {
            surface_id: frame.surface_id,
            sequence: frame.sequence,
        }))
    }

    /// Writes one PNG per live surface, named after `label`.
    fn snapshot(&mut self, label: &str) -> Result<(), RecordError> {
        let label = crate::control::sanitize_label(label);
        let mut written = Vec::new();
        for (id, s) in &self.surfaces {
            if !s.live || s.size.0 == 0 || s.size.1 == 0 {
                continue;
            }
            let Ok(png) = crate::png::encode_bgrx(s.size.0, s.size.1, &s.canvas) else {
                continue;
            };
            let mut path = self
                .out_dir
                .join("snapshots")
                .join(format!("{label}-s{id}.png"));
            let mut n = 2;
            while path.exists() {
                path = self
                    .out_dir
                    .join("snapshots")
                    .join(format!("{label}-s{id}-{n}.png"));
                n += 1;
            }
            fs::write(&path, png)?;
            written.push(path);
        }
        let names: Vec<String> = written
            .iter()
            .filter_map(|p| p.file_name()?.to_str().map(str::to_owned))
            .collect();
        self.log(
            JsonObject::new()
                .str("ev", "snapshot")
                .str("name", &label)
                .raw("files", &crate::json::string_array(&names)),
        )?;
        self.summary.snapshots.extend(written);
        Ok(())
    }

    fn nth_toplevel(&self, n: usize) -> Option<u32> {
        self.order
            .iter()
            .copied()
            .filter(|id| {
                self.surfaces
                    .get(id)
                    .is_some_and(|s| s.role == RoleKind::Toplevel)
            })
            .nth(n.checked_sub(1)?)
    }

    fn newest_popup(&self) -> Option<u32> {
        self.order.iter().rev().copied().find(|id| {
            self.surfaces
                .get(id)
                .is_some_and(|s| s.role == RoleKind::Popup && s.live)
        })
    }

    fn finish(mut self) -> Result<RecordSummary, RecordError> {
        self.snapshot("final")?;
        self.log(JsonObject::new().str("ev", "stop"))?;
        self.log.flush()?;
        if let Some(tiles) = self.tiles.as_mut() {
            tiles.flush()?;
        }
        fs::write(self.out_dir.join("wire.md"), self.markdown())?;
        Ok(self.summary)
    }

    fn markdown(&self) -> String {
        let mut out = String::from(
            "## Wire\n\n\
             What the streamer sent, per scenario, as the recorder received it.\n\n\
             | Scenario | Frames | Full redraws | Tiles | QOI tiles | RAW tiles | Wire KiB | RAW-equivalent KiB | Wire/RAW | Decode ms |\n\
             |---|---:|---:|---:|---:|---:|---:|---:|---:|---:|\n",
        );
        for label in &self.labels {
            let s = self.stats.get(label);
            let get = |f: fn(&LabelStats) -> u64| s.map_or(0, f);
            let (wire, raw) = (get(|s| s.wire_bytes), get(|s| s.raw_bytes));
            let ratio = if raw == 0 {
                0.0
            } else {
                u64_to_f64(wire) / u64_to_f64(raw)
            };
            let _ = writeln!(
                out,
                "| {label} | {} | {} | {} | {} | {} | {:.1} | {:.1} | {ratio:.3} | {:.1} |",
                get(|s| s.frames),
                get(|s| s.full_redraws),
                get(|s| s.tiles),
                get(|s| s.qoi_tiles),
                get(|s| s.raw_tiles),
                kib(wire),
                kib(raw),
                s.map_or(0.0, |s| s.decode_us / 1000.0),
            );
        }
        out.push_str(
            "\n### Surfaces\n\n\
             | Surface | Role | Parent | Last size | Live at the end | Title |\n\
             |---:|---|---:|---|---|---|\n",
        );
        for id in &self.order {
            let Some(s) = self.surfaces.get(id) else {
                continue;
            };
            let title: String = s
                .title
                .chars()
                .take(48)
                .collect::<String>()
                .replace('|', "\\|");
            let _ = writeln!(
                out,
                "| {id} | {} | {} | {}x{} | {} | {title} |",
                if s.role == RoleKind::Popup {
                    "popup"
                } else {
                    "toplevel"
                },
                s.parent.map_or_else(String::new, |p| p.to_string()),
                s.size.0,
                s.size.1,
                if s.live { "yes" } else { "no" },
            );
        }
        if let Some(bye) = &self.summary.bye {
            let _ = writeln!(out, "\nThe server said Bye: {bye}.");
        }
        out
    }
}

/// Runs a script step by step against the recorder.
struct Runner {
    steps: Vec<(usize, Step)>,
    pc: usize,
    wake_at: Option<Instant>,
    waiting: Option<(RoleKind, usize, Instant, usize)>,
}

impl Runner {
    fn new(script: Script) -> Self {
        Self {
            steps: script.steps,
            pc: 0,
            wake_at: None,
            waiting: None,
        }
    }

    /// Executes steps until one must wait; returns the messages to send.
    fn advance(&mut self, rec: &mut Recorder) -> Result<Vec<Body>, RecordError> {
        let mut out = Vec::new();
        loop {
            let now = Instant::now();
            if self.wake_at.is_some_and(|t| now < t) {
                return Ok(out);
            }
            self.wake_at = None;
            let Some((line, step)) = self.steps.get(self.pc).cloned() else {
                return Ok(out);
            };
            if let Some((role, nth, deadline, popups_before)) = self.waiting {
                let met = match role {
                    RoleKind::Toplevel => rec.nth_toplevel(nth).is_some(),
                    RoleKind::Popup => rec.popups_announced > popups_before,
                };
                if met {
                    self.waiting = None;
                    self.pc += 1;
                    continue;
                }
                if now >= deadline {
                    return Err(RecordError::Script {
                        line,
                        message: "wait-for timed out".into(),
                    });
                }
                return Ok(out);
            }
            if self.execute(rec, line, step, now, &mut out)? {
                continue;
            }
            rec.log(
                JsonObject::new()
                    .str("ev", "step")
                    .uint("line", u64::try_from(line).unwrap_or(u64::MAX)),
            )?;
            self.pc += 1;
            if rec.stop || self.wake_at.is_some() {
                return Ok(out);
            }
        }
    }

    /// Executes one step. True when the step only armed a wait and the loop re-checks it.
    fn execute(
        &mut self,
        rec: &mut Recorder,
        line: usize,
        step: Step,
        now: Instant,
        out: &mut Vec<Body>,
    ) -> Result<bool, RecordError> {
        let fail = |message: &str| RecordError::Script {
            line,
            message: message.to_owned(),
        };
        match step {
            Step::Wait(d) => self.wake_at = Some(now + d),
            Step::WaitFor { role, nth, timeout } => {
                self.waiting = Some((role, nth, now + timeout, rec.popups_announced));
                return Ok(true);
            }
            Step::Focus(n) => {
                let id = rec
                    .nth_toplevel(n)
                    .ok_or_else(|| fail("no such toplevel"))?;
                rec.target = Some(id);
                out.push(Body::FocusNotify(FocusNotify { surface_id: id }));
            }
            Step::TargetToplevel(n) => {
                rec.target = Some(
                    rec.nth_toplevel(n)
                        .ok_or_else(|| fail("no such toplevel"))?,
                );
            }
            Step::TargetPopup => {
                rec.target = Some(rec.newest_popup().ok_or_else(|| fail("no live popup"))?);
            }
            Step::Move(x, y) => {
                let surface_id = rec.target.ok_or_else(|| fail("no target: focus first"))?;
                out.push(Body::PointerMove(PointerMove { surface_id, x, y }));
            }
            Step::Button { button, pressed } => {
                let surface_id = rec.target.ok_or_else(|| fail("no target: focus first"))?;
                out.push(Body::PointerButton(PointerButton {
                    surface_id,
                    button,
                    pressed,
                }));
            }
            Step::Wheel(steps) => {
                let surface_id = rec.target.ok_or_else(|| fail("no target: focus first"))?;
                out.push(Body::PointerAxis(PointerAxis {
                    surface_id,
                    steps_x: 0,
                    steps_y: steps.clamp(-64, 64),
                }));
            }
            Step::Key {
                keysym,
                code,
                mods,
                pressed,
            } => out.push(Body::Key(Key {
                keysym,
                code,
                pressed,
                modifiers: mods,
            })),
            Step::Mark(label) => {
                rec.control.set_mark(&label)?;
                rec.set_label(label)?;
            }
            Step::Snap(label) => rec.snapshot(&label)?,
            Step::Stop => rec.stop = true,
        }
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::{RoleKind, Surface};

    fn surface(w: u32, h: u32) -> Surface {
        let mut s = Surface {
            role: RoleKind::Toplevel,
            parent: None,
            title: String::new(),
            size: (0, 0),
            canvas: Vec::new(),
            live: true,
        };
        s.resize(w, h);
        s
    }

    fn pixel(s: &Surface, x: u32, y: u32) -> [u8; 4] {
        let at = usize::try_from((y * s.size.0 + x) * 4).expect("small");
        s.canvas[at..at + 4].try_into().expect("four bytes")
    }

    #[test]
    fn a_tile_is_painted_where_it_belongs_and_clipped() {
        let mut s = surface(4, 3);
        let red = [0_u8, 0, 255, 255].repeat(4);
        s.blit(1, 1, 2, 2, &red);
        assert_eq!(pixel(&s, 0, 0), [0, 0, 0, 0]);
        assert_eq!(pixel(&s, 1, 1), [0, 0, 255, 255]);
        assert_eq!(pixel(&s, 2, 2), [0, 0, 255, 255]);
        assert_eq!(pixel(&s, 3, 2), [0, 0, 0, 0]);
        // Partly outside on every side: only the overlap is painted.
        let blue = [255_u8, 0, 0, 255].repeat(36);
        s.blit(-2, -2, 6, 6, &blue);
        assert_eq!(pixel(&s, 0, 0), [255, 0, 0, 255]);
        assert_eq!(pixel(&s, 3, 2), [255, 0, 0, 255]);
    }

    #[test]
    fn a_resize_keeps_the_overlap() {
        let mut s = surface(2, 2);
        s.blit(0, 0, 2, 2, &[1, 2, 3, 4].repeat(4));
        s.resize(3, 1);
        assert_eq!(s.canvas.len(), 12);
        assert_eq!(pixel(&s, 1, 0), [1, 2, 3, 4]);
        assert_eq!(pixel(&s, 2, 0), [0, 0, 0, 0]);
    }
}
