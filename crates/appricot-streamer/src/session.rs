//! The session pump: the v0 protocol over one WebSocket.
//!
//! [`run_session`] owns one socket, the [`Session`] state machine and the
//! [`BackendHandle`] for the connection's lifetime, in one `select` loop over the socket and
//! the backend's feed. Frame pacing is the client's acks driving [`Session::plan_frame`], never
//! a tick (docs/protocol/v0.md §6: "nothing on a timer"). The clocks this module does read all
//! manage the session, and none of them sends anything: the resume grace (§7), the handshake
//! deadline (§2) and the send deadline that treats a stalled peer as gone.
//!
//! # Handshake
//!
//! The first binary message must be a `Hello` carrying the stream token (rule 8,
//! docs/protocol/v0.md §5), and it must arrive within the handshake deadline
//! ([`crate::config::handshake_timeout_ms`]). Text frames, undecodable bytes, wrong messages,
//! bad tokens, unsupported versions and a silent socket each close with the `Bye` the spec
//! names. Codecs are negotiated to the first offered codec this server can encode — QOI before
//! RAW when the client's order says so; an empty offer means RAW, as the spec fixes.
//!
//! A refused handshake changes nothing: the pump hands its start back ([`SessionEnd::Refused`])
//! with the session state and the grace deadline exactly as they were.
//!
//! # One session, one resume
//!
//! A dropped socket parks the session (backend and all) for the grace window; a reconnect
//! whose `Hello` names the parked `resume_serial` resumes it — the whole window set is
//! re-announced and every surface gets a full-redraw frame. An authenticated `Hello` that
//! does **not** name it replaces the client (`resumed = false`): the spec leaves this to the
//! implementation's policy (§7), and a client that wants a new session "simply sends Hello
//! without resume_serial". A replacement is told the window set exactly as a first client
//! is: the windows did not close with the client they lost, and the backend announces a
//! window once — at its startup, when it adopts what is mapped — never per session. So the
//! parked session lives on under a fresh serial, its clipboard's not-twice memory dropped
//! for a host that received nothing, and the same announcement rebuilds the newcomer's
//! mirror. `BYE_SESSION_GONE` is answered only when there is nothing left to serve: a
//! resume meets a backend whose display died.
//!
//! Keysyms pass through untouched: docs/protocol/v0.md §8 defines them, and the backend owns
//! the resolution — the streamer never rewrites one, and never logs one: nothing the user types
//! reaches a log line.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use axum::extract::ws::{CloseFrame, Message, WebSocket, close_code};
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::watch;
use tokio::time::Instant;

use appricot_core::{
    Anchor, CaptureBackend, FramePlan, GoneReason, InputSink, KeyCode, KeyEvent, Keysym, Point,
    PointerButton, Positioner, PressState, Rect, Role, Scale, Session, SessionEvent, Size,
    SurfaceId,
};
use appricot_encode::{Encoding, cut_into_tiles, encode_tile_owned};
use appricot_proto::PROTOCOL_VERSION;
use appricot_proto::limits::{MAX_FRAME_CREDITS, MAX_TILES_PER_FRAME, codec};
use appricot_proto::wire::{
    self, Body, Bye, ByeReason, ClipboardText, ConfigureAck, CursorImage, DecodeError, Envelope,
    Frame, HelloReply, ResizeAsk, ServerError, SurfaceGone, SurfaceGoneReason, SurfaceMetadata,
    SurfaceNew, Tile, decode_envelope, encode_envelope,
};

use crate::auth::StreamToken;
use crate::backend::{BackendError, BackendHandle, Feed};
use crate::sent_tiles::{SentTiles, SentTilesPerSurface};

/// The sink half of the session's WebSocket.
type WsSink = SplitSink<WebSocket, Message>;
/// The stream half of the session's WebSocket.
type WsStream = SplitStream<WebSocket>;

/// The longest one outbound message may take to leave. A peer that cannot take a message in
/// this long — it stopped reading, or a proxy in the way stalled — is treated as gone and the
/// session parks, so the backend's feed never waits behind a send that will not finish. Far
/// longer than the largest frame (48 RAW tiles, 12 MiB) takes on any link APPricot serves.
const SEND_DEADLINE: Duration = Duration::from_secs(60);

/// Everything a session needs from the server process.
#[derive(Debug)]
pub struct SessionConfig {
    /// The stream token the first `Hello` must carry. Redacted in `Debug`.
    pub token: Arc<StreamToken>,
    /// Turns true when the server asks its session to stop (see
    /// [`crate::server::ServerState::shutdown`]); the session answers `Bye(BYE_SERVER_SHUTDOWN)`.
    pub stop: watch::Receiver<bool>,
}

/// How the pump starts: with the standby state of an unclaimed backend, or resuming a parked
/// session.
#[derive(Debug)]
pub enum Start<B> {
    /// No client is attached, and none was since the backend started.
    Fresh(Standby<B>),
    /// A parked session waits out its grace; its `Hello` decides resume or replace.
    Resume(ParkedSession<B>),
}

/// Why a session is over for good.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndCause {
    /// The session ran its course: a `Bye` either way, a client's protocol fault, an expired
    /// grace, or a stop the server asked for.
    Clean,
    /// Something on this side failed: the display died, or the server hit an internal fault.
    Fault,
}

/// What the pump hands back when its socket is done.
#[derive(Debug)]
pub enum SessionEnd<B> {
    /// The socket died without a closing `Bye`: the session is parked for its grace, backend
    /// included.
    Parked(ParkedSession<B>),
    /// The session is over (a `Bye` was exchanged, the display died, or the server faulted);
    /// the backend was dropped with it and nothing can be served again.
    Ended(EndCause),
    /// Nothing was claimed: the handshake was refused before an authenticated `Hello` (a bad
    /// token, an unsupported version, a silent socket, no `Hello` at all), or the reply never
    /// left. The start comes back exactly as it went in — session state, resume serial and
    /// grace deadline untouched — and the next client may try again: a mistyped token costs a
    /// retry, not the process.
    Refused(Start<B>),
}

/// A backend and the session state kept on it while no socket serves it.
///
/// [`Standby::absorb_feed`] keeps the display's events flowing into the session, so the state
/// stays current and the feed channel stays bounded however long nobody is attached; the keeper
/// task in [`crate::server`] calls it.
#[derive(Debug)]
pub struct Standby<B> {
    session: Session,
    backend: BackendHandle<B>,
    /// Set when the display died while nobody was attached.
    fatal: Option<BackendError>,
}

impl<B> Standby<B> {
    /// The standby state of a backend no session ever ran on.
    pub fn new(backend: BackendHandle<B>) -> Self {
        Self {
            session: Session::new(),
            backend,
            fatal: None,
        }
    }

    /// Applies whatever the backend produced since the last call; outputs are discarded — the
    /// client, when it comes, is synchronised by the announcement of the whole window set, not
    /// by a replay.
    pub fn absorb_feed(&mut self) {
        absorb(&mut self.session, &mut self.backend, &mut self.fatal);
    }

    /// Whether the display died while nobody was attached.
    pub fn display_died(&self) -> bool {
        self.fatal.is_some()
    }
}

/// A session that outlives its socket, waiting out the resume grace.
///
/// The grace ends at an absolute [`ParkedSession::deadline`], fixed when the socket died. A
/// refused handshake hands the session back with the same deadline, so no number of refusals
/// extends the grace.
#[derive(Debug)]
pub struct ParkedSession<B> {
    standby: Standby<B>,
    /// The serial a reconnecting `Hello` must name.
    resume_serial: u32,
    /// When the grace runs out.
    deadline: Instant,
}

impl<B> ParkedSession<B> {
    /// Applies whatever the backend produced while parked (see [`Standby::absorb_feed`]).
    pub fn absorb_feed(&mut self) {
        self.standby.absorb_feed();
    }

    /// When the grace runs out; the keeper tears the session down from then on.
    pub fn deadline(&self) -> Instant {
        self.deadline
    }

    /// Whether the display died while the session was parked.
    pub fn display_died(&self) -> bool {
        self.standby.display_died()
    }
}

/// Drains the backend's feed into `session`, discarding what it would have sent.
fn absorb<B>(
    session: &mut Session,
    backend: &mut BackendHandle<B>,
    fatal: &mut Option<BackendError>,
) {
    for feed in backend.drain_feed() {
        match feed {
            Feed::Events(events) => {
                let mut discard = Vec::new();
                for event in events {
                    session.apply_event(event, &mut discard);
                }
            }
            Feed::Fatal(e) => *fatal = Some(e),
        }
    }
}

/// Sessions and resume serials are numbered per process, starting at 1 (0 would read as
/// "absent" to tired eyes even though the wire's `optional` makes it distinct).
static SESSION_COUNTER: AtomicU32 = AtomicU32::new(1);

/// Server-error codes of v0 (wire.proto, `ServerError.code`).
const ERR_LIMIT: u32 = 1;
const ERR_ORDER: u32 = 3;
const ERR_DECODE: u32 = 4;
const ERR_INTERNAL: u32 = 5;

/// Runs one session from upgrade to socket end.
///
/// A client fault never panics and never fails the process: every violation answers with the
/// `ServerError` and `Bye` the spec names and closes. A parked session is offered only to a
/// socket that died without saying why.
pub async fn run_session<B>(
    socket: WebSocket,
    mut config: SessionConfig,
    start: Start<B>,
) -> SessionEnd<B>
where
    B: CaptureBackend + InputSink + Send + 'static,
{
    let (mut sink, mut stream) = socket.split();

    let mut pump = match start {
        Start::Fresh(standby) => Pump {
            session: standby.session,
            backend: standby.backend,
            surfaces: Vec::new(),
            configure_acks: HashMap::new(),
            sent_tiles: HashMap::new(),
            prefer: Encoding::Raw,
            parked: None,
            fatal: standby.fatal,
            delivery_warned: false,
        },
        Start::Resume(parked) => Pump {
            session: parked.standby.session,
            backend: parked.standby.backend,
            // The resume re-announces the window set; the mirror is rebuilt from it.
            surfaces: Vec::new(),
            configure_acks: HashMap::new(),
            // The comparison cache dies with the connection it served: the resume's full
            // redraws rebuild it, so no tile of the dead connection is held as sent.
            sent_tiles: HashMap::new(),
            prefer: Encoding::Raw,
            parked: Some((parked.resume_serial, parked.deadline)),
            fatal: parked.standby.fatal,
            delivery_warned: false,
        },
    };

    match handshake(&mut sink, &mut stream, &mut config, &mut pump).await {
        // Nothing was claimed: hand the start straight back (see `SessionEnd::Refused`).
        Handshake::Unclaimed => SessionEnd::Refused(unclaim(pump)),
        Handshake::Ended(cause) => SessionEnd::Ended(cause),
        // The client holds the new serial; the state it names is parked under it.
        Handshake::Lost { resume_serial } => park(pump, resume_serial, "handshake"),
        Handshake::Accepted {
            resume_serial,
            session_id,
        } => {
            // The announcement marked every surface for a full redraw; nothing on the feed will
            // call for a frame afterwards, so the first pump happens here, right after the
            // announcement with nothing in between (v0.md §7).
            if let Some(exit) = pump_frames(&mut sink, &mut pump).await.exit() {
                return finish(exit, pump, resume_serial, &session_id).await;
            }
            steady(
                &mut sink,
                &mut stream,
                &mut config,
                pump,
                resume_serial,
                session_id,
            )
            .await
        }
    }
}

/// One live session's state beside the socket.
#[derive(Debug)]
struct Pump<B> {
    session: Session,
    backend: BackendHandle<B>,
    /// The living surface ids, in creation order, mirrored from the session's announcements
    /// (the session exposes no iteration, by design: the streamer is the one that talks).
    surfaces: Vec<SurfaceId>,
    /// Per surface, the configures still waiting, oldest first: the core serial of each and
    /// the client serial it must be answered with. Core mints its own serials, the wire
    /// promises the client its own back. Bounded like core's queue, by
    /// [`appricot_core::MAX_PENDING_CONFIGURES`].
    configure_acks: HashMap<SurfaceId, Vec<(u32, u32)>>,
    /// Per surface, what the client last received per grid cell ([`crate::sent_tiles`]), so a
    /// tile identical to it is omitted from the next frame. Connection-scoped: it never
    /// survives a park, because the resume's full redraws rebuild it.
    sent_tiles: SentTilesPerSurface,
    /// The codec the encoder prefers this session (RAW is the fallback whatever this says).
    prefer: Encoding,
    /// The park this pump claimed: the serial a resume must name and the grace deadline it
    /// keeps. `None` on a fresh start, meaning nothing can be resumed.
    parked: Option<(u32, Instant)>,
    /// Set when the display died while nobody was attached, or during the handshake.
    fatal: Option<BackendError>,
    /// Whether a failed input delivery was already reported at warn level this session.
    delivery_warned: bool,
}

impl<B> Pump<B> {
    /// Drains the backend's feed into the session, discarding what it would have sent (see
    /// [`Standby::absorb_feed`]).
    fn absorb_feed(&mut self) {
        absorb(&mut self.session, &mut self.backend, &mut self.fatal);
    }
}

/// Hands an unclaimed start back after a refused handshake.
///
/// A fresh backend is still fresh, with its standby session. A parked session is still parked,
/// under its own serial and with its **original** grace deadline: the refusal happened before
/// anything could change it, and refusing a handshake never buys more time.
fn unclaim<B>(pump: Pump<B>) -> Start<B> {
    let standby = Standby {
        session: pump.session,
        backend: pump.backend,
        fatal: pump.fatal,
    };
    match pump.parked {
        None => Start::Fresh(standby),
        Some((resume_serial, deadline)) => Start::Resume(ParkedSession {
            standby,
            resume_serial,
            deadline,
        }),
    }
}

/// Parks the pump's session under `resume_serial`, with a grace that starts now.
fn park<B>(mut pump: Pump<B>, resume_serial: u32, session_id: &str) -> SessionEnd<B> {
    pump.session.detach();
    tracing::info!(session = %session_id, "socket gone; session parked for the grace");
    SessionEnd::Parked(ParkedSession {
        standby: Standby {
            session: pump.session,
            backend: pump.backend,
            fatal: pump.fatal,
        },
        resume_serial,
        deadline: Instant::now()
            + Duration::from_millis(u64::from(crate::config::resume_grace_ms())),
    })
}

/// Ends a pump's run the way `exit` says.
///
/// Every key and button the backend holds is released first (contract C9): a socket that goes,
/// or a session that ends, never leaves the app with a key held down.
async fn finish<B>(exit: Exit, pump: Pump<B>, resume_serial: u32, session_id: &str) -> SessionEnd<B>
where
    B: CaptureBackend + InputSink + Send + 'static,
{
    if let Err(e) = pump.backend.blur().await {
        tracing::debug!(error = %e, "releasing held input failed");
    }
    match exit {
        Exit::Park => park(pump, resume_serial, session_id),
        Exit::End(cause) => {
            tracing::info!(session = %session_id, ?cause, "session ended");
            SessionEnd::Ended(cause)
        }
    }
}

/// The outcome of the handshake.
enum Handshake {
    /// Nothing changed: refused before the authenticated `Hello` (the refusal is already
    /// answered and logged), or the reply never left. The caller hands the start back.
    Unclaimed,
    /// Authenticated, and the session cannot go on; the `Bye` was sent.
    Ended(EndCause),
    /// The client received the reply's new serial, and the socket died before the
    /// announcement finished: the session parks under that serial.
    Lost {
        /// The serial the client holds.
        resume_serial: u32,
    },
    /// The handshake succeeded.
    Accepted {
        /// The serial a future reconnect must name.
        resume_serial: u32,
        /// The session id for logs and support.
        session_id: String,
    },
}

/// Logs a refused handshake: the reason only, never what the peer offered.
fn refused(reason: &'static str) {
    tracing::info!(reason, "handshake refused");
}

/// Resolves once the server asks the session to stop; never resolves otherwise.
async fn stop_asked(stop: &mut watch::Receiver<bool>) {
    if stop.wait_for(|stop| *stop).await.is_err() {
        // The server state is gone, and with it anyone who could ask; nothing will.
        std::future::pending::<()>().await;
    }
}

/// Reads the first message and checks it is an authenticated `Hello` this server speaks, or
/// closes with the refusal the spec names and says how the handshake ended.
async fn read_hello(
    sink: &mut WsSink,
    stream: &mut WsStream,
    config: &mut SessionConfig,
) -> Result<wire::Hello, Handshake> {
    let deadline =
        Instant::now() + Duration::from_millis(u64::from(crate::config::handshake_timeout_ms()));
    let bytes = loop {
        let next = tokio::select! {
            biased;
            () = stop_asked(&mut config.stop) => {
                let _ = send_bye(sink, ByeReason::ByeServerShutdown, "the server is stopping")
                    .await;
                return Err(Handshake::Ended(EndCause::Clean));
            }
            next = tokio::time::timeout_at(deadline, stream.next()) => next,
        };
        match next {
            // A socket that upgrades and then says nothing must not hold the one slot: the
            // deadline covers the whole wait, pings included (v0.md §2).
            Err(_elapsed) => {
                refused("no Hello before the handshake deadline");
                let _ = send_bye(
                    sink,
                    ByeReason::ByeProtocolViolation,
                    "no Hello before the handshake deadline",
                )
                .await;
                return Err(Handshake::Unclaimed);
            }
            // A dead socket and a close frame are the same refusal: no Hello arrived.
            Ok(None | Some(Err(_) | Ok(Message::Close(_)))) => {
                refused("the socket closed before a Hello");
                return Err(Handshake::Unclaimed);
            }
            // Pings are answered by the socket itself; pongs carry nothing for us.
            Ok(Some(Ok(Message::Ping(_) | Message::Pong(_)))) => {}
            // No text frames, ever (rule 2); the first message must be binary.
            Ok(Some(Ok(Message::Text(_)))) => {
                refused("a text frame");
                let _ = send_bye(sink, ByeReason::ByeProtocolViolation, "binary frames only").await;
                return Err(Handshake::Unclaimed);
            }
            Ok(Some(Ok(Message::Binary(bytes)))) => break bytes,
        }
    };

    let hello = match decode_envelope(&bytes) {
        Ok(Envelope {
            body: Some(Body::Hello(hello)),
        }) => hello,
        Ok(_) => {
            refused("the first message is not a Hello");
            let _ = send_bye(sink, ByeReason::ByeProtocolViolation, "expected Hello").await;
            return Err(Handshake::Unclaimed);
        }
        Err(e) => {
            refused("the first message does not decode");
            refuse_undecodable(sink, &e).await;
            return Err(Handshake::Unclaimed);
        }
    };

    // Authenticate before anything else is even looked at.
    if !config.token.matches(&hello.stream_token) {
        refused("bad token");
        let _ = send_bye(sink, ByeReason::ByeAuthFailed, "").await;
        return Err(Handshake::Unclaimed);
    }

    // Version: refuse what we do not speak; never guess (rule 1).
    if hello.protocol_version != u32::from(PROTOCOL_VERSION) {
        refused("unsupported protocol version");
        let _ = send_bye(
            sink,
            ByeReason::ByeProtocolVersion,
            &format!("server speaks v{PROTOCOL_VERSION}"),
        )
        .await;
        return Err(Handshake::Unclaimed);
    }
    Ok(hello)
}

/// Reads the first message and answers the `Hello`, or closes with the refusal the spec names.
async fn handshake<B>(
    sink: &mut WsSink,
    stream: &mut WsStream,
    config: &mut SessionConfig,
    pump: &mut Pump<B>,
) -> Handshake
where
    B: CaptureBackend + InputSink + Send + 'static,
{
    let hello = match read_hello(sink, stream, config).await {
        Ok(hello) => hello,
        Err(outcome) => return outcome,
    };

    // Authenticated. Bring the state up to date first: the feed went unread while the socket
    // was upgrading, and a display that died meanwhile must be seen before anything is promised.
    pump.absorb_feed();

    // Codecs: answer the first offered codec the encoder can produce.
    pump.prefer = negotiate(&hello.codecs);

    // Resume or replace. Serials are process-unique, so a fresh session never collides with
    // the serial of the one it replaced.
    let parked_serial = pump.parked.map(|(serial, _)| serial);
    let resumed = parked_serial.is_some() && hello.resume_serial == parked_serial;

    // A display that died leaves nothing to serve: a resume meets the session-gone Bye (v0.md
    // §7), any other Hello the shutdown Bye, and the session ends either way.
    if pump.fatal.is_some() {
        let (reason, text) = if resumed {
            (
                ByeReason::ByeSessionGone,
                "the display died while the session was parked",
            )
        } else {
            (ByeReason::ByeServerShutdown, "the display died")
        };
        let _ = send_bye(sink, reason, text).await;
        tracing::error!("display dead; the handshake ended the session");
        return Handshake::Ended(EndCause::Fault);
    }

    let number = SESSION_COUNTER.fetch_add(1, Ordering::Relaxed);
    let resume_serial = number;
    let session_id = format!("session-{number}");

    let reply = HelloReply {
        protocol_version: u32::from(PROTOCOL_VERSION),
        session_id: session_id.clone(),
        // The limits table keeps both far below any integer width that matters; try_from is
        // only here to say so in the type, not because it can fail.
        max_frame_credits: u32::try_from(MAX_FRAME_CREDITS).expect("fits the limits table"),
        codecs: match pump.prefer {
            Encoding::Qoi => vec![codec::RAW, codec::QOI],
            // RAW is the floor; a codec added later still leaves RAW as the must-decode.
            Encoding::Raw => vec![codec::RAW],
        },
        resumed,
        resume_serial: Some(resume_serial),
        // What the server will actually honour: the test/demo knob overrides the wait and the
        // advertisement together, so the reply never promises a grace it will not keep
        // (crate::config::resume_grace_ms).
        resume_grace_ms: Some(crate::config::resume_grace_ms()),
    };
    match send_body(sink, Body::HelloReply(reply)).await {
        Ok(()) => {}
        // The reply never left and nothing has changed yet: the client still holds only the
        // serial it came with, so everything goes back exactly as it was.
        Err(SendFault::Socket) => return Handshake::Unclaimed,
        Err(SendFault::Encode) => {
            internal_fault(sink).await;
            return Handshake::Ended(EndCause::Fault);
        }
    }

    // A Hello that does not name the parked serial replaces the client (v0.md §7) — only now
    // that the client holds the new serial. The parked session keeps everything the display
    // still says — above all its window set, for the backend announces a window once, at its
    // startup, and never per session — and the announcement below resynchronises the newcomer
    // exactly as it resynchronises a resume: every surface re-announced under a full redraw,
    // pacing started over. Only the clipboard's not-twice memory dies with the replaced
    // client, which received nothing for it to remember.
    if parked_serial.is_some() && !resumed {
        pump.session.forget_clipboard();
    }

    // Announce the whole window set, then the frames (run_session), with nothing in between
    // (v0.md §7). For a resume this is the resynchronisation, always; for a fresh start it is
    // every window the app opened before the client came, when it opened any; for a
    // replacement it is both at once, and with no window either it still must run, to bring
    // the parked session back to a client that can be told things again.
    let mut out = Vec::new();
    if pump.session.is_detached() || pump.session.surface_count() > 0 {
        pump.session.resume(&mut out);
    }
    for event in &out {
        match send_session_event(sink, pump, event).await {
            Ok(()) => {}
            Err(SendFault::Socket) => return Handshake::Lost { resume_serial },
            Err(SendFault::Encode) => {
                internal_fault(sink).await;
                return Handshake::Ended(EndCause::Fault);
            }
        }
    }

    Handshake::Accepted {
        resume_serial,
        session_id,
    }
}

/// Applies one feed item: events into the session, then whatever frames they call for.
async fn apply_feed<B>(
    feed: Option<Feed>,
    sink: &mut WsSink,
    pump: &mut Pump<B>,
    session_id: &str,
) -> Flow
where
    B: CaptureBackend + InputSink + Send + 'static,
{
    match feed {
        // The backend is gone: nothing can be served again (no park; the connection is
        // dead or the thread ended). The cause is logged because the bare "display dead"
        // once sent someone to an X server that was alive: a refused request and a broken
        // connection say different things about the machine, and only the cause tells
        // them apart.
        Some(Feed::Fatal(cause)) => {
            let _ = send_bye(sink, ByeReason::ByeServerShutdown, "the display died").await;
            tracing::error!(session = %session_id, error = %cause, "backend fault; session ended");
            Flow::Stop(EndCause::Fault)
        }
        None => {
            let _ = send_bye(sink, ByeReason::ByeServerShutdown, "the display died").await;
            tracing::error!(
                session = %session_id,
                error = "the backend thread ended",
                "backend fault; session ended"
            );
            Flow::Stop(EndCause::Fault)
        }
        Some(Feed::Events(events)) => {
            let mut out = Vec::new();
            for event in events {
                pump.session.apply_event(event, &mut out);
            }
            match emit_events(sink, pump, &out).await {
                Flow::On => pump_frames(sink, pump).await,
                flow => flow,
            }
        }
    }
}

/// The steady state: one select over the socket and the backend's feed.
async fn steady<B>(
    sink: &mut WsSink,
    stream: &mut WsStream,
    config: &mut SessionConfig,
    mut pump: Pump<B>,
    resume_serial: u32,
    session_id: String,
) -> SessionEnd<B>
where
    B: CaptureBackend + InputSink + Send + 'static,
{
    tracing::info!(session = %session_id, "session started");
    loop {
        // The display gets a look on every iteration, before the socket: the `biased` select
        // below polls the socket first, and a client that talks without pause — a pointer
        // being moved — leaves it ready every time, so the feed starved and the stream
        // collapsed to about one frame per second while the pointer moved, catching up only
        // in the pauses (measured 2026-09-22: 169 pointer moves in 1.5 s produced 2 frames,
        // trailing lag 168 ms). That is the delay a user feels as a lagging pointer.
        //
        // The socket is read again only once a drain comes back empty. What keeps that from
        // starving the socket in turn is not the batch but its producers: the actor pushes at
        // most one feed item per command or per 25 ms poll, and a batch can send frames only
        // while credits are free — at most MAX_FRAME_CREDITS per surface, refilled only by acks
        // read from the socket. A change that lets the feed produce faster, or adds output
        // that needs no credit, must keep that bound or poll the socket inside this loop.
        let ready = pump.backend.drain_feed();
        if !ready.is_empty() {
            for feed in ready {
                if let Some(exit) = apply_feed(Some(feed), sink, &mut pump, &session_id)
                    .await
                    .exit()
                {
                    return finish(exit, pump, resume_serial, &session_id).await;
                }
            }
            continue;
        }

        let from_client = tokio::select! {
            biased;
            () = stop_asked(&mut config.stop) => {
                let _ = send_bye(sink, ByeReason::ByeServerShutdown, "the server is stopping")
                    .await;
                return finish(Exit::End(EndCause::Clean), pump, resume_serial, &session_id)
                    .await;
            }
            message = stream.next() => message,
            feed = pump.backend.next_feed() => {
                if let Some(exit) = apply_feed(feed, sink, &mut pump, &session_id).await.exit() {
                    return finish(exit, pump, resume_serial, &session_id).await;
                }
                continue;
            }
        };

        let flow = match from_client {
            // The socket died, or closed without a Bye: the client is gone without a word, and
            // the session parks for its grace (v0.md §7).
            None | Some(Err(_) | Ok(Message::Close(_))) => Flow::Gone,
            Some(Ok(Message::Binary(bytes))) => handle_client_bytes(sink, &mut pump, &bytes).await,
            // Pings are answered by the socket itself; pongs carry nothing for us.
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => Flow::On,
            // No text frames, ever (rule 2).
            Some(Ok(Message::Text(_))) => {
                let _ = send_bye(sink, ByeReason::ByeProtocolViolation, "binary frames only").await;
                Flow::Stop(EndCause::Clean)
            }
        };
        if let Some(exit) = flow.exit() {
            return finish(exit, pump, resume_serial, &session_id).await;
        }
    }
}

/// Whether the steady loop carries on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flow {
    /// Keep going.
    On,
    /// The session is over; whatever the spec names was already said on the wire.
    Stop(EndCause),
    /// The socket is gone — a send failed or stalled: the session parks, exactly as when a
    /// read fails (v0.md §7, design rule 11).
    Gone,
}

/// How a pump's run ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Exit {
    /// The socket is gone: park for the grace.
    Park,
    /// The session is over.
    End(EndCause),
}

impl Flow {
    /// How the run ends, or `None` while it carries on.
    fn exit(self) -> Option<Exit> {
        match self {
            Self::On => None,
            Self::Stop(cause) => Some(Exit::End(cause)),
            Self::Gone => Some(Exit::Park),
        }
    }
}

/// Why a send did not happen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SendFault {
    /// The encoder refused the message: a bug on this side.
    Encode,
    /// The socket failed, or the peer stalled past [`SEND_DEADLINE`]: it is gone.
    Socket,
}

/// Turns a failed send into the flow it means: a gone socket parks, an encode refusal is an
/// internal fault that ends the session.
async fn fault_flow(sink: &mut WsSink, fault: SendFault) -> Flow {
    match fault {
        SendFault::Socket => Flow::Gone,
        SendFault::Encode => internal_fault(sink).await,
    }
}

/// Closes on a fault of this server's own: `ServerError` code 5 (internal error), then
/// `Bye(BYE_SERVER_SHUTDOWN)` — never a reason that blames the client.
async fn internal_fault(sink: &mut WsSink) -> Flow {
    let _ = send_error_and_bye(
        sink,
        ERR_INTERNAL,
        "internal error",
        ByeReason::ByeServerShutdown,
    )
    .await;
    Flow::Stop(EndCause::Fault)
}

/// The refusal a decode failure earns: `None` to close without a reply, else the
/// `ServerError` code and the `Bye` reason.
fn refusal_for(e: &DecodeError) -> Option<(u32, ByeReason)> {
    match e {
        // An envelope whose oneof names no body this server knows: a protocol violation that
        // closes without a reply (v0.md §1).
        DecodeError::UnknownMessage => None,
        DecodeError::Decode(_) | DecodeError::ProtocolViolation { .. } => {
            Some((ERR_DECODE, ByeReason::ByeProtocolViolation))
        }
        _ => Some((ERR_LIMIT, ByeReason::ByeLimitViolation)),
    }
}

/// Answers a decode failure the way [`refusal_for`] says, and closes.
async fn refuse_undecodable(sink: &mut WsSink, e: &DecodeError) {
    match refusal_for(e) {
        None => close_without_reply(sink).await,
        Some((code, reason)) => {
            let _ = send_error_and_bye(sink, code, &e.to_string(), reason).await;
        }
    }
}

/// Closes the socket with no protocol message: only the WebSocket close frame, with code 1002
/// (protocol error).
async fn close_without_reply(sink: &mut WsSink) {
    let close = CloseFrame {
        code: close_code::PROTOCOL,
        reason: "".into(),
    };
    if let Err(e) = sink.send(Message::Close(Some(close))).await {
        tracing::debug!(error = %e, "socket close failed");
    }
    let _ = sink.close().await;
}

/// Picks the codec this session's tiles prefer: the first offered codec the encoder can
/// produce, or RAW when the offer names none of them.
fn negotiate(offered: &[u32]) -> Encoding {
    for id in offered {
        if let Some(prefer) = Encoding::from_id(*id) {
            return prefer;
        }
    }
    Encoding::Raw
}

/// Decodes one client message, sending the refusal the spec names when it cannot be.
///
/// The `Err` flow has already answered on the wire; the caller only stops.
async fn decode_client_message(sink: &mut WsSink, bytes: &[u8]) -> Result<Body, Flow> {
    match decode_envelope(bytes) {
        Ok(Envelope { body: Some(body) }) => Ok(body),
        // The codec refuses a body-less envelope itself (see `refusal_for`); should it ever
        // hand one over, it earns the same silent close.
        Ok(Envelope { body: None }) => {
            close_without_reply(sink).await;
            Err(Flow::Stop(EndCause::Clean))
        }
        Err(e) => {
            refuse_undecodable(sink, &e).await;
            Err(Flow::Stop(EndCause::Clean))
        }
    }
}

/// Applies the host's configure to the session and the backend.
///
/// An absent size names nothing and is ignored, never fatal. The session names the serial the
/// ack will carry once the backend reports the size the app took — or acks it at once when
/// the proposal changes nothing, and that ack goes out here.
async fn apply_configure<B>(sink: &mut WsSink, pump: &mut Pump<B>, m: wire::Configure) -> Flow
where
    B: CaptureBackend + InputSink + Send + 'static,
{
    let id = SurfaceId::new(m.surface_id);
    let Some(size) = m.size else {
        return Flow::On; // absent size names nothing; ignore, never fatal
    };
    let size = Size::new(size.width, size.height);
    let mut out = Vec::new();
    if let Some(core_serial) = pump.session.configure(id, size, &mut out) {
        let waiting = pump.configure_acks.entry(id).or_default();
        if waiting.len() >= appricot_core::MAX_PENDING_CONFIGURES {
            waiting.remove(0);
        }
        waiting.push((core_serial.get(), m.serial));
        // A proposal of the size the surface already has is acked at once, and the backend
        // is left alone: its own report of the unchanged size would reach the session later,
        // after the next proposal was queued, and ack that one with the old size.
        let acked_at_once = out.iter().any(
            |e| matches!(e, SessionEvent::ConfigureAcked { serial, .. } if *serial == core_serial),
        );
        let flow = emit_events(sink, pump, &out).await;
        if flow != Flow::On {
            return flow;
        }
        if acked_at_once {
            return Flow::On;
        }
        if let Err(e) = pump.backend.configure(id, size).await {
            tracing::warn!(surface = id.get(), error = %e, "backend refused a configure");
        }
    }
    Flow::On
}

/// Delivers one key press or release to the backend.
///
/// The keysym is authoritative and passes through untouched (v0.md §8); the backend owns any
/// codepoint-to-X11 resolution. An invalid physical code drops the message, never the
/// connection. Nothing about the key is ever logged — not the keysym, not the code, not the
/// backend's error, which can name the keysym: that is what the user typed.
async fn deliver_key<B>(pump: &mut Pump<B>, m: wire::Key) -> Flow
where
    B: CaptureBackend + InputSink + Send + 'static,
{
    let Some(code) = KeyCode::new(&m.code) else {
        tracing::debug!("key with an invalid physical code ignored");
        return Flow::On;
    };
    let key = KeyEvent {
        keysym: Keysym(m.keysym),
        code: Some(code),
        state: press_state(m.pressed),
    };
    if pump.backend.key(key).await.is_err() {
        delivery_failed(pump, "key", None);
    }
    Flow::On
}

/// Logs a failed input delivery without flooding the log: the first failure of a session at
/// warn, every later one at debug (a backend whose thread died fails every pointer move).
/// `error` is left out where its text could carry what the user typed.
fn delivery_failed<B>(pump: &mut Pump<B>, what: &'static str, error: Option<&BackendError>) {
    let first = !pump.delivery_warned;
    pump.delivery_warned = true;
    match (first, error) {
        (true, Some(e)) => {
            tracing::warn!(what, error = %e, "input delivery failed; later failures log at debug");
        }
        (true, None) => tracing::warn!(what, "input delivery failed; later failures log at debug"),
        (false, Some(e)) => tracing::debug!(what, error = %e, "input delivery failed"),
        (false, None) => tracing::debug!(what, "input delivery failed"),
    }
}

/// Whether `id` names a surface this connection announced and that still lives.
///
/// Input goes only there: a window the session refused (past a cap) or never reported is
/// tracked by the backend all the same, and a guessed id must not reach it.
fn announced<B>(pump: &Pump<B>, id: SurfaceId) -> bool {
    pump.surfaces.contains(&id)
}

/// Handles one binary message from the client in the steady state.
async fn handle_client_bytes<B>(sink: &mut WsSink, pump: &mut Pump<B>, bytes: &[u8]) -> Flow
where
    B: CaptureBackend + InputSink + Send + 'static,
{
    let body = match decode_client_message(sink, bytes).await {
        Ok(body) => body,
        Err(flow) => return flow,
    };
    match body {
        Body::Configure(m) => apply_configure(sink, pump, m).await,
        Body::FrameAck(m) => {
            pump.session
                .frame_ack(SurfaceId::new(m.surface_id), m.sequence);
            // A freed credit may let a coalesced frame out right away (never a timer).
            pump_frames(sink, pump).await
        }
        Body::PointerMove(m) => {
            let id = SurfaceId::new(m.surface_id);
            if announced(pump, id)
                && let Err(e) = pump.backend.pointer_motion(id, Point::new(m.x, m.y)).await
            {
                delivery_failed(pump, "pointer motion", Some(&e));
            }
            Flow::On
        }
        Body::PointerButton(m) => {
            let id = SurfaceId::new(m.surface_id);
            let Some(button) = pointer_button(m.button) else {
                return Flow::On; // not an X button: ignore, never fatal
            };
            let state = press_state(m.pressed);
            if announced(pump, id)
                && let Err(e) = pump.backend.pointer_button(id, button, state).await
            {
                delivery_failed(pump, "pointer button", Some(&e));
            }
            Flow::On
        }
        Body::PointerAxis(m) => {
            let id = SurfaceId::new(m.surface_id);
            if announced(pump, id)
                && let Err(e) = pump
                    .backend
                    .pointer_axis(id, Point::new(m.steps_x, m.steps_y))
                    .await
            {
                delivery_failed(pump, "pointer axis", Some(&e));
            }
            Flow::On
        }
        Body::Key(m) => deliver_key(pump, m).await,
        Body::FocusNotify(m) => {
            let id = SurfaceId::new(m.surface_id);
            if announced(pump, id)
                && let Err(e) = pump.backend.focus(id).await
            {
                delivery_failed(pump, "focus", Some(&e));
            }
            Flow::On
        }
        Body::BlurRelease(_) => {
            if let Err(e) = pump.backend.blur().await {
                delivery_failed(pump, "blur", Some(&e));
            }
            Flow::On
        }
        Body::ClipboardSet(m) => {
            // The codec already capped the text at MAX_CLIPBOARD_BYTES before this ran. The
            // error is left out of the log: the text is what the user copied.
            //
            // The paste re-arms the not-twice rule of the other direction (v0.md §4.4): the
            // streamer owns the selection again, so the app's next copy is a change to the
            // host even when its text is identical to the last one fetched.
            pump.session.note_clipboard_set();
            if pump.backend.clipboard_set(m.text).await.is_err() {
                delivery_failed(pump, "clipboard", None);
            }
            Flow::On
        }
        Body::CloseRequest(m) => {
            let id = SurfaceId::new(m.surface_id);
            if pump.session.close_request(id)
                && let Err(e) = pump.backend.close(id).await
            {
                delivery_failed(pump, "close request", Some(&e));
            }
            Flow::On
        }
        // The client said Bye: the clean end. Answer in kind, close, and the session ends
        // with the backend — nothing is parked for a client that said goodbye.
        Body::Bye(_) => {
            let _ = send_bye(sink, ByeReason::ByePeerClosed, "").await;
            Flow::Stop(EndCause::Clean)
        }
        // A second Hello, or a message from the wrong direction: forbidden orders (§5). The
        // ServerError carries the code every client-caused close carries, then the Bye names
        // the reason — the same shape a decode failure gets.
        _ => {
            let _ = send_error_and_bye(
                sink,
                ERR_ORDER,
                "message out of order",
                ByeReason::ByeProtocolViolation,
            )
            .await;
            Flow::Stop(EndCause::Clean)
        }
    }
}

/// Maps the wire's X button numbers to the model's buttons.
fn pointer_button(button: u32) -> Option<PointerButton> {
    match button {
        1 => Some(PointerButton::Left),
        2 => Some(PointerButton::Middle),
        3 => Some(PointerButton::Right),
        _ => None,
    }
}

/// Maps pressed flags to press states.
fn press_state(pressed: bool) -> PressState {
    if pressed {
        PressState::Pressed
    } else {
        PressState::Released
    }
}

/// Sends every session event, keeping the mirrored surface set current.
async fn emit_events<B>(sink: &mut WsSink, pump: &mut Pump<B>, out: &[SessionEvent]) -> Flow
where
    B: CaptureBackend + InputSink + Send + 'static,
{
    for event in out {
        if let Err(fault) = send_session_event(sink, pump, event).await {
            return fault_flow(sink, fault).await;
        }
    }
    Flow::On
}

/// Maps one session event onto the wire and sends it.
///
/// A mapping that cannot be encoded is a bug in this crate: it closes the socket rather than
/// emit a broken message.
/// One arm per `SessionEvent`: the mirror of what the session emits. Its length is the
/// event count, not complexity — one message-shaped arm each.
#[allow(clippy::too_many_lines)]
async fn send_session_event<B>(
    sink: &mut WsSink,
    pump: &mut Pump<B>,
    event: &SessionEvent,
) -> Result<(), SendFault>
where
    B: CaptureBackend + InputSink + Send + 'static,
{
    // The mirrored set first: whatever happens to the socket, it stays truthful.
    match event {
        SessionEvent::SurfaceNew { id, .. } => {
            if !pump.surfaces.contains(id) {
                pump.surfaces.push(*id);
            }
        }
        SessionEvent::SurfaceGone { id, .. } => {
            pump.surfaces.retain(|s| s != id);
            pump.configure_acks.remove(id);
            pump.sent_tiles.remove(id);
        }
        _ => {}
    }
    let body = match event {
        SessionEvent::SurfaceNew {
            id,
            role,
            parent,
            size,
            title,
            app_id,
            positioner,
            scale,
        } => Body::SurfaceNew(SurfaceNew {
            surface_id: id.get(),
            role: wire_role(*role),
            parent_id: parent.map(SurfaceId::get),
            size: Some(wire::Size {
                width: size.width,
                height: size.height,
            }),
            title: title.as_str().to_owned(),
            app_id: app_id.as_str().to_owned(),
            positioner: positioner.as_ref().map(wire_positioner),
            scale_120ths: scale.as_120ths(),
        }),
        SessionEvent::SurfaceGone { id, reason } => Body::SurfaceGone(SurfaceGone {
            surface_id: id.get(),
            reason: wire_gone_reason(*reason),
        }),
        SessionEvent::SurfaceMetadata {
            id,
            title,
            app_id,
            scale,
        } => Body::SurfaceMetadata(SurfaceMetadata {
            surface_id: id.get(),
            title: title.as_ref().map(|t| t.as_str().to_owned()),
            app_id: app_id.as_ref().map(|a| a.as_str().to_owned()),
            scale_120ths: scale.map(Scale::as_120ths),
        }),
        SessionEvent::FocusAsk { id } => Body::FocusAsk(wire::FocusAsk {
            surface_id: id.get(),
        }),
        // A size the app took on its own is a fact, not a request: the frames that follow use
        // it, so the client learns it first, as an ack under serial 0, which no host serial
        // ever is (v0.md §4.2).
        SessionEvent::Resized { id, size } => Body::ConfigureAck(ConfigureAck {
            surface_id: id.get(),
            serial: 0,
            size: Some(wire::Size {
                width: size.width,
                height: size.height,
            }),
        }),
        // A resize the app only asked for: the host owns sizes (rule 6), and answering with
        // Configure is how it grants one.
        SessionEvent::ResizeAsk { id, size } => Body::ResizeAsk(ResizeAsk {
            surface_id: id.get(),
            size: Some(wire::Size {
                width: size.width,
                height: size.height,
            }),
        }),
        SessionEvent::CursorChanged { cursor } => Body::CursorImage(CursorImage {
            serial: cursor.serial,
            width: cursor.size.width,
            height: cursor.size.height,
            hotspot_x: u32::try_from(cursor.hotspot.x.max(0)).unwrap_or(0),
            hotspot_y: u32::try_from(cursor.hotspot.y.max(0)).unwrap_or(0),
            argb_premultiplied: cursor.argb.clone(),
        }),
        SessionEvent::CursorGone => Body::CursorGone(wire::CursorGone {}),
        SessionEvent::ClipboardAsk => Body::ClipboardAsk(wire::ClipboardAsk {}),
        // The app copied: untrusted text, already deduplicated and capped by the session.
        SessionEvent::ClipboardText { text } => {
            Body::ClipboardText(ClipboardText { text: text.clone() })
        }
        SessionEvent::ConfigureAcked { id, serial, size } => {
            // The ack answers the named configure and every older one still waiting: the
            // client hears the newest serial, and the older ones are done with.
            let client_serial = pump.configure_acks.get_mut(id).and_then(|waiting| {
                let at = waiting.iter().position(|(core, _)| *core == serial.get())?;
                let client = waiting[at].1;
                waiting.drain(..=at);
                Some(client)
            });
            Body::ConfigureAck(ConfigureAck {
                surface_id: id.get(),
                // No client serial names it (a proposal this connection never sent): the
                // size is still the surface's from here on, so it goes out like any size
                // the app took on its own, under serial 0.
                serial: client_serial.unwrap_or(0),
                size: Some(wire::Size {
                    width: size.width,
                    height: size.height,
                }),
            })
        }
    };
    send_body(sink, body).await
}

/// Plans, captures, encodes and sends one frame per surface that has damage and a free
/// credit.
///
/// Frames leave only when damage or a freed credit calls this — never a timer (v0.md §6).
async fn pump_frames<B>(sink: &mut WsSink, pump: &mut Pump<B>) -> Flow
where
    B: CaptureBackend + InputSink + Send + 'static,
{
    let ids: Vec<SurfaceId> = pump.surfaces.clone();
    for id in ids {
        let Some(plan) = pump.session.plan_frame(id) else {
            continue;
        };
        let Some(surface) = pump.session.surface(id) else {
            continue;
        };
        let bounds = surface.size();
        let cache = pump.sent_tiles.entry(id).or_default();
        match build_frame(&pump.backend, pump.prefer, cache, id, bounds, &plan).await {
            Ok(Some(frame)) => {
                if let Err(fault) = send_body(sink, Body::Frame(frame)).await {
                    return fault_flow(sink, fault).await;
                }
            }
            // Nothing to send: every tile was identical to what the client holds, every
            // capture failed (contract C2), or the damage vanished. A frame with no tile
            // never goes out and consumes no sequence number, and the plan goes back as if it
            // had never been made: its credit, its damage and any full redraw it owed. The
            // next damage or ack plans it again.
            Ok(None) => {
                pump.session.abort_frame(id, &plan);
            }
            Err(e) => {
                // Building a frame cannot fail in a correct process; if it does, the session
                // is lying about its own state and must not continue. The fault is this
                // server's, and the close says so.
                tracing::error!(surface = id.get(), error = %e, "frame build failed");
                return internal_fault(sink).await;
            }
        }
    }
    Flow::On
}

/// Captures and encodes one planned frame, omitting tiles the client already holds.
///
/// The plan's rectangles are unioned before cutting: one cut of the union always stays inside
/// `MAX_TILES_PER_FRAME` (a whole 1920x1200 surface is 40 tiles), where many rectangles cut
/// apart could pass the cap.
///
/// A tile is skipped, never fatal, when its capture fails (the surface died mid-frame), when
/// the buffer does not match the tile's rectangle (the surface changed size under the capture,
/// against the `CaptureBackend` contract), or when the encoder refuses the buffer: a tile on
/// the wire always carries exactly the pixels its rectangle names (contract C2). A tile
/// identical to what the client last received for its grid cell is skipped too (v0.md §4.3):
/// the client draws frames in sequence order, so its canvas already shows those pixels. The
/// event that explains a change follows on the feed. `Ok(None)` means no tile is left, and the
/// caller hands the plan back.
async fn build_frame<B>(
    backend: &BackendHandle<B>,
    prefer: Encoding,
    cache: &mut SentTiles,
    id: SurfaceId,
    bounds: Size,
    plan: &FramePlan,
) -> Result<Option<Frame>, FrameError>
where
    B: CaptureBackend + InputSink + Send + 'static,
{
    let Some(all) = plan.rects.iter().copied().reduce(Rect::union) else {
        return Ok(None); // damage vanished between planning and here
    };
    // A full redraw sends every tile and restarts the comparison; any other frame compares
    // against what was sent at the surface's current size, which a size change just dropped.
    if plan.full_redraw {
        cache.begin_full_redraw(bounds);
    } else {
        cache.note_size(bounds);
    }
    let mut tiles = Vec::new();
    for rect in cut_into_tiles(all, bounds) {
        let buffer = match backend.capture(id, rect).await {
            Ok(buffer) => buffer,
            Err(e) => {
                // A surface can die mid-frame; its surface-gone event follows on the feed.
                tracing::warn!(surface = id.get(), error = %e, "tile capture failed; tile skipped");
                continue;
            }
        };
        if buffer.size != rect.size {
            tracing::debug!(
                surface = id.get(),
                "captured buffer does not match its tile; tile skipped"
            );
            continue;
        }
        // The buffer is dropped right after: a RAW tile moves its pixels, never copies them.
        let encoded = match encode_tile_owned(buffer, prefer) {
            Ok(encoded) => encoded,
            Err(e) => {
                tracing::debug!(surface = id.get(), error = %e, "encoder refused a tile; tile skipped");
                continue;
            }
        };
        if !plan.full_redraw && cache.same_as_sent(rect, encoded.codec, &encoded.data) {
            // The client's canvas already shows this tile's pixels; sending it would change
            // nothing (v0.md §4.3).
            continue;
        }
        cache.record(rect, encoded.codec, &encoded.data);
        tiles.push(Tile {
            rect: Some(wire::Rect {
                x: rect.origin.x,
                y: rect.origin.y,
                width: rect.size.width,
                height: rect.size.height,
            }),
            codec: encoded.codec,
            data: encoded.data,
        });
    }
    if tiles.is_empty() {
        return Ok(None);
    }
    if tiles.len() > MAX_TILES_PER_FRAME {
        return Err(FrameError(
            "cut produced more tiles than one frame may carry".to_owned(),
        ));
    }
    Ok(Some(Frame {
        surface_id: id.get(),
        sequence: plan.sequence,
        full_redraw: plan.full_redraw,
        tiles,
    }))
}

/// A frame could not be built: a bug on this side, not a client fault.
#[derive(Debug)]
struct FrameError(String);

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Encodes and sends one body as one binary message.
///
/// An encode refusal is a bug on this side ([`SendFault::Encode`]): the peer gets a close
/// rather than a broken message. A socket error, or a send that does not finish within
/// [`SEND_DEADLINE`], means the peer is gone ([`SendFault::Socket`]).
async fn send_body(sink: &mut WsSink, body: Body) -> Result<(), SendFault> {
    let bytes = encode_envelope(&Envelope { body: Some(body) }).map_err(|e| {
        tracing::error!(error = %e, "encode refused an outbound message; this is a bug");
        SendFault::Encode
    })?;
    match tokio::time::timeout(SEND_DEADLINE, sink.send(Message::Binary(bytes.into()))).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => {
            tracing::debug!(error = %e, "socket send failed");
            Err(SendFault::Socket)
        }
        Err(_elapsed) => {
            tracing::warn!("a send stalled past its deadline; the peer is treated as gone");
            Err(SendFault::Socket)
        }
    }
}

/// Sends a Bye and closes.
async fn send_bye(sink: &mut WsSink, reason: ByeReason, text: &str) -> Result<(), ()> {
    let _ = send_body(
        sink,
        Body::Bye(Bye {
            reason: reason as i32,
            text: text.to_owned(),
        }),
    )
    .await;
    sink.close().await.map_err(|e| {
        tracing::debug!(error = %e, "socket close failed");
    })
}

/// Sends a ServerError, then the Bye, then closes.
async fn send_error_and_bye(
    sink: &mut WsSink,
    code: u32,
    text: &str,
    reason: ByeReason,
) -> Result<(), ()> {
    let _ = send_body(
        sink,
        Body::ServerError(ServerError {
            code,
            text: text.to_owned(),
        }),
    )
    .await;
    send_bye(sink, reason, "").await
}

/// Maps a model role onto the wire.
fn wire_role(role: Role) -> i32 {
    match role {
        Role::Toplevel => wire::Role::Toplevel as i32,
        Role::Popup { .. } => wire::Role::Popup as i32,
    }
}

/// Maps a gone reason onto the wire.
fn wire_gone_reason(reason: GoneReason) -> i32 {
    match reason {
        GoneReason::AppClosed => SurfaceGoneReason::GoneAppClosed as i32,
        GoneReason::ParentGone => SurfaceGoneReason::GoneParentGone as i32,
    }
}

/// Maps a positioner onto the wire.
fn wire_positioner(p: &Positioner) -> wire::Positioner {
    wire::Positioner {
        anchor_rect: Some(wire::Rect {
            x: p.anchor_rect.origin.x,
            y: p.anchor_rect.origin.y,
            width: p.anchor_rect.size.width,
            height: p.anchor_rect.size.height,
        }),
        anchor: wire_anchor(p.anchor),
        gravity: wire_anchor(p.gravity),
        offset: Some(wire::Point {
            x: p.offset.x,
            y: p.offset.y,
        }),
        size: Some(wire::Size {
            width: p.size.width,
            height: p.size.height,
        }),
    }
}

/// Maps an anchor onto the wire. The wire's numbers are `xdg_positioner`'s names; the model's
/// enum order is not, so every arm is spelled out.
fn wire_anchor(a: Anchor) -> i32 {
    match a {
        Anchor::Center => wire::Anchor::Center as i32,
        Anchor::Top => wire::Anchor::Top as i32,
        Anchor::Bottom => wire::Anchor::Bottom as i32,
        Anchor::Left => wire::Anchor::Left as i32,
        Anchor::Right => wire::Anchor::Right as i32,
        Anchor::TopLeft => wire::Anchor::TopLeft as i32,
        Anchor::BottomLeft => wire::Anchor::BottomLeft as i32,
        Anchor::TopRight => wire::Anchor::TopRight as i32,
        Anchor::BottomRight => wire::Anchor::BottomRight as i32,
    }
}

#[cfg(test)]
mod tests {
    use super::{ERR_DECODE, ERR_LIMIT, negotiate, pointer_button, press_state, refusal_for};

    use appricot_core::{PointerButton, PressState};
    use appricot_proto::limits::codec;
    use appricot_proto::wire::{Body, ByeReason, DecodeError, decode_envelope};

    #[test]
    fn an_empty_offer_means_raw() {
        assert_eq!(negotiate(&[]), super::Encoding::Raw);
    }

    #[test]
    fn the_first_known_codec_wins_in_client_order() {
        assert_eq!(negotiate(&[codec::QOI]), super::Encoding::Qoi);
        assert_eq!(negotiate(&[codec::RAW, codec::QOI]), super::Encoding::Raw);
        assert_eq!(negotiate(&[codec::QOI, codec::RAW]), super::Encoding::Qoi);
    }

    #[test]
    fn an_offer_of_nothing_known_falls_back_to_raw() {
        assert_eq!(negotiate(&[9, 7]), super::Encoding::Raw);
    }

    #[test]
    fn x_button_numbers_map_to_the_model() {
        assert_eq!(pointer_button(1), Some(PointerButton::Left));
        assert_eq!(pointer_button(2), Some(PointerButton::Middle));
        assert_eq!(pointer_button(3), Some(PointerButton::Right));
        assert_eq!(pointer_button(0), None);
        assert_eq!(pointer_button(4), None);
    }

    #[test]
    fn pressed_flags_map_to_press_states() {
        assert_eq!(press_state(true), PressState::Pressed);
        assert_eq!(press_state(false), PressState::Released);
    }

    #[test]
    fn an_envelope_naming_no_known_body_closes_without_a_reply() {
        // Field 26, wire type 2, empty: a message this version does not know. Field 25 was
        // this test's unknown message until v0 gained `clipboard_text` for it.
        let unknown = decode_envelope(&[0xd2, 0x01, 0x00]).expect_err("no body is refused");
        assert_eq!(
            refusal_for(&unknown),
            None,
            "v0.md §1: close without a reply"
        );
        // An empty `clipboard_text` — field 25 with no text — names a known message now, so
        // it decodes; the streamer answer for a client sending a server-to-client message is
        // the wrong-direction close (§5), not the no-reply close of an unknown one.
        let known = decode_envelope(&[0xca, 0x01, 0x00]).expect("field 25 is clipboard_text");
        let Body::ClipboardText(message) = known.body.expect("a oneof body is present") else {
            panic!("field 25 decodes as clipboard_text");
        };
        assert_eq!(message.text, "");
        let empty = decode_envelope(&[]).expect_err("an empty envelope names no body");
        assert_eq!(refusal_for(&empty), None);
    }

    #[test]
    fn other_decode_failures_keep_their_codes() {
        assert_eq!(
            refusal_for(&DecodeError::TooLarge),
            Some((ERR_LIMIT, ByeReason::ByeLimitViolation))
        );
        assert_eq!(
            refusal_for(&DecodeError::LimitViolation {
                field: "hello.stream_token"
            }),
            Some((ERR_LIMIT, ByeReason::ByeLimitViolation))
        );
        let garbage = decode_envelope(&[0xff, 0xff, 0xff, 0x7f, 0x00, 0x01])
            .expect_err("garbage does not decode");
        assert_eq!(
            refusal_for(&garbage),
            Some((ERR_DECODE, ByeReason::ByeProtocolViolation))
        );
    }
}
