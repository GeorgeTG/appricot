//! Every size change reaches the client before the frames that use it (v0.md §4.2).
//!
//! - A size the app takes on its own arrives as `ConfigureAck` with serial 0, before the first
//!   frame whose tiles reach past the old size. A client that sized its registry by a
//!   `ResizeAsk` instead would refuse those tiles, and the surface would freeze.
//! - A burst of configures is acked one by one, each with the size the app took for it.
//! - A configure of the size the surface already has is acked at once, even by a backend that
//!   reports nothing when nothing changed, and the app's next resize of its own is not
//!   mistaken for its answer.

// The shared mock and the harness carry helpers this binary does not exercise.
#[allow(dead_code)]
mod common;
#[allow(dead_code)]
mod harness;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use appricot_core::{
    CaptureBackend, InputSink, KeyEvent, PixelBuffer, Point, PointerButton, PressState, Rect, Size,
    SurfaceEvent, SurfaceId,
};
use appricot_proto::wire::{Body, Configure, ConfigureAck, Frame};

use common::{MockBackend, MockError, MockHandle};
use harness::{Client, ack, expect_quiet, read_body, send, serve, start};

/// The mock, with one difference that matters here: like an X server asked for the geometry a
/// window already has, it reports nothing for a configure that changes nothing. The shared mock
/// reports a `Resized` for every configure, which hides a configure left waiting.
struct QuietOnNoOp {
    inner: MockBackend,
    /// The size each surface has, as this backend applied it.
    sizes: Arc<Mutex<HashMap<u32, Size>>>,
}

impl CaptureBackend for QuietOnNoOp {
    type Error = MockError;

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

impl InputSink for QuietOnNoOp {
    type Error = MockError;

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
        let mut sizes = self.sizes.lock().expect("the size map is not poisoned");
        if sizes.get(&id.get()) == Some(&size) {
            return Ok(());
        }
        sizes.insert(id.get(), size);
        self.inner.configure(id, size)
    }

    fn close(&mut self, id: SurfaceId) -> Result<(), Self::Error> {
        self.inner.close(id)
    }

    fn clipboard_set(&mut self, text: &str) -> Result<(), Self::Error> {
        self.inner.clipboard_set(text)
    }
}

/// A served session with one 400x300 toplevel, id 1, whose first frame is drawn and acked.
async fn one_window() -> (Client, MockHandle, Arc<Mutex<HashMap<u32, Size>>>) {
    let (inner, mock) = MockBackend::pair();
    let sizes = Arc::new(Mutex::new(HashMap::new()));
    let backend = QuietOnNoOp {
        inner,
        sizes: Arc::clone(&sizes),
    };
    let addr = serve(backend).await;
    let (mut ws, reply) = start(addr, None).await;
    assert!(!reply.resumed);

    sizes
        .lock()
        .expect("the size map is not poisoned")
        .insert(1, Size::new(400, 300));
    mock.create_surface(1, Size::new(400, 300));
    match read_body(&mut ws).await {
        Body::SurfaceNew(m) => assert_eq!(m.surface_id, 1),
        other => panic!("expected SurfaceNew, got {other:?}"),
    }
    let first = expect_frame(&mut ws).await;
    assert!(first.full_redraw);
    ack(&mut ws, 1, first.sequence).await;
    (ws, mock, sizes)
}

/// Reads the next body, which must be a Frame.
async fn expect_frame(ws: &mut Client) -> Frame {
    match read_body(ws).await {
        Body::Frame(frame) => frame,
        other => panic!("expected a Frame, got {other:?}"),
    }
}

/// Reads the next body, which must be a ConfigureAck.
async fn expect_configure_ack(ws: &mut Client) -> ConfigureAck {
    match read_body(ws).await {
        Body::ConfigureAck(ack) => ack,
        other => panic!("expected a ConfigureAck, got {other:?}"),
    }
}

/// A `ConfigureAck`'s serial and size, for one assert.
fn serial_and_size(ack: &ConfigureAck) -> (u32, u32, u32) {
    let size = ack.size.expect("an ack carries a size");
    (ack.serial, size.width, size.height)
}

/// The right and bottom edges the frame's tiles reach.
fn reach(frame: &Frame) -> (u32, u32) {
    frame.tiles.iter().fold((0, 0), |(right, bottom), tile| {
        let rect = tile.rect.expect("every tile carries a rect");
        let x = u32::try_from(rect.x).expect("tiles sit inside the surface");
        let y = u32::try_from(rect.y).expect("tiles sit inside the surface");
        (right.max(x + rect.width), bottom.max(y + rect.height))
    })
}

/// Sends a Configure of `width` x `height` under the client's `serial`.
async fn configure(ws: &mut Client, serial: u32, width: u32, height: u32) {
    send(
        ws,
        Body::Configure(Configure {
            surface_id: 1,
            serial,
            size: Some(appricot_proto::wire::Size { width, height }),
        }),
    )
    .await;
}

#[tokio::test]
async fn a_size_the_app_takes_on_its_own_reaches_the_client_before_its_frames() {
    let (mut ws, mock, _sizes) = one_window().await;

    // The app grows its window with no configure waiting (a details expander, say), then
    // draws into the new area.
    mock.push(SurfaceEvent::Resized {
        id: SurfaceId::new(1),
        size: Size::new(600, 400),
    });
    mock.push(SurfaceEvent::Damaged {
        id: SurfaceId::new(1),
        rect: Rect::new(450, 320, 100, 50),
    });

    // The size is a fact, sent as an ack under serial 0 — never a mere ResizeAsk — and it
    // comes first.
    let resized = expect_configure_ack(&mut ws).await;
    assert_eq!(resized.surface_id, 1);
    assert_eq!(serial_and_size(&resized), (0, 600, 400));

    // Then the frame, whose tiles reach past the old 400x300 and stay inside 600x400.
    let frame = expect_frame(&mut ws).await;
    assert_eq!(frame.sequence, 2);
    assert_eq!(reach(&frame), (600, 400));
}

#[tokio::test]
async fn a_burst_of_configures_is_acked_one_by_one_with_the_size_each_took() {
    let (mut ws, _mock, _sizes) = one_window().await;

    // A live drag: two proposals before the app answers the first.
    configure(&mut ws, 11, 800, 600).await;
    configure(&mut ws, 12, 900, 700).await;

    let first = expect_configure_ack(&mut ws).await;
    assert_eq!(serial_and_size(&first), (11, 800, 600));
    // The first answer's size is the host's truth only until the second arrives, and the
    // frames that follow never run past the size last acked.
    let mut acked = (800, 600);
    loop {
        match read_body(&mut ws).await {
            Body::Frame(frame) => {
                let (right, bottom) = reach(&frame);
                assert!(right <= acked.0 && bottom <= acked.1, "{frame:?}");
                ack(&mut ws, 1, frame.sequence).await;
            }
            Body::ConfigureAck(second) => {
                assert_eq!(serial_and_size(&second), (12, 900, 700));
                acked = (900, 700);
                break;
            }
            other => panic!("expected a Frame or the second ack, got {other:?}"),
        }
    }
    let frame = expect_frame(&mut ws).await;
    assert_eq!(reach(&frame), acked);
}

#[tokio::test]
async fn a_configure_that_changes_nothing_is_acked_at_once_and_left_behind() {
    let (mut ws, mock, _sizes) = one_window().await;

    // The size the window already has: this backend reports nothing, and the ack still comes.
    configure(&mut ws, 21, 400, 300).await;
    let noop = expect_configure_ack(&mut ws).await;
    assert_eq!(serial_and_size(&noop), (21, 400, 300));
    expect_quiet(&mut ws, 200, "one configure, one ack, and no frame").await;

    // The app's next resize of its own is its own: serial 0, not the stale 21.
    mock.push(SurfaceEvent::Resized {
        id: SurfaceId::new(1),
        size: Size::new(500, 300),
    });
    let own = expect_configure_ack(&mut ws).await;
    assert_eq!(serial_and_size(&own), (0, 500, 300));
    let frame = expect_frame(&mut ws).await;
    assert_eq!(reach(&frame), (500, 300));
}

#[tokio::test]
async fn an_oversized_window_is_streamed_at_the_wire_caps_and_the_session_lives() {
    let (mut ws, mock, _sizes) = one_window().await;

    // Wider and taller than the wire can carry (1920x1200): clamped, not a dead session.
    mock.push(SurfaceEvent::Resized {
        id: SurfaceId::new(1),
        size: Size::new(2560, 1440),
    });
    let clamped = expect_configure_ack(&mut ws).await;
    assert_eq!(serial_and_size(&clamped), (0, 1920, 1200));
    let frame = expect_frame(&mut ws).await;
    assert_eq!(reach(&frame), (1920, 1200));
    ack(&mut ws, 1, frame.sequence).await;

    // The session still serves: a popup larger than the caps is announced clamped too.
    mock.push(SurfaceEvent::Created {
        id: SurfaceId::new(2),
        role: appricot_core::Role::Popup {
            parent: SurfaceId::new(1),
            positioner: appricot_core::Positioner::at(Point::new(0, 0), Size::new(2000, 30)),
        },
        size: Size::new(2000, 30),
        parent: None,
    });
    match read_body(&mut ws).await {
        Body::SurfaceNew(m) => {
            let size = m.size.expect("a size is carried");
            assert_eq!((size.width, size.height), (1920, 30));
            let positioner = m.positioner.expect("a popup carries a positioner");
            let size = positioner.size.expect("a positioner carries a size");
            assert_eq!((size.width, size.height), (1920, 30));
        }
        other => panic!("expected the popup's SurfaceNew, got {other:?}"),
    }
}
