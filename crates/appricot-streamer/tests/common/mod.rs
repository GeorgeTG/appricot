//! The mock backend for the streamer's integration tests.
//!
//! It implements both frozen traits against shared state a test can see: input is recorded,
//! surface events are queued and drained on demand, and `capture` paints deterministic
//! pixels. It is how the tests watch the server from the backend side of the world. The
//! server spawn and the WebSocket client helpers live in [`harness`].

pub mod harness;

use std::collections::{HashMap, VecDeque};
use std::error::Error;
use std::fmt;
use std::sync::{Arc, Mutex};

use appricot_core::{
    CaptureBackend, InputSink, KeyEvent, PixelBuffer, PixelFormat, Point, PointerButton,
    PressState, Rect, Size, SurfaceEvent, SurfaceId,
};

/// How one capture breaks the `CaptureBackend` contract, for the tests of contract C2.
///
/// Only the binaries that test the contract build one; the others see dead variants.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureFault {
    /// A buffer one pixel narrower than the rectangle asked for: the surface shrank under the
    /// capture.
    Narrower,
    /// A buffer of the rectangle's size with no bytes and stride 0.
    Empty,
}

/// What the mock recorded, one variant per `InputSink` call, in arrival order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    /// `pointer_motion`.
    Motion {
        /// The surface.
        surface: u32,
        /// Where.
        at: Point,
    },
    /// `pointer_button`.
    Button {
        /// The surface.
        surface: u32,
        /// Which button.
        button: PointerButton,
        /// Down or up.
        state: PressState,
    },
    /// `pointer_axis`.
    Axis {
        /// The surface.
        surface: u32,
        /// The steps.
        steps: Point,
    },
    /// `key`.
    Key {
        /// The event.
        key: KeyEvent,
    },
    /// `focus`.
    Focus {
        /// The surface.
        surface: u32,
    },
    /// `blur`.
    Blur,
    /// `configure`.
    Configure {
        /// The surface.
        surface: u32,
        /// The size.
        size: Size,
    },
    /// `clipboard_set`.
    Clipboard {
        /// The text.
        text: String,
    },
    /// `close`.
    Close {
        /// The surface.
        surface: u32,
    },
}

/// The shared state of a mock backend.
#[derive(Default)]
pub struct MockState {
    /// Surface events waiting to be drained, oldest first.
    queue: Vec<SurfaceEvent>,
    /// The input that arrived, oldest first.
    pub input: Vec<Input>,
    /// The surfaces the mock pretends exist, for `capture`.
    surfaces: HashMap<u32, Size>,
    /// Set to fail `drain_events` once, simulating a dead display.
    pub fail_drain: bool,
    /// When set, every pointer motion damages the surface it lands on, after this pause (an
    /// app that redraws under the pointer, with the round trip a real display costs).
    motion_damage: Option<std::time::Duration>,
    /// Faults the next captures commit, one each, oldest first.
    capture_faults: VecDeque<CaptureFault>,
    /// Set when the server dropped the backend: the display connection is gone.
    dropped: bool,
}

/// A test's handle to the mock's state.
#[derive(Clone, Default)]
pub struct MockHandle(Arc<Mutex<MockState>>);

/// Every helper here is shared by the test binaries, and each binary uses its own subset —
/// a helper outside a binary's subset would read as dead code in that binary, so the impl
/// opts out of the lint rather than every test.
#[allow(dead_code)]
impl MockHandle {
    /// Queues a surface event for the backend actor to drain.
    pub fn push(&self, event: SurfaceEvent) {
        self.lock().queue.push(event);
    }

    /// Queues a `Created` event for a toplevel of `size`.
    pub fn create_surface(&self, id: u32, size: Size) {
        self.create_surface_with_parent(id, size, None);
    }

    /// Queues a `Created` event for a toplevel of `size` that is a dialog of `parent`.
    ///
    /// A dialog is an ordinary toplevel whose `Created` names another surface as its
    /// parent; the role stays `Toplevel` (a popup's parent lives in its role instead).
    pub fn create_dialog(&self, id: u32, parent: u32, size: Size) {
        self.create_surface_with_parent(id, size, Some(parent));
    }

    /// Queues a `Created` event for a toplevel, with an optional dialog parent.
    fn create_surface_with_parent(&self, id: u32, size: Size, parent: Option<u32>) {
        let id = SurfaceId::new(id);
        self.lock().surfaces.insert(id.get(), size);
        self.push(SurfaceEvent::Created {
            id,
            role: appricot_core::Role::Toplevel,
            size,
            parent: parent.map(SurfaceId::new),
        });
    }

    /// The input recorded so far.
    pub fn input(&self) -> Vec<Input> {
        self.lock().input.clone()
    }

    /// Waits until `want` input records have arrived, then returns them.
    ///
    /// Panics (with how many arrived) after `secs` seconds — the actor drains every 25 ms, so
    /// a few seconds is generous.
    pub async fn wait_input(&self, want: usize) -> Vec<Input> {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if self.input().len() >= want {
                return self.input();
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out waiting for {want} input records; got {:?}",
                self.input()
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }

    /// Kills the display: the backend's next drain fails, as a dead X connection does.
    pub fn kill_display(&self) {
        self.lock().fail_drain = true;
    }

    /// Makes every pointer motion damage the surface under it, each after `pause`.
    pub fn damage_on_motion(&self, pause: std::time::Duration) {
        self.lock().motion_damage = Some(pause);
    }

    /// Makes the next captures commit `faults`, one per capture, in order.
    pub fn fail_next_captures(&self, faults: &[CaptureFault]) {
        self.lock().capture_faults.extend(faults.iter().copied());
    }

    /// How many pointer motions arrived so far.
    pub fn motions(&self) -> usize {
        self.lock()
            .input
            .iter()
            .filter(|i| matches!(i, Input::Motion { .. }))
            .count()
    }

    /// Whether the server dropped the backend.
    pub fn is_dropped(&self) -> bool {
        self.lock().dropped
    }

    /// How many surface events still wait for the backend actor to drain them.
    pub fn queued(&self) -> usize {
        self.lock().queue.len()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, MockState> {
        self.0.lock().expect("the mock state is not poisoned")
    }
}

/// The mock backend the server drives.
pub struct MockBackend(Arc<Mutex<MockState>>);

impl MockBackend {
    /// Builds the backend and the test's handle to its state.
    pub fn pair() -> (Self, MockHandle) {
        let state = Arc::new(Mutex::new(MockState::default()));
        (Self(Arc::clone(&state)), MockHandle(state))
    }
}

impl Drop for MockBackend {
    fn drop(&mut self) {
        // A test that panicked while holding the state must not turn this into a double panic.
        let mut state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.dropped = true;
    }
}

/// The mock's error: a message and nothing else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MockError(pub String);

impl fmt::Display for MockError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Error for MockError {}

impl CaptureBackend for MockBackend {
    type Error = MockError;

    fn drain_events(&mut self, out: &mut Vec<SurfaceEvent>) -> Result<(), Self::Error> {
        let mut state = self.0.lock().expect("the mock state is not poisoned");
        if state.fail_drain {
            state.fail_drain = false;
            return Err(MockError("the display died".to_owned()));
        }
        out.append(&mut state.queue);
        Ok(())
    }

    fn root_size(&mut self) -> Result<Size, Self::Error> {
        Ok(Size::new(1400, 900))
    }

    fn capture(&mut self, id: SurfaceId, rect: Rect) -> Result<PixelBuffer, Self::Error> {
        let mut state = self.0.lock().expect("the mock state is not poisoned");
        if !state.surfaces.contains_key(&id.get()) {
            return Err(MockError(format!("no surface {}", id.get())));
        }
        Ok(match state.capture_faults.pop_front() {
            None => paint(id.get(), rect),
            Some(CaptureFault::Narrower) => {
                let narrower = Size::new(rect.size.width.saturating_sub(1), rect.size.height);
                paint(
                    id.get(),
                    Rect {
                        size: narrower,
                        ..rect
                    },
                )
            }
            Some(CaptureFault::Empty) => PixelBuffer {
                size: rect.size,
                stride: 0,
                format: PixelFormat::Bgrx8888,
                data: Vec::new(),
            },
        })
    }
}

impl InputSink for MockBackend {
    type Error = MockError;

    fn pointer_motion(&mut self, id: SurfaceId, at: Point) -> Result<(), Self::Error> {
        let damage = self
            .0
            .lock()
            .expect("the mock state is not poisoned")
            .motion_damage;
        if let Some(pause) = damage {
            // The pause stands for the display round trip; it runs on the backend's thread,
            // as a real one would.
            std::thread::sleep(pause);
            self.0
                .lock()
                .expect("the mock state is not poisoned")
                .queue
                .push(SurfaceEvent::Damaged {
                    id,
                    rect: Rect::new(0, 0, 16, 16),
                });
        }
        self.record(Input::Motion {
            surface: id.get(),
            at,
        });
        Ok(())
    }

    fn pointer_button(
        &mut self,
        id: SurfaceId,
        button: PointerButton,
        state: PressState,
    ) -> Result<(), Self::Error> {
        self.record(Input::Button {
            surface: id.get(),
            button,
            state,
        });
        Ok(())
    }

    fn pointer_axis(&mut self, id: SurfaceId, steps: Point) -> Result<(), Self::Error> {
        self.record(Input::Axis {
            surface: id.get(),
            steps,
        });
        Ok(())
    }

    fn key(&mut self, key: KeyEvent) -> Result<(), Self::Error> {
        self.record(Input::Key { key });
        Ok(())
    }

    fn focus(&mut self, id: SurfaceId) -> Result<(), Self::Error> {
        self.record(Input::Focus { surface: id.get() });
        Ok(())
    }

    fn blur(&mut self) -> Result<(), Self::Error> {
        self.record(Input::Blur);
        Ok(())
    }

    /// Records the configure and reports the app applying it, as a real backend would once
    /// the app had taken the size.
    fn configure(&mut self, id: SurfaceId, size: Size) -> Result<(), Self::Error> {
        let mut state = self.0.lock().expect("the mock state is not poisoned");
        state.surfaces.insert(id.get(), size);
        state.input.push(Input::Configure {
            surface: id.get(),
            size,
        });
        state.queue.push(SurfaceEvent::Resized { id, size });
        Ok(())
    }

    fn close(&mut self, id: SurfaceId) -> Result<(), Self::Error> {
        self.record(Input::Close { surface: id.get() });
        Ok(())
    }

    fn clipboard_set(&mut self, text: &str) -> Result<(), Self::Error> {
        self.record(Input::Clipboard {
            text: text.to_owned(),
        });
        Ok(())
    }
}

impl MockBackend {
    fn record(&mut self, input: Input) {
        self.0
            .lock()
            .expect("the mock state is not poisoned")
            .input
            .push(input);
    }
}

/// Paints `rect` of surface `id` with a deterministic pattern.
///
/// Every pixel's blue, green and red are functions of `(id, x, y)`; the unused byte is 0xFF,
/// which survives every codec round trip.
fn paint(id: u32, rect: Rect) -> PixelBuffer {
    let width = rect.size.width;
    let height = rect.size.height;
    let mut data = Vec::with_capacity(4 * width as usize * height as usize);
    for y in 0..height {
        for x in 0..width {
            let b = deterministic(id, x, y, 1);
            let g = deterministic(id, x, y, 2);
            let r = deterministic(id, x, y, 3);
            data.extend_from_slice(&[b, g, r, 0xFF]);
        }
    }
    PixelBuffer {
        size: rect.size,
        stride: 4 * width as usize,
        format: PixelFormat::Bgrx8888,
        data,
    }
}

fn deterministic(id: u32, x: u32, y: u32, channel: u32) -> u8 {
    let v = id
        .wrapping_mul(7919)
        .wrapping_add(x.wrapping_mul(104_729))
        .wrapping_add(y.wrapping_mul(129_9709))
        .wrapping_add(channel.wrapping_mul(15_498_689));
    u8::try_from(v % 251).expect("below 251")
}
