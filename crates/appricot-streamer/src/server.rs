//! The HTTP shell: readiness, and one WebSocket per process.
//!
//! Routes:
//!
//! - `GET /readyz` — `200 ready` once the backend is connected and its extension probe passed;
//!   `503` before that. Readiness is latched by the caller that owns the backend.
//! - `GET /session` — a WebSocket upgrade speaking the v0 protocol. Binary frames only; the
//!   first message must be the authenticated `Hello` (see [`crate::session`]).
//!
//! **One session per process.** While a session's socket is live, a second upgrade is refused
//! with `503`. When a session's socket dies without a closing `Bye`, the session (and the
//! backend under it) is parked for the resume grace; an upgrade during the grace resumes it
//! when the `Hello` names the stored `resume_serial`, and replaces it otherwise. After a clean
//! end or an expired grace the backend is torn down and every further upgrade is refused: the
//! display connection belongs to the one session this process serves.
//!
//! The session is claimed by an **authenticated `Hello`**, not by the upgrade. The slot is taken
//! before the upgrade only so two upgraders cannot both proceed; a handshake the server refuses
//! (a bad token, an unsupported version, no `Hello` at all) hands it straight back, so a
//! mistyped token costs a retry rather than the process, and a probe cannot brick the streamer.
//!
//! The server binds `127.0.0.1` or a unix socket, nothing else; [`crate::config::parse_bind`]
//! enforces that before any listener exists.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::State;
use axum::extract::ws::WebSocketUpgrade;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use tokio::net::TcpListener;

use appricot_core::{CaptureBackend, InputSink};

use crate::backend::BackendHandle;
use crate::config::Bind;
use crate::session::{self, ParkedSession, SessionConfig, Start};

/// The shared state of the serving process.
#[derive(Debug)]
pub struct ServerState<B> {
    token: Arc<Vec<u8>>,
    ready: Arc<AtomicBool>,
    slot: Arc<Mutex<Slot<B>>>,
}

/// What the one session slot holds.
#[derive(Debug)]
pub enum Slot<B> {
    /// The backend is connected and no session is running: the first upgrade takes it.
    Idle(BackendHandle<B>),
    /// A session pump is running on a socket.
    Live,
    /// The session lost its socket and waits out its resume grace.
    Parked(ParkedSession<B>),
    /// The backend is gone (clean end, expired grace, or a display failure). Refused from here
    /// on; a new display connection needs a new process.
    Dead,
}

impl<B> ServerState<B> {
    /// Builds the state around a connected backend.
    ///
    /// Readiness starts red; the caller latches it green once the extension probe has passed,
    /// through [`ServerState::set_ready`] or by cloning [`ServerState::ready_flag`].
    pub fn new(token: Vec<u8>, backend: BackendHandle<B>) -> Arc<Self> {
        Arc::new(Self {
            token: Arc::new(token),
            ready: Arc::new(AtomicBool::new(false)),
            slot: Arc::new(Mutex::new(Slot::Idle(backend))),
        })
    }

    /// The readiness flag, for the caller that owns the backend to set.
    pub fn ready_flag(self: &Arc<Self>) -> Arc<AtomicBool> {
        Arc::clone(&self.ready)
    }

    /// Latches readiness on.
    pub fn set_ready(&self) {
        self.ready.store(true, Ordering::Release);
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
    if state.ready.load(Ordering::Acquire) {
        (StatusCode::OK, "ready").into_response()
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, "starting").into_response()
    }
}

async fn session_upgrade<B>(
    State(state): State<Arc<ServerState<B>>>,
    upgrade: WebSocketUpgrade,
) -> Response
where
    B: CaptureBackend + InputSink + Send + 'static,
{
    // Take the slot before the upgrade: whoever wins the mutex owns the one session. A
    // handshake the server then refuses hands it back (see `SessionEnd::Refused`).
    let taken = {
        let mut slot = state.slot.lock().expect("the session slot is not poisoned");
        if matches!(*slot, Slot::Idle(_) | Slot::Parked(_)) {
            Some(slot_take(&mut slot))
        } else {
            None
        }
    };
    let Some(start) = taken else {
        return busy();
    };

    upgrade.on_upgrade(move |socket| async move {
        let config = SessionConfig {
            token: Arc::clone(&state.token),
        };
        let end = session::run_session(socket, config, start).await;
        park_or_end(&state, end);
    })
}

/// A `live` or `dead` slot, as a refusal.
fn busy() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        "a session already owns this streamer, or its backend is gone",
    )
        .into_response()
}

/// Replaces the slot with [`Slot::Live`] and returns what it held.
fn slot_take<B>(slot: &mut Slot<B>) -> Start<B> {
    match std::mem::replace(slot, Slot::Live) {
        Slot::Idle(backend) => Start::Fresh(backend),
        Slot::Parked(parked) => Start::Resume(parked),
        Slot::Live | Slot::Dead => unreachable!("the caller checked the variant first"),
    }
}

/// Parks a session that lost its socket, or records a clean end.
///
/// A parked session gets a keeper task for its grace window: it drains the display's events
/// into the parked session (so state stays current and memory stays bounded) and tears the
/// backend down when the grace expires unclaimed.
fn park_or_end<B>(state: &Arc<ServerState<B>>, end: session::SessionEnd<B>)
where
    B: CaptureBackend + InputSink + Send + 'static,
{
    let mut slot = state.slot.lock().expect("the session slot is not poisoned");
    match end {
        session::SessionEnd::Parked(parked) => {
            let grace = parked.grace;
            *slot = Slot::Parked(parked);
            drop(slot);
            tokio::spawn(grace_keeper::<B>(Arc::clone(state), grace));
        }
        session::SessionEnd::Ended => {
            *slot = Slot::Dead;
        }
        // No session was ever claimed: put the backend back where it came from, so the next
        // client is served instead of meeting a dead process. Without this, the upgrade alone
        // claimed the one session — a mistyped token, a port scan or a probe bricked the
        // streamer for good (measured 2026-09-21).
        session::SessionEnd::Refused(start) => match start {
            Start::Fresh(backend) => *slot = Slot::Idle(backend),
            Start::Resume(parked) => {
                let grace = parked.grace;
                *slot = Slot::Parked(parked);
                drop(slot);
                tokio::spawn(grace_keeper::<B>(Arc::clone(state), grace));
            }
        },
    }
}

/// Watches a parked session until it is resumed or its grace expires.
async fn grace_keeper<B>(state: Arc<ServerState<B>>, grace: std::time::Duration)
where
    B: CaptureBackend + InputSink + Send + 'static,
{
    /// How often the keeper wakes to drain the display into the parked session.
    const TICK: std::time::Duration = std::time::Duration::from_millis(50);

    let deadline = tokio::time::Instant::now() + grace;
    loop {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            teardown_parked(&state);
            return;
        }
        tokio::time::sleep_until((now + TICK).min(deadline)).await;

        // Drain whatever the display produced into the parked session, so a resume finds
        // current state and the feed channel never grows without bound. A slot that is no
        // longer parked means the session was resumed; the keeper is done.
        let still_parked = {
            let mut slot = state.slot.lock().expect("the session slot is not poisoned");
            match &mut *slot {
                Slot::Parked(parked) => {
                    parked.absorb_feed();
                    true
                }
                _ => false,
            }
        };
        if !still_parked {
            return;
        }
    }
}

/// Drops a parked session, tearing its backend down with it.
fn teardown_parked<B>(state: &Arc<ServerState<B>>) {
    let mut slot = state.slot.lock().expect("the session slot is not poisoned");
    if matches!(*slot, Slot::Parked(_)) {
        *slot = Slot::Dead;
    }
}

/// Serves until the process ends.
///
/// Returns the bound address so the caller can log it (and tests can connect).
pub async fn serve<B>(state: Arc<ServerState<B>>, bind: &Bind) -> std::io::Result<Bound>
where
    B: CaptureBackend + InputSink + Send + 'static,
{
    let app = router::<B>().with_state(state);
    match bind {
        Bind::Loopback { port } => {
            let listener = TcpListener::bind(("127.0.0.1", *port)).await?;
            let addr = listener.local_addr()?;
            tracing::info!(%addr, "listening on loopback");
            axum::serve(listener, app).await?;
            Ok(Bound::Loopback(addr))
        }
        Bind::Unix { path } => serve_unix(app, path).await,
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
async fn serve_unix(app: Router, path: &std::path::Path) -> std::io::Result<Bound> {
    let listener = match tokio::net::UnixListener::bind(path) {
        Ok(l) => l,
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            // A socket file left by a crashed process. Inside this container the socket
            // directory belongs to the streamer, so removing a dead file is safe; a live one
            // still refuses the bind and the error surfaces.
            tracing::warn!(path = %path.display(), "socket file exists; removing and retrying");
            std::fs::remove_file(path)?;
            tokio::net::UnixListener::bind(path)?
        }
        Err(e) => return Err(e),
    };
    tracing::info!(path = %path.display(), "listening on a unix socket");
    let bound = Bound::Unix(path.to_owned());
    axum::serve(listener, app).await?;
    Ok(bound)
}

#[cfg(not(unix))]
async fn serve_unix(app: Router, path: &std::path::Path) -> std::io::Result<Bound> {
    let _ = app;
    let _ = path;
    Err(std::io::Error::other(
        "unix sockets are not supported on this platform; bind loopback:<port>",
    ))
}
