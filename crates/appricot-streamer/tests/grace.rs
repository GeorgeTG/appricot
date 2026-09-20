//! The resume grace on a short leash: expiry, teardown, and what a reconnect then meets.
//!
//! The protocol's grace (docs/protocol/v0.md §7) is `RESUME_GRACE_MS` = 10 s — too slow to sit
//! out in a test. `appricot_streamer::config::set_resume_grace_ms` shortens it for this process
//! (the knob exists for exactly that; it overrides both the parked wait and the value the
//! `HelloReply` advertises). The override is process-wide and the tests of one binary share a
//! process, so every test here holds one lock and restores the default before it ends.
//!
//! What this file pins, against the real server over a real loopback WebSocket:
//!
//! - a session dropped inside the grace still resumes, however short the grace is;
//! - a `Hello` without a serial replaces a parked session with a fresh one: the old window set
//!   is never re-announced;
//! - once the grace expires, the keeper tears the parked session — backend included — down, and
//!   nothing is served again: v0.md §7 leaves the aftermath to the streamer, and this streamer's
//!   answer is the one session per process (a reconnect is refused at the upgrade, not answered
//!   with a `HelloReply`).

// The shared mock carries helpers this binary does not exercise; that is what sharing means.
#[allow(dead_code)]
mod common;

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use appricot_core::{
    CaptureBackend, InputSink, KeyEvent, PixelBuffer, Point, PointerButton, PressState, Rect, Size,
    SurfaceEvent, SurfaceId,
};
use appricot_proto::wire::{Body, Envelope, Hello, decode_envelope, encode_envelope};
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

use appricot_streamer::backend::BackendHandle;
use appricot_streamer::server::{self, ServerState};

const TOKEN: &[u8] = b"test-stream-token";

/// One lock for every test that touches the process-wide grace override: the tests of this
/// binary run in parallel tasks and the override is shared state. A `tokio` mutex, because
/// the guard is held across every await of a test.
static GRACE_KNOB: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Holds the lock for the test's duration.
async fn knob() -> tokio::sync::MutexGuard<'static, ()> {
    GRACE_KNOB.lock().await
}

/// A [`common::MockBackend`] that sets a flag when it is dropped, so a test can watch the
/// keeper tear the backend down. The shared mock stays untouched; the wrapper lives here.
struct DroppingMock {
    inner: common::MockBackend,
    dropped: Arc<AtomicBool>,
}

impl Drop for DroppingMock {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::Release);
    }
}

impl CaptureBackend for DroppingMock {
    type Error = common::MockError;

    fn drain_events(&mut self, out: &mut Vec<SurfaceEvent>) -> Result<(), Self::Error> {
        self.inner.drain_events(out)
    }

    fn root_size(&mut self) -> Result<Size, Self::Error> {
        self.inner.root_size()
    }

    fn capture(&mut self, id: SurfaceId, rect: Rect) -> Result<PixelBuffer, Self::Error> {
        self.inner.capture(id, rect)
    }
}

impl InputSink for DroppingMock {
    type Error = common::MockError;

    fn pointer_motion(&mut self, id: SurfaceId, at: Point) -> Result<(), Self::Error> {
        self.inner.pointer_motion(id, at)
    }

    fn pointer_button(
        &mut self,
        id: SurfaceId,
        button: PointerButton,
        state: PressState,
    ) -> Result<(), Self::Error> {
        self.inner.pointer_button(id, button, state)
    }

    fn pointer_axis(&mut self, id: SurfaceId, steps: Point) -> Result<(), Self::Error> {
        self.inner.pointer_axis(id, steps)
    }

    fn key(&mut self, key: KeyEvent) -> Result<(), Self::Error> {
        self.inner.key(key)
    }

    fn focus(&mut self, id: SurfaceId) -> Result<(), Self::Error> {
        self.inner.focus(id)
    }

    fn blur(&mut self) -> Result<(), Self::Error> {
        self.inner.blur()
    }

    fn configure(&mut self, id: SurfaceId, size: Size) -> Result<(), Self::Error> {
        self.inner.configure(id, size)
    }

    fn close(&mut self, id: SurfaceId) -> Result<(), Self::Error> {
        self.inner.close(id)
    }

    fn clipboard_set(&mut self, text: &str) -> Result<(), Self::Error> {
        self.inner.clipboard_set(text)
    }
}

/// A spawned server: its address, its state, the mock's handle, and the mock's drop flag.
struct TestServer {
    addr: SocketAddr,
    state: Arc<ServerState<DroppingMock>>,
    mock: common::MockHandle,
    dropped: Arc<AtomicBool>,
}

/// Binds the server on an ephemeral loopback port and serves it in the background.
async fn spawn_server() -> TestServer {
    let (inner, mock) = common::MockBackend::pair();
    let dropped = Arc::new(AtomicBool::new(false));
    let backend = BackendHandle::spawn(DroppingMock {
        inner,
        dropped: Arc::clone(&dropped),
    });
    let state = ServerState::new(TOKEN.to_vec(), backend);

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("an ephemeral loopback port is free");
    let addr = listener
        .local_addr()
        .expect("the listener knows its address");
    let app = server::router::<DroppingMock>().with_state(Arc::clone(&state));
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("the server serves");
    });

    TestServer {
        addr,
        state,
        mock,
        dropped,
    }
}

/// A connected client WebSocket.
type Client = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Connects a WebSocket client to the server's `/session`.
async fn connect(addr: &SocketAddr) -> Client {
    let url = format!("ws://{addr}/session");
    match connect_async(url).await {
        Ok((stream, _response)) => stream,
        Err(e) => panic!("cannot connect to /session: {e}"),
    }
}

/// Connects, retrying while the server answers 503 (the previous socket's park has not landed
/// yet). Gives up after five seconds.
async fn connect_with_retry(addr: &SocketAddr) -> Client {
    let url = format!("ws://{addr}/session");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        match connect_async(url.clone()).await {
            Ok((stream, _response)) => return stream,
            Err(_) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(e) => panic!("cannot connect to /session, even with retries: {e}"),
        }
    }
}

/// The bytes of one client envelope.
fn envelope(body: Body) -> Vec<u8> {
    encode_envelope(&Envelope { body: Some(body) }).expect("the test builds valid messages")
}

/// A Hello with this token, offering RAW, naming `resume_serial` when it has one.
fn hello(resume_serial: Option<u32>) -> Vec<u8> {
    envelope(Body::Hello(Hello {
        protocol_version: 0,
        client_name: "grace-test".into(),
        stream_token: TOKEN.to_vec(),
        codecs: vec![],
        resume_serial,
    }))
}

/// Sends one envelope.
async fn send(ws: &mut Client, bytes: &[u8]) {
    ws.send(Message::Binary(bytes.to_vec().into()))
        .await
        .expect("the client sends");
}

/// Reads the next envelope, skipping pings, failing on anything else.
async fn read_body(ws: &mut Client) -> Body {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        let next = tokio::time::timeout(left, ws.next())
            .await
            .expect("an envelope arrives within the deadline")
            .expect("the socket is open");
        let message = next.expect("the socket carries a message");
        match message {
            Message::Binary(bytes) => {
                return decode_envelope(&bytes)
                    .expect("the server sends valid envelopes")
                    .body
                    .expect("the server never sends an empty envelope");
            }
            Message::Pong(_) => {}
            other => panic!("expected a binary envelope, got {other:?}"),
        }
    }
}

/// Reads the next `SurfaceNew` of `surface_id`, panicking with what arrived instead.
async fn expect_surface_new(ws: &mut Client, surface_id: u32) -> appricot_proto::wire::SurfaceNew {
    match read_body(ws).await {
        Body::SurfaceNew(m) if m.surface_id == surface_id => m,
        Body::Bye(bye) => panic!(
            "got Bye({:?}, {:?}) while waiting for SurfaceNew({surface_id})",
            bye.reason, bye.text
        ),
        other => panic!("expected SurfaceNew({surface_id}), got {other:?}"),
    }
}

/// Reads the next `Frame` of `surface_id`, panicking with what arrived instead.
async fn expect_frame(ws: &mut Client, surface_id: u32) -> appricot_proto::wire::Frame {
    match read_body(ws).await {
        Body::Frame(m) if m.surface_id == surface_id => m,
        Body::Bye(bye) => panic!(
            "got Bye({:?}, {:?}) while waiting for a Frame of {surface_id}",
            bye.reason, bye.text
        ),
        other => panic!("expected a Frame of {surface_id}, got {other:?}"),
    }
}

/// Waits until the mock's drop flag is set, panicking after `deadline`.
async fn wait_dropped(server: &TestServer, deadline: Duration) {
    let end = tokio::time::Instant::now() + deadline;
    while !server.dropped.load(Ordering::Acquire) {
        assert!(
            tokio::time::Instant::now() < end,
            "the parked backend was never torn down"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Drives one session to a live state with one window, and returns the socket with the
/// `HelloReply`'s resume serial.
async fn live_session_with_a_window(server: &TestServer, id: u32) -> (Client, u32) {
    let mut ws = connect(&server.addr).await;
    send(&mut ws, &hello(None)).await;
    let Body::HelloReply(reply) = read_body(&mut ws).await else {
        panic!("expected a HelloReply");
    };
    assert!(!reply.resumed);
    let serial = reply.resume_serial.expect("a session can be resumed");
    server.mock.create_surface(id, Size::new(120, 80));
    expect_surface_new(&mut ws, id).await;
    expect_frame(&mut ws, id).await;
    (ws, serial)
}

#[tokio::test]
async fn an_expired_grace_tears_the_parked_session_down() {
    let _knob = knob().await;
    appricot_streamer::config::set_resume_grace_ms(300);

    let server = spawn_server().await;
    server.state.set_ready();
    let (ws, _serial) = live_session_with_a_window(&server, 7).await;

    // Drop the socket without a Bye: the session parks for 300 ms, then the keeper tears it —
    // backend included — down (v0.md §7: after the grace, the session is gone).
    drop(ws);
    wait_dropped(&server, Duration::from_secs(5)).await;

    // Past the grace, the parked serial names nothing and nothing is served again: the upgrade
    // itself is refused (the one session of this process is dead), never answered with a
    // HelloReply. This streamer's answer to v0.md §7's "what a reconnect then meets".
    let url = format!("ws://{}/session", server.addr);
    assert!(
        connect_async(url).await.is_err(),
        "an expired session serves nothing again"
    );

    appricot_streamer::config::set_resume_grace_ms(0);
}

#[tokio::test]
async fn a_short_grace_still_resumes_a_session_dropped_within_it() {
    let _knob = knob().await;
    appricot_streamer::config::set_resume_grace_ms(2_000);

    let server = spawn_server().await;
    server.state.set_ready();
    let (first, serial) = live_session_with_a_window(&server, 7).await;
    drop(first);

    // Inside the 2 s grace, a reconnect naming the serial resumes the session.
    let mut second = connect_with_retry(&server.addr).await;
    send(&mut second, &hello(Some(serial))).await;
    let Body::HelloReply(reply) = read_body(&mut second).await else {
        panic!("expected a HelloReply");
    };
    assert!(reply.resumed, "the parked session resumes within the grace");

    // The whole window set is re-announced, then one full-redraw frame per surface.
    let announced = expect_surface_new(&mut second, 7).await;
    let size = announced.size.expect("a size is carried");
    assert_eq!((size.width, size.height), (120, 80));
    let frame = expect_frame(&mut second, 7).await;
    assert!(frame.full_redraw, "a resume repaints everything");
    assert_eq!(frame.sequence, 1, "the resume restarts the sequences");

    appricot_streamer::config::set_resume_grace_ms(0);
}

#[tokio::test]
async fn a_hello_without_a_serial_replaces_a_parked_session_with_a_fresh_one() {
    let _knob = knob().await;
    appricot_streamer::config::set_resume_grace_ms(2_000);

    let server = spawn_server().await;
    server.state.set_ready();
    let (first, _serial) = live_session_with_a_window(&server, 7).await;
    drop(first);

    // A Hello that names no serial replaces the parked session (v0.md §7): the reply says
    // resumed = false and the client starts from an empty window set.
    let mut second = connect_with_retry(&server.addr).await;
    send(&mut second, &hello(None)).await;
    let Body::HelloReply(reply) = read_body(&mut second).await else {
        panic!("expected a HelloReply");
    };
    assert!(!reply.resumed, "no serial named: fresh, not resumed");

    // A new window is announced on demand, and the old one never comes back: the replaced
    // session's window set died with it.
    server.mock.create_surface(8, Size::new(90, 60));
    let announced = expect_surface_new(&mut second, 8).await;
    let size = announced.size.expect("a size is carried");
    assert_eq!((size.width, size.height), (90, 60));
    expect_frame(&mut second, 8).await;

    let quiet = tokio::time::timeout(Duration::from_millis(300), read_body(&mut second)).await;
    assert!(
        quiet.is_err(),
        "surface 7 of the replaced session is never re-announced; the socket stays quiet"
    );

    appricot_streamer::config::set_resume_grace_ms(0);
}

#[tokio::test]
async fn the_reply_names_the_grace_actually_honoured() {
    let _knob = knob().await;
    appricot_streamer::config::set_resume_grace_ms(250);

    let server = spawn_server().await;
    server.state.set_ready();
    let mut ws = connect(&server.addr).await;
    send(&mut ws, &hello(None)).await;
    let Body::HelloReply(reply) = read_body(&mut ws).await else {
        panic!("expected a HelloReply");
    };
    // The knob overrides the wait and the advertisement together: what the reply promises is
    // what the park will honour, or the resume the client plans around the reply is a lie.
    assert_eq!(reply.resume_grace_ms, Some(250));

    appricot_streamer::config::set_resume_grace_ms(0);
}
