//! The backend actor: one OS thread owns the display connection.
//!
//! `CaptureBackend` and `InputSink` are synchronous, and the real backend sits on an X11
//! socket: reading events, grabbing pixels and injecting input are blocking calls that must
//! never sit on the async runtime's workers. This module puts the backend on a dedicated thread
//! and hands the async side a [`BackendHandle`]: events arrive on a channel, every request is a
//! command with a one-shot reply.
//!
//! Why a thread and not `tokio::task::block_in_place`: the thread also solves event intake. The
//! backend reports events only when asked, and the async side has no wake-up of its own when the
//! display becomes readable, so *someone* has to poll. The actor polls
//! [`CaptureBackend::drain_events`] every 25 ms between commands (the private `POLL_INTERVAL`),
//! which keeps the cost at one poll per tick on a thread nobody waits on. Frame *pacing* stays
//! ack-driven (rule 5, docs/protocol/README.md): this poll decides when pixels are looked at,
//! never when a frame is sent — a frame still needs a free credit, which still needs the
//! client's ack.

use std::error::Error as StdError;
use std::sync::mpsc as std_mpsc;
use std::thread;
use std::time::Duration;

use appricot_core::{
    CaptureBackend, InputSink, KeyEvent, PixelBuffer, PointerButton, PressState, SurfaceEvent,
    SurfaceId,
};
use appricot_core::{Point, Rect, Size};
use tokio::sync::{mpsc, oneshot};

/// How long the actor waits for a command before it polls the display for events again.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// A boxed backend error: [`CaptureBackend::Error`] is `Error + Send + Sync + 'static`, so every
/// failure crosses the channel as this.
pub type BackendError = Box<dyn StdError + Send + Sync>;

/// What the backend produced for the async side.
#[derive(Debug)]
pub enum Feed {
    /// Events drained from the display.
    Events(Vec<SurfaceEvent>),
    /// [`CaptureBackend::drain_events`] failed. The display connection is gone; the session
    /// cannot continue and this feed ends.
    Fatal(BackendError),
}

/// One request into the backend thread.
enum Cmd {
    RootSize(oneshot::Sender<Result<Size, BackendError>>),
    Capture {
        id: SurfaceId,
        rect: Rect,
        reply: oneshot::Sender<Result<PixelBuffer, BackendError>>,
    },
    PointerMotion {
        id: SurfaceId,
        at: Point,
        reply: oneshot::Sender<Result<(), BackendError>>,
    },
    PointerButton {
        id: SurfaceId,
        button: PointerButton,
        state: PressState,
        reply: oneshot::Sender<Result<(), BackendError>>,
    },
    PointerAxis {
        id: SurfaceId,
        steps: Point,
        reply: oneshot::Sender<Result<(), BackendError>>,
    },
    Key {
        key: KeyEvent,
        reply: oneshot::Sender<Result<(), BackendError>>,
    },
    Focus {
        id: SurfaceId,
        reply: oneshot::Sender<Result<(), BackendError>>,
    },
    Blur(oneshot::Sender<Result<(), BackendError>>),
    Configure {
        id: SurfaceId,
        size: Size,
        reply: oneshot::Sender<Result<(), BackendError>>,
    },
    Close {
        id: SurfaceId,
        reply: oneshot::Sender<Result<(), BackendError>>,
    },
    ClipboardSet {
        text: String,
        reply: oneshot::Sender<Result<(), BackendError>>,
    },
    /// Stop the thread and drop the backend, closing the display connection.
    Stop,
}

/// The async side's handle to the backend thread.
///
/// Cloning is deliberately impossible: one handle owns by one backend. When the handle drops,
/// the thread stops and the backend is dropped with it.
pub struct BackendHandle<B> {
    cmd: std_mpsc::Sender<Cmd>,
    feed: mpsc::UnboundedReceiver<Feed>,
    _join: thread::JoinHandle<()>,
    _marker: std::marker::PhantomData<fn() -> B>,
}

impl<B> std::fmt::Debug for BackendHandle<B> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BackendHandle")
    }
}

impl<B> Drop for BackendHandle<B> {
    fn drop(&mut self) {
        // Best effort: the thread also ends when this sender drops right after.
        let _ = self.cmd.send(Cmd::Stop);
    }
}

/// What the async side sees when the backend thread can no longer answer.
const THREAD_ENDED: &str = "the backend thread ended";

/// The feed half of the handle: reading it needs no backend at all, only the channel.
impl<B> BackendHandle<B> {
    /// Receives the next feed item, waiting while the thread works.
    ///
    /// Returns `None` when the actor ended (after a [`Feed::Fatal`], or when the thread was
    /// stopped).
    pub async fn next_feed(&mut self) -> Option<Feed> {
        self.feed.recv().await
    }

    /// Takes every feed item that is ready right now, without waiting.
    pub fn drain_feed(&mut self) -> Vec<Feed> {
        let mut out = Vec::new();
        while let Ok(item) = self.feed.try_recv() {
            out.push(item);
        }
        out
    }
}

impl<B> BackendHandle<B>
where
    B: CaptureBackend + InputSink + Send + 'static,
{
    /// Starts the actor thread around `backend`.
    ///
    /// Events and requests are served until the handle drops or the backend fails; a failure is
    /// reported once as [`Feed::Fatal`] and ends the feed.
    ///
    /// # Panics
    ///
    /// Panics when the thread cannot be spawned: the process is out of resources, and there is
    /// no way to serve a backend without its thread.
    pub fn spawn(backend: B) -> Self {
        let (cmd_tx, mut cmd_rx) = std_mpsc::channel::<Cmd>();
        // Unbounded, so the actor never blocks on the async side; bounded in practice because
        // someone always drains it: the session pump while a socket is attached, the server's
        // keeper while the backend is idle or parked, and nobody for longer than the handshake
        // deadline or a send deadline in between (crate::server, crate::session).
        let (feed_tx, feed_rx) = mpsc::unbounded_channel::<Feed>();
        let join = thread::Builder::new()
            .name("appricot-backend".into())
            .spawn(move || actor(backend, &mut cmd_rx, &feed_tx))
            .expect("the backend thread spawns while the process can still allocate");

        Self {
            cmd: cmd_tx,
            feed: feed_rx,
            _join: join,
            _marker: std::marker::PhantomData,
        }
    }

    /// The root window's size.
    ///
    /// Fails when the display connection has died.
    pub async fn root_size(&self) -> Result<Size, BackendError> {
        self.call(Cmd::RootSize).await
    }

    /// Copies the pixels of `rect` of surface `id`.
    ///
    /// Fails when the surface is gone or the display connection has died; the caller skips the
    /// tile and carries on.
    pub async fn capture(&self, id: SurfaceId, rect: Rect) -> Result<PixelBuffer, BackendError> {
        self.call(|reply| Cmd::Capture { id, rect, reply }).await
    }

    /// Moves the pointer to `at` inside surface `id`.
    pub async fn pointer_motion(&self, id: SurfaceId, at: Point) -> Result<(), BackendError> {
        self.input(|reply| Cmd::PointerMotion { id, at, reply })
            .await
    }

    /// Presses or releases a pointer button over surface `id`.
    pub async fn pointer_button(
        &self,
        id: SurfaceId,
        button: PointerButton,
        state: PressState,
    ) -> Result<(), BackendError> {
        self.input(|reply| Cmd::PointerButton {
            id,
            button,
            state,
            reply,
        })
        .await
    }

    /// Scrolls by whole steps inside surface `id`.
    pub async fn pointer_axis(&self, id: SurfaceId, steps: Point) -> Result<(), BackendError> {
        self.input(|reply| Cmd::PointerAxis { id, steps, reply })
            .await
    }

    /// Presses or releases a key in the focused surface.
    pub async fn key(&self, key: KeyEvent) -> Result<(), BackendError> {
        self.input(|reply| Cmd::Key { key, reply }).await
    }

    /// Gives surface `id` the keyboard focus.
    pub async fn focus(&self, id: SurfaceId) -> Result<(), BackendError> {
        self.input(|reply| Cmd::Focus { id, reply }).await
    }

    /// Releases every key and button the backend holds.
    pub async fn blur(&self) -> Result<(), BackendError> {
        self.input(Cmd::Blur).await
    }

    /// Applies the host's configure to surface `id`.
    pub async fn configure(&self, id: SurfaceId, size: Size) -> Result<(), BackendError> {
        self.input(|reply| Cmd::Configure { id, size, reply }).await
    }

    /// Asks the app to close surface `id`.
    pub async fn close(&self, id: SurfaceId) -> Result<(), BackendError> {
        self.input(|reply| Cmd::Close { id, reply }).await
    }

    /// Makes `text` the clipboard the backend serves.
    pub async fn clipboard_set(&self, text: String) -> Result<(), BackendError> {
        self.input(|reply| Cmd::ClipboardSet { text, reply }).await
    }

    async fn input<F>(&self, make: F) -> Result<(), BackendError>
    where
        F: FnOnce(oneshot::Sender<Result<(), BackendError>>) -> Cmd,
    {
        self.call(make).await
    }

    /// Sends one command and awaits its reply.
    ///
    /// A send failure (the thread ended) and a dropped reply channel both surface as
    /// [`THREAD_ENDED`], so callers cannot distinguish a backend that died from one that was
    /// stopped: neither can answer again.
    async fn call<T, F>(&self, make: F) -> Result<T, BackendError>
    where
        F: FnOnce(oneshot::Sender<Result<T, BackendError>>) -> Cmd,
        T: Send + 'static,
    {
        let (tx, rx) = oneshot::channel();
        if self.cmd.send(make(tx)).is_err() {
            return Err(THREAD_ENDED.into());
        }
        match rx.await {
            Ok(reply) => reply,
            Err(_) => Err(THREAD_ENDED.into()),
        }
    }
}

/// The thread body: drain, serve one command or sleep a tick, repeat.
fn actor<B>(mut backend: B, cmd: &mut std_mpsc::Receiver<Cmd>, feed: &mpsc::UnboundedSender<Feed>)
where
    B: CaptureBackend + InputSink,
{
    loop {
        // Events first: a command that just injected input may have produced some, and a
        // spontaneous repaint must not wait for the next request.
        let mut events = Vec::new();
        if let Err(e) = backend.drain_events(&mut events) {
            // The trait contract: after a drain failure the session cannot go on.
            let _ = feed.send(Feed::Fatal(Box::new(e)));
            return;
        }
        if !events.is_empty() && feed.send(Feed::Events(events)).is_err() {
            return; // The handle is gone; nobody is listening.
        }

        match cmd.recv_timeout(POLL_INTERVAL) {
            // A stop asked for and a disconnected handle both end the thread the same way.
            Ok(Cmd::Stop) | Err(std_mpsc::RecvTimeoutError::Disconnected) => return,
            Ok(command) => serve(&mut backend, command),
            Err(std_mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
}

/// Runs one command against the backend and answers its one-shot.
fn serve<B>(backend: &mut B, command: Cmd)
where
    B: CaptureBackend + InputSink,
{
    match command {
        Cmd::RootSize(reply) => {
            let _ = reply.send(backend.root_size().map_err(Into::into));
        }
        Cmd::Capture { id, rect, reply } => {
            let _ = reply.send(backend.capture(id, rect).map_err(Into::into));
        }
        Cmd::PointerMotion { id, at, reply } => {
            let _ = reply.send(backend.pointer_motion(id, at).map_err(Into::into));
        }
        Cmd::PointerButton {
            id,
            button,
            state,
            reply,
        } => {
            let _ = reply.send(
                backend
                    .pointer_button(id, button, state)
                    .map_err(Into::into),
            );
        }
        Cmd::PointerAxis { id, steps, reply } => {
            let _ = reply.send(backend.pointer_axis(id, steps).map_err(Into::into));
        }
        Cmd::Key { key, reply } => {
            let _ = reply.send(backend.key(key).map_err(Into::into));
        }
        Cmd::Focus { id, reply } => {
            let _ = reply.send(backend.focus(id).map_err(Into::into));
        }
        Cmd::Blur(reply) => {
            let _ = reply.send(backend.blur().map_err(Into::into));
        }
        Cmd::Configure { id, size, reply } => {
            let _ = reply.send(backend.configure(id, size).map_err(Into::into));
        }
        Cmd::Close { id, reply } => {
            let _ = reply.send(backend.close(id).map_err(Into::into));
        }
        Cmd::ClipboardSet { text, reply } => {
            let _ = reply.send(backend.clipboard_set(&text).map_err(Into::into));
        }
        Cmd::Stop => unreachable!("Stop is handled by the actor loop"),
    }
}
