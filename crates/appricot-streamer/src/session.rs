//! The session pump: the v0 protocol over one WebSocket.
//!
//! [`run_session`] owns one socket, the [`Session`] state machine and the
//! [`BackendHandle`] for the connection's lifetime, in one `select` loop over the socket and
//! the backend's feed. Nothing here touches a clock except the resume grace
//! (docs/protocol/v0.md §7: "the one sanctioned timer in the whole protocol"), which is
//! session management — frame pacing is the client's acks driving [`Session::plan_frame`],
//! never a tick (§6: "nothing on a timer").
//!
//! # Handshake
//!
//! The first binary message must be a `Hello` carrying the stream token (rule 8,
//! docs/protocol/v0.md §5). Text frames, undecodable bytes, wrong messages, bad tokens and
//! unsupported versions each close with the `Bye` the spec names. Codecs are negotiated to
//! the first offered codec this server can encode — QOI before RAW when the client's order
//! says so; an empty offer means RAW, as the spec fixes.
//!
//! # One session, one resume
//!
//! A dropped socket parks the session (backend and all) for the grace window; a reconnect
//! whose `Hello` names the parked `resume_serial` resumes it — the whole window set is
//! re-announced and every surface gets a full-redraw frame. An authenticated `Hello` that
//! does **not** name it replaces the parked session with a fresh one (`resumed = false`):
//! the spec leaves this to the implementation's policy (§7), and a client that wants a new
//! session "simply sends Hello without resume_serial". `BYE_SESSION_GONE` is answered only
//! when there is nothing left to serve: the backend died with the display connection.
//!
//! Keysyms pass through untouched: on the wire a printable key's keysym is its Unicode
//! codepoint and a non-printing key's is an X11 keysym (docs/protocol/v0.md §8), and the
//! backend owns the resolution — the streamer never rewrites one.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket};
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};

use appricot_core::{
    Anchor, CaptureBackend, FramePlan, GoneReason, InputSink, KeyCode, KeyEvent, Keysym, Point,
    PointerButton, Positioner, PressState, Rect, Role, Scale, Session, SessionEvent, Size,
    SurfaceId,
};
use appricot_encode::{Encoding, cut_into_tiles, encode_tile};
use appricot_proto::PROTOCOL_VERSION;
use appricot_proto::limits::{MAX_FRAME_CREDITS, MAX_TILES_PER_FRAME, codec};
use appricot_proto::wire::{
    self, Body, Bye, ByeReason, ConfigureAck, CursorImage, DecodeError, Envelope, Frame,
    HelloReply, ResizeAsk, ServerError, SurfaceGone, SurfaceGoneReason, SurfaceMetadata,
    SurfaceNew, Tile, decode_envelope, encode_envelope,
};

use crate::auth::token_matches;
use crate::backend::{BackendError, BackendHandle, Feed};

/// The sink half of the session's WebSocket.
type WsSink = SplitSink<WebSocket, Message>;
/// The stream half of the session's WebSocket.
type WsStream = SplitStream<WebSocket>;

/// Everything a session needs from the server process.
#[derive(Debug)]
pub struct SessionConfig {
    /// The stream token the first `Hello` must carry.
    pub token: Arc<Vec<u8>>,
}

/// How the pump starts: with a bare backend, or resuming a parked session.
#[derive(Debug)]
pub enum Start<B> {
    /// No session ever ran on this backend.
    Fresh(BackendHandle<B>),
    /// A parked session waits out its grace; its `Hello` decides resume or replace.
    Resume(ParkedSession<B>),
}

/// What the pump hands back when its socket is done.
#[derive(Debug)]
pub enum SessionEnd<B> {
    /// The socket died without a closing `Bye`: the session is parked for its grace, backend
    /// included, and the caller starts the grace keeper.
    Parked(ParkedSession<B>),
    /// The session is over (a `Bye` was exchanged, or the display died); the backend was
    /// dropped with it and nothing can be served again.
    Ended,
    /// The handshake was refused — a bad token, an unsupported version, or no `Hello` at all —
    /// so no session ever existed and nothing was claimed. The caller puts the backend back and
    /// the next client may try again: a mistyped token costs a retry, not the process.
    Refused(Start<B>),
}

/// A session that outlives its socket, waiting out the resume grace.
///
/// While parked, [`ParkedSession::absorb_feed`] keeps the display's events flowing into the
/// session (state stays current and the feed channel stays bounded); the keeper task in
/// [`crate::server`] calls it and tears the session down when the grace expires.
#[derive(Debug)]
pub struct ParkedSession<B> {
    session: Session,
    backend: BackendHandle<B>,
    /// The serial a reconnecting `Hello` must name.
    pub(crate) resume_serial: u32,
    /// The grace window, from [`RESUME_GRACE_MS`].
    pub(crate) grace: Duration,
    /// Set when the display died while parked; a resume meets `BYE_SERVER_SHUTDOWN`.
    pub(crate) fatal: Option<BackendError>,
}

impl<B> ParkedSession<B> {
    /// Applies whatever the backend produced while parked; outputs are discarded — the
    /// client, when it comes back, is resynchronised by the resume, not by a replay.
    pub fn absorb_feed(&mut self) {
        for feed in self.backend.drain_feed() {
            match feed {
                Feed::Events(events) => {
                    let mut discard = Vec::new();
                    for event in events {
                        self.session.apply_event(event, &mut discard);
                    }
                }
                Feed::Fatal(e) => self.fatal = Some(e),
            }
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

/// Runs one session from upgrade to socket end.
///
/// A client fault never panics and never fails the process: every violation answers with the
/// `ServerError` and `Bye` the spec names and closes. A parked session is offered only to a
/// socket that died without saying why.
pub async fn run_session<B>(
    socket: WebSocket,
    config: SessionConfig,
    start: Start<B>,
) -> SessionEnd<B>
where
    B: CaptureBackend + InputSink + Send + 'static,
{
    let (mut sink, mut stream) = socket.split();

    let mut pump = match start {
        Start::Fresh(backend) => Pump {
            session: Session::new(),
            backend,
            surfaces: Vec::new(),
            configure_acks: HashMap::new(),
            prefer: Encoding::Raw,
            parked_resume_serial: 0,
            parked_fatal: None,
        },
        Start::Resume(parked) => {
            let serial = parked.resume_serial;
            let fatal = parked.fatal;
            Pump {
                session: parked.session,
                backend: parked.backend,
                // The resume re-announces the window set; the mirror is rebuilt from it.
                surfaces: Vec::new(),
                configure_acks: HashMap::new(),
                prefer: Encoding::Raw,
                parked_resume_serial: serial,
                parked_fatal: fatal,
            }
        }
    };

    match handshake(&mut sink, &mut stream, &config, &mut pump).await {
        // Nothing was claimed: the session begins with an accepted `Hello`, so a refused
        // handshake hands the backend straight back (see `SessionEnd::Refused`).
        Handshake::Rejected => SessionEnd::Refused(unclaim(pump)),
        Handshake::Accepted {
            resume_serial,
            session_id,
        } => {
            // A resume marks every surface for a full redraw inside the handshake; nothing on
            // the feed will call for a frame afterwards, so the first pump happens here. For
            // a fresh session this plans nothing (no surfaces live yet).
            if pump_frames(&mut sink, &mut pump).await == Flow::Stop {
                return SessionEnd::Ended;
            }
            steady(&mut sink, &mut stream, pump, resume_serial, session_id).await
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
    /// The codec the encoder prefers this session (RAW is the fallback whatever this says).
    prefer: Encoding,
    /// The serial a parked session was answering with when its socket died; 0 on a fresh
    /// start, meaning nothing can be resumed.
    parked_resume_serial: u32,
    /// Set when resuming a session whose display died while parked.
    parked_fatal: Option<BackendError>,
}

/// Hands an unclaimed backend back after a refused handshake.
///
/// A fresh backend is still fresh — no session ever touched it. A parked session is still
/// parked, and its session state is untouched: the refusal happened before the authenticated
/// `Hello` that is the only thing able to change it. The grace is re-armed in full, so a
/// client that keeps refusing handshakes extends a parked session by one window per attempt;
/// that is bounded by the same rate as any other request and buys the retry that matters.
fn unclaim<B>(pump: Pump<B>) -> Start<B>
where
    B: CaptureBackend + InputSink + Send + 'static,
{
    if pump.parked_resume_serial == 0 {
        Start::Fresh(pump.backend)
    } else {
        Start::Resume(ParkedSession {
            session: pump.session,
            backend: pump.backend,
            resume_serial: pump.parked_resume_serial,
            grace: Duration::from_millis(u64::from(crate::config::resume_grace_ms())),
            fatal: pump.parked_fatal,
        })
    }
}

/// The outcome of the auth gate.
enum Handshake {
    /// The socket closed with the refusal `Bye` already sent; the session is over.
    Rejected,
    /// The handshake succeeded.
    Accepted {
        /// The serial a future reconnect must name.
        resume_serial: u32,
        /// The session id for logs and support.
        session_id: String,
    },
}

/// Reads the first message and answers the `Hello`, or closes with the refusal the spec names.
async fn handshake(
    sink: &mut WsSink,
    stream: &mut WsStream,
    config: &SessionConfig,
    pump: &mut Pump<impl CaptureBackend + InputSink + Send + 'static>,
) -> Handshake {
    let bytes = loop {
        match stream.next().await {
            // A dead socket and a close frame are the same refusal: no Hello arrived.
            None | Some(Err(_) | Ok(Message::Close(_))) => return Handshake::Rejected,
            // Pings are answered by the socket itself; pongs carry nothing for us.
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
            // No text frames, ever (rule 2); the first message must be binary.
            Some(Ok(Message::Text(_))) => {
                let _ = send_bye(sink, ByeReason::ByeProtocolViolation, "binary frames only").await;
                return Handshake::Rejected;
            }
            Some(Ok(Message::Binary(bytes))) => break bytes.to_vec(),
        }
    };

    let hello = match decode_envelope(&bytes) {
        Ok(Envelope {
            body: Some(Body::Hello(hello)),
        }) => hello,
        Ok(_) => {
            let _ = send_bye(sink, ByeReason::ByeProtocolViolation, "expected Hello").await;
            return Handshake::Rejected;
        }
        Err(e) => return decode_refusal(sink, &e).await,
    };

    // Authenticate before anything else is even looked at.
    if !token_matches(&config.token, &hello.stream_token) {
        let _ = send_bye(sink, ByeReason::ByeAuthFailed, "").await;
        return Handshake::Rejected;
    }

    // Version: refuse what we do not speak; never guess (rule 1).
    if hello.protocol_version != u32::from(PROTOCOL_VERSION) {
        let _ = send_bye(
            sink,
            ByeReason::ByeProtocolVersion,
            &format!("server speaks v{PROTOCOL_VERSION}"),
        )
        .await;
        return Handshake::Rejected;
    }

    // Codecs: answer the first offered codec the encoder can produce.
    pump.prefer = negotiate(&hello.codecs);

    // Resume or replace. Serials are process-unique, so a fresh session never collides with
    // the serial of the one it replaced.
    let number = SESSION_COUNTER.fetch_add(1, Ordering::Relaxed);
    let resume_serial = number;
    let session_id = format!("session-{number}");
    let resumed =
        hello.resume_serial == Some(pump.parked_resume_serial) && pump.parked_resume_serial != 0;

    // A parked session whose display died meets the shutdown Bye, not a resume.
    if resumed && pump.parked_fatal.is_some() {
        let _ = send_bye(
            sink,
            ByeReason::ByeServerShutdown,
            "the display died while parked",
        )
        .await;
        return Handshake::Rejected;
    }

    // A Hello that does not name the parked serial replaces the session (v0.md §7). The
    // parked core session died with its client: it is detached, and a detached session emits
    // nothing and plans no frames — so the replacement starts from a fresh one on the same
    // backend, or the new client would hold a socket that never tells it a thing.
    if !resumed && pump.parked_resume_serial != 0 {
        pump.session = Session::new();
    }

    let reply = HelloReply {
        protocol_version: u32::from(PROTOCOL_VERSION),
        session_id: session_id.clone(),
        // The limits table keeps both far below any integer width that matters; try_from is
        // only here to say so in the type, not because it can fail.
        max_frame_credits: u32::try_from(MAX_FRAME_CREDITS).expect("fits the limits table"),
        codecs: match pump.prefer {
            Encoding::Qoi => vec![codec::RAW, codec::QOI],
            // RAW is the floor; a codec added later still leaves RAW as the must-decode.
            _ => vec![codec::RAW],
        },
        resumed,
        resume_serial: Some(resume_serial),
        // What the server will actually honour: the test/demo knob overrides the wait and the
        // advertisement together, so the reply never promises a grace it will not keep
        // (crate::config::resume_grace_ms).
        resume_grace_ms: Some(crate::config::resume_grace_ms()),
    };
    if send_body(sink, Body::HelloReply(reply)).await.is_err() {
        return Handshake::Rejected;
    }

    // A resume re-announces the whole window set; the steady loop then plans one full-redraw
    // frame per surface, because the resume reset every surface's pacing.
    if resumed {
        let mut out = Vec::new();
        pump.session.resume(&mut out);
        for event in &out {
            if send_session_event(sink, pump, event).await.is_err() {
                return Handshake::Rejected;
            }
        }
    }

    Handshake::Accepted {
        resume_serial,
        session_id,
    }
}

/// Applies one feed item: events into the session, then whatever frames they call for.
///
/// [`Flow::Stop`] means the session is over — the display died, or the wire failed.
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
        // The display died: nothing can be served again (no park; the backend is gone).
        None | Some(Feed::Fatal(_)) => {
            let _ = send_bye(sink, ByeReason::ByeServerShutdown, "the display died").await;
            tracing::error!(session = %session_id, "display dead; session ended");
            Flow::Stop
        }
        Some(Feed::Events(events)) => {
            let mut out = Vec::new();
            for event in events {
                pump.session.apply_event(event, &mut out);
            }
            if emit_events(sink, pump, &out).await == Flow::Stop {
                return Flow::Stop;
            }
            pump_frames(sink, pump).await
        }
    }
}

/// The steady state: one select over the socket and the backend's feed.
async fn steady<B>(
    sink: &mut WsSink,
    stream: &mut WsStream,
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
        // trailing lag 168 ms). That is the delay a user feels as a lagging pointer. A feed
        // batch is finite, so the socket is served as soon as the batch is applied.
        let ready = pump.backend.drain_feed();
        if !ready.is_empty() {
            for feed in ready {
                if apply_feed(Some(feed), sink, &mut pump, &session_id).await == Flow::Stop {
                    return SessionEnd::Ended;
                }
            }
            continue;
        }

        let from_client = tokio::select! {
            biased;
            message = stream.next() => message,
            feed = pump.backend.next_feed() => {
                if apply_feed(feed, sink, &mut pump, &session_id).await == Flow::Stop {
                    return SessionEnd::Ended;
                }
                continue;
            }
        };

        match from_client {
            // The socket died without a Bye: park the session for its grace (v0.md §7).
            None | Some(Err(_)) => {
                pump.session.detach();
                tracing::info!(session = %session_id, "socket gone; session parked for the grace");
                return SessionEnd::Parked(ParkedSession {
                    session: pump.session,
                    backend: pump.backend,
                    resume_serial,
                    grace: Duration::from_millis(u64::from(crate::config::resume_grace_ms())),
                    fatal: None,
                });
            }
            Some(Ok(message)) => match message {
                Message::Binary(bytes) => {
                    if handle_client_bytes(sink, &mut pump, &bytes).await == Flow::Stop {
                        return SessionEnd::Ended;
                    }
                }
                // Pings are answered by the socket itself; pongs carry nothing for us.
                Message::Ping(_) | Message::Pong(_) => {}
                Message::Close(_) => {
                    // A close frame, not a Bye: the client is gone without a word. Parked,
                    // like any dropped socket — the grace decides what happens next.
                    pump.session.detach();
                    tracing::info!(session = %session_id, "socket closed; session parked");
                    return SessionEnd::Parked(ParkedSession {
                        session: pump.session,
                        backend: pump.backend,
                        resume_serial,
                        grace: Duration::from_millis(u64::from(crate::config::resume_grace_ms())),
                        fatal: None,
                    });
                }
                // No text frames, ever (rule 2).
                Message::Text(_) => {
                    let _ =
                        send_bye(sink, ByeReason::ByeProtocolViolation, "binary frames only").await;
                    return SessionEnd::Ended;
                }
            },
        }
    }
}

/// Whether the steady loop carries on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flow {
    /// Keep going.
    On,
    /// The session is done; the caller returns.
    Stop,
}

/// Answers a decode failure with the ServerError and Bye the spec names.
async fn decode_refusal(sink: &mut WsSink, e: &DecodeError) -> Handshake {
    let (code, reason) = match e {
        DecodeError::TooLarge | DecodeError::LimitViolation { .. } => {
            (ERR_LIMIT, ByeReason::ByeLimitViolation)
        }
        DecodeError::Decode(_) => (ERR_DECODE, ByeReason::ByeProtocolViolation),
    };
    let _ = send_error_and_bye(sink, code, &e.to_string(), reason).await;
    Handshake::Rejected
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
    let envelope = match decode_envelope(bytes) {
        Ok(envelope) => envelope,
        Err(e) => {
            let (code, reason) = match &e {
                DecodeError::Decode(_) => (ERR_DECODE, ByeReason::ByeProtocolViolation),
                _ => (ERR_LIMIT, ByeReason::ByeLimitViolation),
            };
            let _ = send_error_and_bye(sink, code, &e.to_string(), reason).await;
            return Err(Flow::Stop);
        }
    };
    if let Some(body) = envelope.body {
        Ok(body)
    } else {
        let _ = send_bye(sink, ByeReason::ByeProtocolViolation, "empty envelope").await;
        Err(Flow::Stop)
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
        if emit_events(sink, pump, &out).await == Flow::Stop {
            return Flow::Stop;
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
/// connection.
async fn deliver_key<B>(pump: &mut Pump<B>, m: wire::Key) -> Flow
where
    B: CaptureBackend + InputSink + Send + 'static,
{
    let Some(code) = KeyCode::new(&m.code) else {
        tracing::debug!(code = %m.code, "key with an invalid physical code ignored");
        return Flow::On;
    };
    if m.modifiers != 0 {
        // v0 logs modifiers and acts on none of them.
        tracing::trace!(modifiers = m.modifiers, "key modifiers");
    }
    let key = KeyEvent {
        keysym: Keysym(m.keysym),
        code: Some(code),
        state: press_state(m.pressed),
    };
    if let Err(e) = pump.backend.key(key).await {
        tracing::warn!(error = %e, "key delivery failed");
    }
    Flow::On
}

/// Handles one binary message from the client in the steady state.
async fn handle_client_bytes<B>(sink: &mut WsSink, pump: &mut Pump<B>, bytes: &[u8]) -> Flow
where
    B: CaptureBackend + InputSink + Send + 'static,
{
    let Ok(body) = decode_client_message(sink, bytes).await else {
        return Flow::Stop;
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
            if let Err(e) = pump
                .backend
                .pointer_motion(SurfaceId::new(m.surface_id), Point::new(m.x, m.y))
                .await
            {
                tracing::warn!(error = %e, "pointer motion delivery failed");
            }
            Flow::On
        }
        Body::PointerButton(m) => {
            let Some(button) = pointer_button(m.button) else {
                return Flow::On; // not an X button: ignore, never fatal
            };
            let state = press_state(m.pressed);
            if let Err(e) = pump
                .backend
                .pointer_button(SurfaceId::new(m.surface_id), button, state)
                .await
            {
                tracing::warn!(error = %e, "pointer button delivery failed");
            }
            Flow::On
        }
        Body::PointerAxis(m) => {
            if let Err(e) = pump
                .backend
                .pointer_axis(
                    SurfaceId::new(m.surface_id),
                    Point::new(m.steps_x, m.steps_y),
                )
                .await
            {
                tracing::warn!(error = %e, "pointer axis delivery failed");
            }
            Flow::On
        }
        Body::Key(m) => deliver_key(pump, m).await,
        Body::FocusNotify(m) => {
            if let Err(e) = pump.backend.focus(SurfaceId::new(m.surface_id)).await {
                tracing::warn!(error = %e, "focus delivery failed");
            }
            Flow::On
        }
        Body::BlurRelease(_) => {
            if let Err(e) = pump.backend.blur().await {
                tracing::warn!(error = %e, "blur delivery failed");
            }
            Flow::On
        }
        Body::ClipboardSet(m) => {
            // The codec already capped the text at MAX_CLIPBOARD_BYTES before this ran.
            if let Err(e) = pump.backend.clipboard_set(m.text).await {
                tracing::warn!(error = %e, "clipboard delivery failed");
            }
            Flow::On
        }
        Body::CloseRequest(m) => {
            let id = SurfaceId::new(m.surface_id);
            if pump.session.close_request(id)
                && let Err(e) = pump.backend.close(id).await
            {
                tracing::warn!(error = %e, "close request delivery failed");
            }
            Flow::On
        }
        // The client said Bye: the clean end. Answer in kind, close, and the session ends
        // with the backend — nothing is parked for a client that said goodbye.
        Body::Bye(_) => {
            let _ = send_bye(sink, ByeReason::ByePeerClosed, "").await;
            Flow::Stop
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
            Flow::Stop
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
        if send_session_event(sink, pump, event).await.is_err() {
            return Flow::Stop;
        }
    }
    Flow::On
}

/// Maps one session event onto the wire and sends it.
///
/// A mapping that cannot be encoded is a bug in this crate: it closes the socket rather than
/// emit a broken message.
async fn send_session_event<B>(
    sink: &mut WsSink,
    pump: &mut Pump<B>,
    event: &SessionEvent,
) -> Result<(), ()>
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
        match build_frame(pump, id, bounds, plan).await {
            Ok(Some(frame)) => {
                if send_body(sink, Body::Frame(frame)).await.is_err() {
                    return Flow::Stop;
                }
            }
            // Nothing to send (the damage vanished or every tile's surface died mid-frame).
            Ok(None) => {}
            Err(e) => {
                // Building a frame cannot fail in a correct process; if it does, the session
                // is lying about its own state and must not continue.
                tracing::error!(surface = id.get(), error = %e, "frame build failed");
                let _ = send_bye(
                    sink,
                    ByeReason::ByeProtocolViolation,
                    "internal frame error",
                )
                .await;
                return Flow::Stop;
            }
        }
    }
    Flow::On
}

/// Captures and encodes one planned frame.
///
/// The plan's rectangles are unioned before cutting: one cut of the union always stays inside
/// `MAX_TILES_PER_FRAME` (a whole 1920x1200 surface is 40 tiles), where many rectangles cut
/// apart could pass the cap.
async fn build_frame<B>(
    pump: &Pump<B>,
    id: SurfaceId,
    bounds: Size,
    plan: FramePlan,
) -> Result<Option<Frame>, FrameError>
where
    B: CaptureBackend + InputSink + Send + 'static,
{
    let Some(all) = plan.rects.iter().copied().reduce(Rect::union) else {
        return Ok(None); // damage vanished between planning and here
    };
    let mut tiles = Vec::new();
    for rect in cut_into_tiles(all, bounds) {
        let buffer = match pump.backend.capture(id, rect).await {
            Ok(buffer) => buffer,
            Err(e) => {
                // A surface can die mid-frame; its surface-gone event follows on the feed.
                tracing::warn!(surface = id.get(), error = %e, "tile capture failed; tile skipped");
                continue;
            }
        };
        let encoded = encode_tile(&buffer, pump.prefer)
            .map_err(|e| FrameError(format!("encode refused a captured tile: {e}")))?;
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
/// Fails (and the caller closes) when the message would break the limits table: an encode
/// error is a bug on this side, and the peer gets a close rather than a broken message.
async fn send_body(sink: &mut WsSink, body: Body) -> Result<(), ()> {
    let bytes = encode_envelope(&Envelope { body: Some(body) }).map_err(|e| {
        tracing::error!(error = %e, "encode refused an outbound message; this is a bug");
    })?;
    sink.send(Message::Binary(bytes.into())).await.map_err(|e| {
        tracing::debug!(error = %e, "socket send failed");
    })
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
    use super::{negotiate, pointer_button, press_state};

    use appricot_core::{PointerButton, PressState};
    use appricot_proto::limits::codec;

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
}
