//! The HTTP shell: readiness, and one WebSocket per process.
//!
//! Routes:
//!
//! - `GET /readyz` — `200 ready` once the backend is connected and its extension probe passed,
//!   for as long as the process can still serve its session; `503 starting` before that, and
//!   `503 gone` once the session is over. Readiness is latched by the caller that owns the
//!   backend. The route carries no token, deliberately: it answers one bit and nothing of the
//!   session, and a token check would put the secret into every health probe's configuration.
//!   That is an open gap against rule 2 of docs/security/threat-model.md §4.2, recorded there
//!   until L2's readiness gate exists.
//! - `GET /session` — a WebSocket upgrade speaking the v0 protocol. Binary frames only; the
//!   first message must be the authenticated `Hello` (see [`crate::session`]). One message, and
//!   one frame, may carry at most `MAX_MESSAGE_BYTES`: the WebSocket layer enforces the wire's
//!   cap itself, before anything is assembled or decoded.
//!
//! **One session per process.** While a session's socket is live, a second upgrade is refused
//! with `503`. When a session's socket dies without a closing `Bye`, the session (and the
//! backend under it) is parked for the resume grace; an upgrade during the grace resumes it
//! when the `Hello` names the stored `resume_serial`, and replaces it otherwise. After a clean
//! end, an expired grace or a display failure the backend is torn down, every further upgrade
//! is refused with `410`, readiness reads red, and [`serve`] returns so the process can exit:
//! the display connection belongs to the one session this process serves.
//!
//! The session is claimed by an **authenticated `Hello`**, not by the upgrade. The slot is taken
//! before the upgrade only so two upgraders cannot both proceed; a handshake the server refuses
//! (a bad token, an unsupported version, no `Hello` within the handshake deadline) hands it
//! straight back with the park's grace deadline unchanged, so a mistyped token costs a retry
//! rather than the process, a silent socket holds the slot for the deadline at most, and no
//! number of refusals extends a parked session's life. An upgrade that fails before the session
//! begins hands the slot back too, and a session task that dies without settling leaves the
//! slot dead rather than live forever (the private `Claim` guard).
//!
//! **One keeper per process** watches whatever no socket serves: it drains the display's events
//! into the idle or parked session (so the state stays current and the feed channel stays
//! bounded), tears a parked session down when its grace deadline passes, and ends the process's
//! session when the display dies with nobody attached.
//!
//! The server binds `127.0.0.1` or a unix socket, nothing else; [`crate::config::parse_bind`]
//! enforces that before any listener exists.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::extract::ws::{WebSocket, WebSocketUpgrade};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio::time::Instant;

use appricot_core::{CaptureBackend, InputSink};
use appricot_proto::limits::MAX_MESSAGE_BYTES;

use crate::auth::StreamToken;
use crate::backend::BackendHandle;
use crate::config::Bind;
use crate::session::{self, EndCause, ParkedSession, SessionConfig, SessionEnd, Standby, Start};

/// How often the keeper wakes to drain the display into a session nobody is attached to.
const KEEPER_TICK: Duration = Duration::from_millis(50);

/// The shared state of the serving process.
#[derive(Debug)]
pub struct ServerState<B> {
    token: Arc<StreamToken>,
    ready: Arc<AtomicBool>,
    slot: Arc<Mutex<Slot<B>>>,
    /// Turns true when a stop is asked for; every session watches it.
    stop: watch::Sender<bool>,
    /// Why the one session is over, once it is; [`serve`] returns on it.
    over: watch::Sender<Option<EndCause>>,
    /// Whether the keeper task was started.
    keeper: AtomicBool,
}

/// What the one session slot holds.
#[derive(Debug)]
pub enum Slot<B> {
    /// The backend is connected and no client has claimed it: the first authenticated `Hello`
    /// takes it. The keeper drains the display into the standby session meanwhile.
    Idle(Standby<B>),
    /// A socket holds the slot: a handshake in progress, or a session pump running.
    Live,
    /// The session lost its socket and waits out its resume grace.
    Parked(ParkedSession<B>),
    /// The backend is gone (clean end, expired grace, a display failure, or a stop). Refused
    /// from here on; a new display connection needs a new process.
    Dead,
}

impl<B> ServerState<B>
where
    B: CaptureBackend + InputSink + Send + 'static,
{
    /// Builds the state around a connected backend.
    ///
    /// Readiness starts red; the caller latches it green once the extension probe has passed,
    /// through [`ServerState::set_ready`] or by cloning [`ServerState::ready_flag`]. Called
    /// inside a Tokio runtime, this starts the keeper task at once; otherwise the keeper starts
    /// with [`serve`] or with the first upgrade.
    pub fn new(token: impl Into<StreamToken>, backend: BackendHandle<B>) -> Arc<Self> {
        let state = Arc::new(Self {
            token: Arc::new(token.into()),
            ready: Arc::new(AtomicBool::new(false)),
            slot: Arc::new(Mutex::new(Slot::Idle(Standby::new(backend)))),
            stop: watch::Sender::new(false),
            over: watch::Sender::new(None),
            keeper: AtomicBool::new(false),
        });
        state.ensure_keeper();
        state
    }

    /// Starts the keeper task unless it runs already; needs a Tokio runtime, and waits for the
    /// next call when there is none.
    fn ensure_keeper(self: &Arc<Self>) {
        if self.keeper.swap(true, Ordering::AcqRel) {
            return;
        }
        match tokio::runtime::Handle::try_current() {
            Ok(runtime) => {
                runtime.spawn(keeper(Arc::clone(self)));
            }
            Err(_) => self.keeper.store(false, Ordering::Release),
        }
    }

    /// Stops the server: a live session answers `Bye(BYE_SERVER_SHUTDOWN)` and ends, an idle
    /// or parked one is torn down, readiness reads red and [`serve`] returns.
    ///
    /// This is the path a termination signal takes once one is wired to it.
    pub fn shutdown(&self) {
        self.stop.send_replace(true);
        let mut slot = self.lock_slot();
        if matches!(*slot, Slot::Idle(_) | Slot::Parked(_)) {
            self.go_dead(&mut slot, EndCause::Clean);
        }
    }

    /// The session configuration one upgrade runs with.
    fn session_config(&self) -> SessionConfig {
        SessionConfig {
            token: Arc::clone(&self.token),
            stop: self.stop.subscribe(),
        }
    }

    /// Puts what a session pump handed back into the slot.
    fn settle(&self, end: SessionEnd<B>) {
        let mut slot = self.lock_slot();
        let stopping = *self.stop.borrow();
        match end {
            SessionEnd::Ended(cause) => self.go_dead(&mut slot, cause),
            // A stop asked for while this socket ran: nothing is kept for later.
            SessionEnd::Parked(_) | SessionEnd::Refused(_) if stopping => {
                self.go_dead(&mut slot, EndCause::Clean);
            }
            // A socket gone, or a refused handshake: no session was ever claimed, so the start
            // goes back where it came from and the next client is served instead of meeting a
            // dead process. Without this, the upgrade alone claimed the one session — a
            // mistyped token, a port scan or a probe bricked the streamer for good (measured
            // 2026-09-21). A park comes back with its original deadline; the keeper still ends
            // it on time.
            SessionEnd::Parked(parked) | SessionEnd::Refused(Start::Resume(parked)) => {
                *slot = Slot::Parked(parked);
            }
            SessionEnd::Refused(Start::Fresh(standby)) => *slot = Slot::Idle(standby),
        }
    }

    /// One pass of the keeper: drain the display into whatever nobody is attached to, and end
    /// what must end. Returns when to wake next, or `None` once the session is over.
    fn keep(&self) -> Option<Instant> {
        /// What one pass decided, once the borrow of the slot's content is over.
        enum Next {
            Wake(Instant),
            End(EndCause),
            Done,
        }

        let now = Instant::now();
        let mut slot = self.lock_slot();
        let next = match &mut *slot {
            Slot::Idle(standby) => {
                standby.absorb_feed();
                if standby.display_died() {
                    tracing::error!("the display died with no client attached; nothing to serve");
                    Next::End(EndCause::Fault)
                } else {
                    Next::Wake(now + KEEPER_TICK)
                }
            }
            Slot::Parked(parked) => {
                parked.absorb_feed();
                if now >= parked.deadline() {
                    tracing::info!("the resume grace expired; the parked session is torn down");
                    // A display that died while parked stays parked to answer a resume with
                    // BYE_SESSION_GONE; at the deadline it ends like any other park.
                    Next::End(if parked.display_died() {
                        EndCause::Fault
                    } else {
                        EndCause::Clean
                    })
                } else {
                    Next::Wake((now + KEEPER_TICK).min(parked.deadline()))
                }
            }
            // The pump owns the backend while a socket holds the slot.
            Slot::Live => Next::Wake(now + KEEPER_TICK),
            Slot::Dead => Next::Done,
        };
        match next {
            Next::Wake(at) => Some(at),
            Next::End(cause) => {
                self.go_dead(&mut slot, cause);
                None
            }
            Next::Done => None,
        }
    }
}

impl<B> ServerState<B> {
    /// The readiness flag, for the caller that owns the backend to set.
    pub fn ready_flag(self: &Arc<Self>) -> Arc<AtomicBool> {
        Arc::clone(&self.ready)
    }

    /// Latches readiness on.
    pub fn set_ready(&self) {
        self.ready.store(true, Ordering::Release);
    }

    /// Why the one session is over, or `None` while the process can still serve it.
    pub fn outcome(&self) -> Option<EndCause> {
        *self.over.borrow()
    }

    /// Resolves once the one session is over.
    pub async fn until_over(&self) {
        let mut over = self.over.subscribe();
        // The sender lives in `self`, so the wait cannot lose it.
        let _ = over.wait_for(Option::is_some).await;
    }

    /// Locks the slot. A panic elsewhere while it was held (core code applying a hostile app's
    /// events, say) does not poison it for good: the state inside is still the slot's, and the
    /// guards in this module decide what it becomes.
    fn lock_slot(&self) -> MutexGuard<'_, Slot<B>> {
        self.slot.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Ends the one session for good: the backend drops with the slot's content, readiness
    /// reads red, and [`serve`] returns.
    fn go_dead(&self, slot: &mut Slot<B>, cause: EndCause) {
        *slot = Slot::Dead;
        self.over.send_if_modified(|over| {
            if over.is_some() {
                return false;
            }
            *over = Some(cause);
            true
        });
    }
}

/// The routes, with the state that owns the backend.
pub fn router<B>() -> Router<Arc<ServerState<B>>>
where
    B: CaptureBackend + InputSink + Send + 'static,
{
    Router::new()
        .route("/readyz", get(readyz))
        .route("/session", get(session_upgrade::<B>))
}

async fn readyz<B>(State(state): State<Arc<ServerState<B>>>) -> Response {
    if !state.ready.load(Ordering::Acquire) {
        return (StatusCode::SERVICE_UNAVAILABLE, "starting").into_response();
    }
    if matches!(*state.lock_slot(), Slot::Dead) {
        return (StatusCode::SERVICE_UNAVAILABLE, "gone").into_response();
    }
    (StatusCode::OK, "ready").into_response()
}

async fn session_upgrade<B>(
    State(state): State<Arc<ServerState<B>>>,
    upgrade: WebSocketUpgrade,
) -> Response
where
    B: CaptureBackend + InputSink + Send + 'static,
{
    state.ensure_keeper();
    // Take the slot before the upgrade: whoever wins the mutex owns the one session. A
    // handshake the server then refuses hands it back (see `SessionEnd::Refused`).
    let start = {
        let mut slot = state.lock_slot();
        // A park whose grace ran out is over, even when the keeper has not woken yet.
        if let Slot::Parked(parked) = &*slot
            && Instant::now() >= parked.deadline()
        {
            let cause = if parked.display_died() {
                EndCause::Fault
            } else {
                EndCause::Clean
            };
            state.go_dead(&mut slot, cause);
        }
        match &*slot {
            Slot::Idle(_) | Slot::Parked(_) => slot_take(&mut slot),
            Slot::Live => return busy(),
            Slot::Dead => return gone(),
        }
    };

    let claim = Claim {
        state: Arc::clone(&state),
        start: Some(start),
        settled: false,
    };
    // The wire caps a message at MAX_MESSAGE_BYTES, and the WebSocket layer enforces the same
    // number on a whole message and on any one frame of it, so a fragmented message is refused
    // before it is assembled, not after (tungstenite's own defaults are 64 MiB and 16 MiB).
    upgrade
        .max_message_size(MAX_MESSAGE_BYTES)
        .max_frame_size(MAX_MESSAGE_BYTES)
        .on_upgrade(move |socket| claim.run(socket))
}

/// A live slot, as a refusal.
fn busy() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        "a session already owns this streamer",
    )
        .into_response()
}

/// A dead slot, as a refusal: nothing will be served again.
fn gone() -> Response {
    (
        StatusCode::GONE,
        "this streamer's session is over; nothing is served again",
    )
        .into_response()
}

/// Replaces the slot with [`Slot::Live`] and returns what it held.
fn slot_take<B>(slot: &mut Slot<B>) -> Start<B> {
    match std::mem::replace(slot, Slot::Live) {
        Slot::Idle(standby) => Start::Fresh(standby),
        Slot::Parked(parked) => Start::Resume(parked),
        Slot::Live | Slot::Dead => unreachable!("the caller checked the variant first"),
    }
}

/// The session slot, claimed for one upgrade.
///
/// Whatever happens to the upgrade, the slot never stays [`Slot::Live`] with nobody behind it.
/// A callback that never runs — the upgrade failed before the `101` left, so axum drops it —
/// hands the start back on drop, untouched. A session task that ends without settling — a
/// panic, which Tokio catches and answers by dropping the task — leaves the slot
/// [`Slot::Dead`]: the session's state can no longer be trusted, and readiness reads red.
struct Claim<B>
where
    B: CaptureBackend + InputSink + Send + 'static,
{
    state: Arc<ServerState<B>>,
    start: Option<Start<B>>,
    settled: bool,
}

impl<B> Claim<B>
where
    B: CaptureBackend + InputSink + Send + 'static,
{
    /// Runs the session on the upgraded socket and settles the slot with its end.
    async fn run(mut self, socket: WebSocket) {
        let Some(start) = self.start.take() else {
            return; // a claim holds its start until it runs, and runs once
        };
        let config = self.state.session_config();
        let end = session::run_session(socket, config, start).await;
        self.state.settle(end);
        self.settled = true;
    }
}

impl<B> Drop for Claim<B>
where
    B: CaptureBackend + InputSink + Send + 'static,
{
    fn drop(&mut self) {
        if let Some(start) = self.start.take() {
            tracing::debug!("the upgrade failed before the session began; the slot is handed back");
            self.state.settle(SessionEnd::Refused(start));
        } else if !self.settled {
            tracing::error!("a session task ended without settling; the session is torn down");
            let mut slot = self.state.lock_slot();
            self.state.go_dead(&mut slot, EndCause::Fault);
        }
    }
}

/// Watches whatever no socket serves, until the session is over (see the module docs).
async fn keeper<B>(state: Arc<ServerState<B>>)
where
    B: CaptureBackend + InputSink + Send + 'static,
{
    /// Ends the session if the keeper stops before it does: a panic in a pass (Tokio catches
    /// it and drops the task) must not leave a park that never expires.
    struct Watch<'a, B>
    where
        B: CaptureBackend + InputSink + Send + 'static,
    {
        state: &'a ServerState<B>,
        done: bool,
    }

    impl<B> Drop for Watch<'_, B>
    where
        B: CaptureBackend + InputSink + Send + 'static,
    {
        fn drop(&mut self) {
            if !self.done {
                let mut slot = self.state.lock_slot();
                if !matches!(*slot, Slot::Dead) {
                    tracing::error!("the session keeper stopped early; the session is torn down");
                    self.state.go_dead(&mut slot, EndCause::Fault);
                }
            }
        }
    }

    let mut guard = Watch {
        state: &state,
        done: false,
    };
    while let Some(wake) = guard.state.keep() {
        tokio::time::sleep_until(wake).await;
    }
    guard.done = true;
}

/// Serves until the one session is over, or until [`ServerState::shutdown`].
///
/// Returns the bound address so the caller can log it; [`ServerState::outcome`] then says why
/// the session ended.
pub async fn serve<B>(state: Arc<ServerState<B>>, bind: &Bind) -> std::io::Result<Bound>
where
    B: CaptureBackend + InputSink + Send + 'static,
{
    state.ensure_keeper();
    let app = router::<B>().with_state(Arc::clone(&state));
    let over = async move { state.until_over().await };
    match bind {
        Bind::Loopback { port } => {
            let listener = TcpListener::bind(("127.0.0.1", *port)).await?;
            let addr = listener.local_addr()?;
            tracing::info!(%addr, "listening on loopback");
            axum::serve(listener, app)
                .with_graceful_shutdown(over)
                .await?;
            Ok(Bound::Loopback(addr))
        }
        Bind::Unix { path } => serve_unix(app, path, over).await,
    }
}

/// Where the server ended up listening.
#[derive(Debug, Clone)]
pub enum Bound {
    /// The loopback address and port.
    Loopback(std::net::SocketAddr),
    /// The unix socket path.
    #[cfg_attr(not(unix), allow(dead_code))]
    Unix(std::path::PathBuf),
}

#[cfg(unix)]
async fn serve_unix(
    app: Router,
    path: &std::path::Path,
    over: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<Bound> {
    let listener = bind_unix(path)?;
    tracing::info!(path = %path.display(), "listening on a unix socket");
    let bound = Bound::Unix(path.to_owned());
    axum::serve(listener, app)
        .with_graceful_shutdown(over)
        .await?;
    Ok(bound)
}

/// Binds the unix socket at `path`, and makes it the owner's alone.
///
/// Something already at the path is removed only when it is a socket nobody answers on — the
/// file a crashed process left. A socket another server still answers on, and anything that is
/// not a socket at all, is refused, and the error names it: bind(2) reports every existing path
/// the same way, and removing a live socket's path would silently take it over. Filesystem
/// permissions are this bind's access control, so the socket is made mode 0600 before a
/// connection is served.
#[cfg(unix)]
fn bind_unix(path: &std::path::Path) -> std::io::Result<tokio::net::UnixListener> {
    use std::io::{Error, ErrorKind};
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};

    let listener = match tokio::net::UnixListener::bind(path) {
        Ok(listener) => listener,
        Err(e) if e.kind() == ErrorKind::AddrInUse => {
            if !std::fs::symlink_metadata(path)?.file_type().is_socket() {
                return Err(Error::new(
                    ErrorKind::AlreadyExists,
                    format!(
                        "{} exists and is not a socket; not replacing it",
                        path.display()
                    ),
                ));
            }
            if std::os::unix::net::UnixStream::connect(path).is_ok() {
                return Err(Error::new(
                    ErrorKind::AddrInUse,
                    format!(
                        "another server answers on {}; not taking it over",
                        path.display()
                    ),
                ));
            }
            tracing::warn!(path = %path.display(), "removing a stale socket file");
            std::fs::remove_file(path)?;
            tokio::net::UnixListener::bind(path)?
        }
        Err(e) => return Err(e),
    };
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

#[cfg(not(unix))]
async fn serve_unix(
    app: Router,
    path: &std::path::Path,
    over: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<Bound> {
    let _ = (app, path, over);
    Err(std::io::Error::other(
        "unix sockets are not supported on this platform; bind loopback:<port>",
    ))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use appricot_core::{
        CaptureBackend, InputSink, KeyEvent, PixelBuffer, Point, PointerButton, PressState, Rect,
        Size, SurfaceEvent, SurfaceId,
    };

    use super::{Claim, ServerState, Slot, slot_take};
    use crate::backend::BackendHandle;
    use crate::session::EndCause;

    /// A backend that serves nothing: these tests watch the slot, not the display.
    struct Null;

    /// The null backend's error; it never happens.
    #[derive(Debug)]
    struct Never;

    impl std::fmt::Display for Never {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("never")
        }
    }

    impl std::error::Error for Never {}

    impl CaptureBackend for Null {
        type Error = Never;
        fn drain_events(&mut self, _: &mut Vec<SurfaceEvent>) -> Result<(), Never> {
            Ok(())
        }
        fn root_size(&mut self) -> Result<Size, Never> {
            Ok(Size::new(1, 1))
        }
        fn capture(&mut self, _: SurfaceId, rect: Rect) -> Result<PixelBuffer, Never> {
            Ok(PixelBuffer {
                size: rect.size,
                stride: 4 * rect.size.width as usize,
                format: appricot_core::PixelFormat::Bgrx8888,
                data: vec![0; 4 * rect.size.width as usize * rect.size.height as usize],
            })
        }
    }

    impl InputSink for Null {
        type Error = Never;
        fn pointer_motion(&mut self, _: SurfaceId, _: Point) -> Result<(), Never> {
            Ok(())
        }
        fn pointer_button(
            &mut self,
            _: SurfaceId,
            _: PointerButton,
            _: PressState,
        ) -> Result<(), Never> {
            Ok(())
        }
        fn pointer_axis(&mut self, _: SurfaceId, _: Point) -> Result<(), Never> {
            Ok(())
        }
        fn key(&mut self, _: KeyEvent) -> Result<(), Never> {
            Ok(())
        }
        fn focus(&mut self, _: SurfaceId) -> Result<(), Never> {
            Ok(())
        }
        fn blur(&mut self) -> Result<(), Never> {
            Ok(())
        }
        fn configure(&mut self, _: SurfaceId, _: Size) -> Result<(), Never> {
            Ok(())
        }
        fn close(&mut self, _: SurfaceId) -> Result<(), Never> {
            Ok(())
        }
        fn clipboard_set(&mut self, _: &str) -> Result<(), Never> {
            Ok(())
        }
    }

    fn claimed() -> (Arc<ServerState<Null>>, Claim<Null>) {
        let state = ServerState::new(b"t".to_vec(), BackendHandle::spawn(Null));
        let start = slot_take(&mut state.lock_slot());
        let claim = Claim {
            state: Arc::clone(&state),
            start: Some(start),
            settled: false,
        };
        (state, claim)
    }

    #[tokio::test]
    async fn an_upgrade_that_never_runs_hands_the_slot_back() {
        let (state, claim) = claimed();
        assert!(matches!(*state.lock_slot(), Slot::Live));
        // axum drops the callback unrun when the upgrade fails before the 101 left.
        drop(claim);
        assert!(
            matches!(*state.lock_slot(), Slot::Idle(_)),
            "the next client must be served, not refused as busy"
        );
        assert_eq!(state.outcome(), None);
    }

    #[tokio::test]
    async fn a_session_task_that_dies_unsettled_leaves_the_slot_dead() {
        let (state, mut claim) = claimed();
        // The session took its start and then never settled: what a panic inside it leaves.
        drop(claim.start.take());
        drop(claim);
        assert!(
            matches!(*state.lock_slot(), Slot::Dead),
            "never Live forever with nobody behind it"
        );
        assert_eq!(state.outcome(), Some(EndCause::Fault));
    }

    #[tokio::test]
    async fn a_stop_tears_an_idle_backend_down() {
        let (state, claim) = claimed();
        drop(claim);
        state.shutdown();
        assert!(matches!(*state.lock_slot(), Slot::Dead));
        assert_eq!(state.outcome(), Some(EndCause::Clean));
        state.until_over().await;
    }
}
