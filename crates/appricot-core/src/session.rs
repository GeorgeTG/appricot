//! One streamer session: the surface set under the wire caps, and the frames planned over it.
//!
//! A [`Session`] is a pure state machine fed from two sides. The backend side is
//! [`Session::apply_event`], which takes what a backend reported and updates the set. The host
//! side is everything else: [`Session::configure`], [`Session::frame_ack`],
//! [`Session::close_request`], and the detach and resume pair that models a socket dying and
//! a client coming back. Both sides are decisions, not I/O: the session never opens anything,
//! never waits, and never reads a clock, so a streamer drives it from its own event loop and
//! every rule here is deterministic and unit-testable. How long a detached session waits for
//! its client is the streamer's to decide and to count.
//!
//! What comes out is [`SessionEvent`]: one variant per server-to-client message the session
//! itself decides. The streamer maps them onto the wire; core keeps no wire vocabulary beyond
//! the bounded strings a title and an app id already are.
//!
//! The caps are the wire limits table's own rows, re-exported from [`appricot_proto::limits`],
//! so a number is changed in one place and the session honours what the streamer advertises.

pub use appricot_proto::limits::{MAX_FRAME_CREDITS, MAX_POPUPS_PER_PARENT, MAX_SURFACES};

use appricot_proto::limits::{
    AppId, MAX_CURSOR_HEIGHT, MAX_CURSOR_WIDTH, MAX_SURFACE_HEIGHT, MAX_SURFACE_WIDTH, Title,
};

use crate::capture::SurfaceEvent;
use crate::frame::FrameCredits;
use crate::geometry::{Rect, Size};
use crate::pixels::CursorImage;
use crate::role::{Positioner, Role};
use crate::surface::{ConfigureSerial, Resolution, Scale, Surface, SurfaceId};

/// Why a surface is gone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GoneReason {
    /// The app closed the window itself.
    AppClosed,
    /// Its parent went first; popups do not outlive parents.
    ParentGone,
}

/// What the session decided the streamer must put on the wire, one variant per
/// server-to-client message it owns.
///
/// Plain data: the streamer maps each variant onto the wire (core keeps no wire vocabulary
/// beyond the bounded strings a title and an app id already are). The enum is exhaustive on
/// purpose: a variant added here fails every match that has not yet decided what it means on
/// the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionEvent {
    /// A surface appeared and becomes a window the host shows.
    SurfaceNew {
        /// The surface.
        id: SurfaceId,
        /// What it is for.
        role: Role,
        /// A popup's or a dialog's parent; `None` for a plain toplevel, and for a dialog
        /// whose parent went first — dialogs do not follow their parents, so the
        /// announcement names a parent only while it lives.
        parent: Option<SurfaceId>,
        /// The size the surface has as of now.
        size: Size,
        /// The title as of now: empty until a metadata event fills it.
        title: Title,
        /// The app id as of now: empty until a metadata event fills it.
        app_id: AppId,
        /// How a popup is placed inside its parent, with the size the popup has as of now;
        /// `None` for a toplevel.
        positioner: Option<Positioner>,
        /// Device pixels per logical pixel.
        scale: Scale,
    },
    /// The surface is gone; its id is never reused.
    SurfaceGone {
        /// The surface.
        id: SurfaceId,
        /// Why it went.
        reason: GoneReason,
    },
    /// Title, app id or scale changed; absent fields are unchanged.
    SurfaceMetadata {
        /// The surface.
        id: SurfaceId,
        /// The new title.
        title: Option<Title>,
        /// The new app id.
        app_id: Option<AppId>,
        /// The new scale. No wave-1 feed reports one, so this is always `None` yet.
        scale: Option<Scale>,
    },
    /// The app asked for the keyboard focus. The host decides; the streamer reports.
    FocusAsk {
        /// The surface that asked.
        id: SurfaceId,
    },
    /// The app asked to resize itself. The host decides; the streamer reports. Nothing
    /// changes until the host answers with a configure.
    ResizeAsk {
        /// The surface that asked.
        id: SurfaceId,
        /// The size it asked for, cut to the wire's surface caps.
        size: Size,
    },
    /// The cursor image changed. One cursor per session, not per surface.
    ///
    /// `cursor.serial` is the session's own count, one more for every image it hands out,
    /// even an image the app showed before: a backend's serial is at most a cache key, and
    /// the client drops an image whose serial is not above the last one it drew.
    CursorChanged {
        /// The new cursor image.
        cursor: CursorImage,
    },
    /// No cursor image is known: the host draws its own cursor. A resume says this when the
    /// session never saw a cursor.
    CursorGone,
    /// The app pasted and the backend holds no clipboard text. The host decides what, if
    /// anything, to send back.
    ClipboardAsk,
    /// The app took a size, answering the configure named by the serial and every older
    /// configure of the surface still waiting. `size` may be the app's own clamp of what was
    /// proposed; it is the surface's size from here on.
    ConfigureAcked {
        /// The surface.
        id: SurfaceId,
        /// The newest configure the size answers.
        serial: ConfigureSerial,
        /// The size the app really took.
        size: Size,
    },
    /// The surface's size changed with no configure waiting: the app resized it on its own
    /// (a popup growing itself, say). It is a fact, not a request: the surface has this size
    /// from here on, and the client must learn it before any frame at the new size.
    Resized {
        /// The surface.
        id: SurfaceId,
        /// The new size, cut to the wire's surface caps.
        size: Size,
    },
}

/// One frame the session decided to send.
///
/// Built by [`Session::plan_frame`]; the streamer cuts its rectangles into tiles and encodes
/// them. A frame that is never sent goes back through [`Session::abort_frame`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FramePlan {
    /// The per-surface sequence, starting at 1 and rising by one. The client's ack names it.
    pub sequence: u32,
    /// True when the client must drop every tile it holds and this frame's rectangles cover
    /// the whole surface: the first frame of a surface, and the first after a resume.
    pub full_redraw: bool,
    /// What changed, in surface coordinates.
    pub rects: Vec<Rect>,
}

/// The frame bookkeeping of one tracked surface.
#[derive(Debug, Clone)]
struct FrameState {
    /// The sequence of the last frame planned; the next is one more.
    next_sequence: u32,
    /// The sequences sent and not yet acked, at most [`MAX_FRAME_CREDITS`] of them.
    outstanding: Vec<u32>,
    /// The pacing itself.
    credits: FrameCredits,
    /// Whether the next planned frame must be a full redraw.
    needs_full_redraw: bool,
}

impl FrameState {
    /// The state of a surface nobody has framed yet: no sequences in flight, full credits,
    /// and a first frame that must be complete.
    fn fresh() -> Self {
        Self {
            next_sequence: 0,
            outstanding: Vec::new(),
            // The limits table keeps the count tiny; the conversion cannot fail.
            credits: FrameCredits::new(u32::try_from(MAX_FRAME_CREDITS).unwrap_or(u32::MAX)),
            needs_full_redraw: true,
        }
    }
}

/// One living surface, and what the session keeps beside it.
#[derive(Debug, Clone)]
struct Tracked {
    /// The surface itself.
    surface: Surface,
    /// The title as of the last metadata event.
    title: Title,
    /// The app id as of the last metadata event.
    app_id: AppId,
    /// Frame pacing for this surface.
    frames: FrameState,
}

/// The surface set of one streamer session, and the decisions over it.
///
/// A session is fed from two sides — the backend's [`SurfaceEvent`]s through
/// [`Session::apply_event`], and the host's window requests through [`Session::configure`],
/// [`Session::frame_ack`] and [`Session::close_request`] — and it decides what the streamer
/// must put on the wire ([`SessionEvent`]) and which frame goes next ([`Session::plan_frame`]).
///
/// # Caps and ids
///
/// At most [`MAX_SURFACES`] surfaces live, and one parent keeps at most
/// [`MAX_POPUPS_PER_PARENT`] popups. A `Created` past a cap is refused — not tracked, never
/// announced — and every later event about a refused surface is dropped, like any event about
/// an unknown id. Ids rise strictly, as the [`CaptureBackend`](crate::CaptureBackend) contract
/// requires: a `Created` whose id is not above every id the session accepted before is
/// refused, so no id is ever reused and the session remembers one number, not every id. A
/// popup whose parent is not living is refused; when a surface goes, its popups go with it,
/// and theirs after them, each reported [`GoneReason::ParentGone`]. A dialog's parent — a
/// toplevel naming another toplevel, X11's `WM_TRANSIENT_FOR` — is advice, not a lifetime
/// link: it is kept only while it names a living toplevel, and it never takes the dialog with
/// it when it goes.
///
/// Every size a backend reports is cut to the wire's `MAX_SURFACE_WIDTH` x
/// `MAX_SURFACE_HEIGHT`: a larger window is streamed as its top-left part, instead of failing
/// the encode of the message that would announce it. A cursor image the wire cannot carry is
/// dropped, and the host keeps the last one.
///
/// # Configure and ack
///
/// The host proposes sizes with [`Session::configure`]; each surface keeps a short queue of
/// the proposals the app has not answered yet. When the backend reports the size the app
/// took, the newest proposal of that size is acked ([`SessionEvent::ConfigureAcked`]), and
/// every older one with it; when none has that size (the app clamped), the newest is acked.
/// A proposal of the size the surface already has, with nothing waiting, is acked at once. A
/// size change with nothing waiting is the app's own ([`SessionEvent::Resized`]), and a report
/// that changes nothing with nothing waiting says nothing.
///
/// # Detach and resume
///
/// [`Session::detach`] models the socket dying: nothing is planned and nothing is emitted
/// while the session waits, but state moves on. [`Session::resume`] models the client coming
/// back: the whole living window set is announced again, then the cursor, every surface is
/// marked for a full redraw, and each surface's frame pacing and waiting configures start
/// over, because the new connection holds nothing in flight.
#[derive(Debug, Clone)]
pub struct Session {
    /// The living surfaces, in creation order.
    surfaces: Vec<Tracked>,
    /// The highest id ever accepted; a `Created` must name a higher one.
    highest_id: Option<SurfaceId>,
    /// The last configure serial handed out; the next is one more, wrapping past 0.
    configure_serial: u32,
    /// The cursor image as the host last saw it, or would have while detached, carrying the
    /// serial the session gave it.
    cursor: Option<CursorImage>,
    /// The last cursor serial handed out; the next is one more, wrapping past 0.
    cursor_serial: u32,
    /// Whether the socket is gone and the session waits for its client.
    detached: bool,
}

impl Session {
    /// An empty, attached session.
    pub fn new() -> Self {
        Self {
            surfaces: Vec::new(),
            highest_id: None,
            configure_serial: 0,
            cursor: None,
            cursor_serial: 0,
            detached: false,
        }
    }

    /// Applies one backend event and appends what the streamer must send.
    ///
    /// Events about unknown or refused surfaces are dropped silently. While the session is
    /// detached nothing is appended: state still moves on, and [`Session::resume`]
    /// re-synchronises the client with the whole window set and the cursor. A clipboard ask or
    /// a focus ask made while detached is not replayed: the backend refused that paste at
    /// once, and the app's next paste asks again. Damage is never an event; it comes out
    /// through [`Session::plan_frame`].
    pub fn apply_event(&mut self, ev: SurfaceEvent, out: &mut Vec<SessionEvent>) {
        match ev {
            SurfaceEvent::Created {
                id,
                role,
                size,
                parent,
            } => self.apply_created(id, role, size, parent, out),
            SurfaceEvent::Metadata { id, title, app_id } => {
                let Some(tracked) = self.tracked_mut(id) else {
                    return;
                };
                if tracked.title == title && tracked.app_id == app_id {
                    return;
                }
                let event = SessionEvent::SurfaceMetadata {
                    id,
                    title: Some(title.clone()),
                    app_id: Some(app_id.clone()),
                    scale: None,
                };
                tracked.title = title;
                tracked.app_id = app_id;
                self.emit(out, event);
            }
            SurfaceEvent::Damaged { id, rect } => {
                if let Some(tracked) = self.tracked_mut(id) {
                    tracked.surface.add_damage(rect);
                }
            }
            SurfaceEvent::Resized { id, size } => {
                let size = clamp_to_wire(size);
                let Some(tracked) = self.tracked_mut(id) else {
                    return;
                };
                match tracked.surface.resolve(size) {
                    Resolution::Acked(serial) => {
                        self.emit(out, SessionEvent::ConfigureAcked { id, serial, size });
                    }
                    Resolution::Unprompted => self.emit(out, SessionEvent::Resized { id, size }),
                    Resolution::Unchanged => {}
                }
            }
            SurfaceEvent::Destroyed { id } => self.destroy(id, GoneReason::AppClosed, out),
            SurfaceEvent::FocusRequested { id } => {
                if self.tracked(id).is_some() {
                    self.emit(out, SessionEvent::FocusAsk { id });
                }
            }
            SurfaceEvent::ResizeRequested { id, size } => {
                if self.tracked(id).is_some() {
                    let size = clamp_to_wire(size);
                    self.emit(out, SessionEvent::ResizeAsk { id, size });
                }
            }
            SurfaceEvent::CursorChanged { cursor } => self.apply_cursor(cursor, out),
            SurfaceEvent::ClipboardRequested => {
                self.emit(out, SessionEvent::ClipboardAsk);
            }
        }
    }

    /// Proposes `size` for surface `id`, and returns the serial that names the proposal, or
    /// `None` for an unknown id, where the request is dropped like every other and spends no
    /// serial.
    ///
    /// Serials come from one session-wide counter, so they rise across surfaces. The
    /// surface's size changes only when the app's answer arrives — a `Resized` backend event,
    /// which the session answers with [`SessionEvent::ConfigureAcked`] carrying the size the
    /// app really took. A proposal of the size the surface already has, with no other
    /// proposal waiting, changes nothing, so it is acked at once: the ack is appended to
    /// `out` before this returns. `size` is cut to the wire's surface caps first.
    pub fn configure(
        &mut self,
        id: SurfaceId,
        size: Size,
        out: &mut Vec<SessionEvent>,
    ) -> Option<ConfigureSerial> {
        self.tracked(id)?;
        let size = clamp_to_wire(size);
        self.configure_serial = self.configure_serial.wrapping_add(1).max(1);
        let serial = ConfigureSerial::new(self.configure_serial);
        if self.tracked_mut(id)?.surface.propose(serial, size) {
            self.emit(out, SessionEvent::ConfigureAcked { id, serial, size });
        }
        Some(serial)
    }

    /// Records that the client drew the frame named by `sequence`, freeing its credit.
    ///
    /// An ack for a sequence not in flight — unknown, stale, or already acked — changes
    /// nothing and is never fatal; the wire promises the client the same mercy in return.
    pub fn frame_ack(&mut self, id: SurfaceId, sequence: u32) {
        let Some(tracked) = self.tracked_mut(id) else {
            return;
        };
        if let Some(at) = tracked
            .frames
            .outstanding
            .iter()
            .position(|&s| s == sequence)
        {
            tracked.frames.outstanding.remove(at);
            tracked.frames.credits.ack();
        }
    }

    /// Plans the next frame of surface `id`, taking its damage.
    ///
    /// `Some` comes back only when the session is attached, the surface lives, has damage,
    /// and a credit is free ([`MAX_FRAME_CREDITS`] in flight per surface). The rectangles
    /// leave the surface, so damage gathered while no credit is free coalesces into the next
    /// frame instead of piling up. `full_redraw` marks the first frame of a surface and the
    /// first planned after a [`Session::resume`].
    ///
    /// The plan commits its credit, its sequence and its damage at once. A plan the caller
    /// cannot send goes back through [`Session::abort_frame`].
    pub fn plan_frame(&mut self, id: SurfaceId) -> Option<FramePlan> {
        if self.detached {
            return None;
        }
        let tracked = self.surfaces.iter_mut().find(|t| t.surface.id() == id)?;
        if !tracked.surface.has_damage() || !tracked.frames.credits.try_send() {
            return None;
        }
        tracked.frames.next_sequence = tracked.frames.next_sequence.wrapping_add(1);
        let sequence = tracked.frames.next_sequence;
        tracked.frames.outstanding.push(sequence);
        let full_redraw = tracked.frames.needs_full_redraw;
        tracked.frames.needs_full_redraw = false;
        let rects = tracked.surface.take_damage();
        Some(FramePlan {
            sequence,
            full_redraw,
            rects,
        })
    }

    /// Hands back a frame planned for surface `id` that was never sent — every tile capture
    /// failed, say — as if it had never been planned.
    ///
    /// Only the plan returned last for the surface can come back. Its credit is freed, its
    /// sequence is not spent (the next plan reuses it, so the client sees no gap), its damage
    /// is restored, and a full redraw it carried is owed again. Returns whether the plan was
    /// taken back: false, with nothing changed, for any other plan or a surface that is gone.
    pub fn abort_frame(&mut self, id: SurfaceId, plan: &FramePlan) -> bool {
        let Some(tracked) = self.tracked_mut(id) else {
            return false;
        };
        let frames = &mut tracked.frames;
        if frames.next_sequence != plan.sequence
            || frames.outstanding.last() != Some(&plan.sequence)
        {
            return false;
        }
        frames.outstanding.pop();
        frames.credits.ack();
        frames.next_sequence = frames.next_sequence.wrapping_sub(1);
        if plan.full_redraw {
            frames.needs_full_redraw = true;
            tracked.surface.invalidate();
        } else {
            for rect in &plan.rects {
                tracked.surface.add_damage(*rect);
            }
        }
        true
    }

    /// Takes the host's request that the app close surface `id`.
    ///
    /// Returns whether the surface lives, so the streamer asks the backend only when it
    /// must. Nothing changes here: the app decides, and the surface goes when the backend
    /// reports it gone.
    pub fn close_request(&mut self, id: SurfaceId) -> bool {
        self.tracked(id).is_some()
    }

    /// Records the socket dying: planning and emission stop until [`Session::resume`], while
    /// the state still follows the backend.
    ///
    /// How long the session waits for its client is the streamer's to decide and to count;
    /// the session keeps no clock.
    pub fn detach(&mut self) {
        self.detached = true;
    }

    /// Records the client reattaching, and re-synchronises it, in this order:
    ///
    /// 1. [`SessionEvent::SurfaceNew`] for every living surface, in creation order, as it
    ///    stands: its size, metadata, scale, role, parent and positioner;
    /// 2. exactly one cursor event: [`SessionEvent::CursorChanged`] with the current image
    ///    under a new serial, or [`SessionEvent::CursorGone`] when no cursor is known.
    ///
    /// Every surface is marked for a full redraw and its frame pacing starts over — the new
    /// connection holds nothing in flight — so the first frame planned after this carries the
    /// whole surface at sequence 1. Configures still waiting are forgotten: the connection
    /// that proposed them is gone, and the re-announced size is the truth.
    pub fn resume(&mut self, out: &mut Vec<SessionEvent>) {
        self.detached = false;
        for tracked in &mut self.surfaces {
            tracked.frames = FrameState::fresh();
            tracked.surface.invalidate();
            tracked.surface.forget_pending();
        }
        for tracked in &self.surfaces {
            out.push(self.surface_new_event(tracked));
        }
        let cursor = match self.cursor.take() {
            Some(mut cursor) => {
                cursor.serial = self.next_cursor_serial();
                self.cursor = Some(cursor.clone());
                SessionEvent::CursorChanged { cursor }
            }
            None => SessionEvent::CursorGone,
        };
        out.push(cursor);
    }

    /// Whether the socket is gone and the session waits for its client.
    pub fn is_detached(&self) -> bool {
        self.detached
    }

    /// How many surfaces live.
    pub fn surface_count(&self) -> usize {
        self.surfaces.len()
    }

    /// The surface itself, for an id the session tracks.
    pub fn surface(&self, id: SurfaceId) -> Option<&Surface> {
        self.tracked(id).map(|tracked| &tracked.surface)
    }

    /// Tracks a surface the backend just reported, under the caps, and announces it.
    ///
    /// Refused — tracked nowhere, announced nowhere — when its id is not above every id
    /// accepted before, when a popup has no living parent or its parent already keeps
    /// [`MAX_POPUPS_PER_PARENT`] popups, or when [`MAX_SURFACES`] surfaces already live. The
    /// size, and a popup's positioner size, are cut to the wire's surface caps.
    ///
    /// `parent` is a dialog's toplevel parent (a popup's parent lives in its role, never
    /// here). It is advice, not a lifetime link: the session keeps it only while it names a
    /// living toplevel — a parent naming nothing, or the surface itself, is dropped, and the
    /// surface is still tracked and announced, unlike a popup without a parent. A dialog
    /// does not go when its parent goes; only popups cascade.
    fn apply_created(
        &mut self,
        id: SurfaceId,
        role: Role,
        size: Size,
        parent: Option<SurfaceId>,
        out: &mut Vec<SessionEvent>,
    ) {
        if self.highest_id.is_some_and(|highest| id <= highest) {
            return;
        }
        if let Role::Popup { parent, .. } = role {
            if parent == id || self.tracked(parent).is_none() {
                return;
            }
            if self.popup_count(parent) >= MAX_POPUPS_PER_PARENT {
                return;
            }
        }
        if self.surfaces.len() >= MAX_SURFACES {
            return;
        }
        self.highest_id = Some(id);
        let role = match role {
            Role::Popup { parent, positioner } => Role::Popup {
                parent,
                positioner: Positioner {
                    size: clamp_to_wire(positioner.size),
                    ..positioner
                },
            },
            Role::Toplevel => Role::Toplevel,
        };
        // A dialog's parent is kept only while it names a living toplevel — the id itself
        // cannot be one yet, so the self case falls out of the same check, and
        // `set_toplevel_parent` refuses a popup's (whose parent is its role's) regardless.
        let parent = parent.filter(|parent| self.is_living_toplevel(*parent));
        let mut surface = Surface::new(id, role, clamp_to_wire(size), Scale::ONE);
        surface.set_toplevel_parent(parent);
        let tracked = Tracked {
            surface,
            title: Title::default(),
            app_id: AppId::default(),
            frames: FrameState::fresh(),
        };
        let event = self.surface_new_event(&tracked);
        self.surfaces.push(tracked);
        self.emit(out, event);
    }

    /// Takes a cursor image from the backend and forwards it under the session's own serial.
    ///
    /// An image the wire cannot carry — over the cursor caps, or with a pixel buffer that is
    /// not exactly `width * height * 4` bytes — is dropped, and so is an image identical to
    /// the one the host already has: neither changes what the host should draw.
    fn apply_cursor(&mut self, mut cursor: CursorImage, out: &mut Vec<SessionEvent>) {
        let fits = cursor.size.width <= MAX_CURSOR_WIDTH
            && cursor.size.height <= MAX_CURSOR_HEIGHT
            && CursorImage::byte_len(cursor.size) == Some(cursor.argb.len());
        if !fits {
            return;
        }
        let same = self.cursor.as_ref().is_some_and(|last| {
            last.size == cursor.size && last.hotspot == cursor.hotspot && last.argb == cursor.argb
        });
        if same {
            return;
        }
        cursor.serial = self.next_cursor_serial();
        self.cursor = Some(cursor.clone());
        self.emit(out, SessionEvent::CursorChanged { cursor });
    }

    /// The next cursor serial: one per image handed out, rising, never 0.
    fn next_cursor_serial(&mut self) -> u32 {
        self.cursor_serial = self.cursor_serial.wrapping_add(1).max(1);
        self.cursor_serial
    }

    /// Removes surface `id`, announces it gone, and takes its popups with it — and theirs,
    /// each [`GoneReason::ParentGone`]. An unknown id removes nothing.
    fn destroy(&mut self, id: SurfaceId, reason: GoneReason, out: &mut Vec<SessionEvent>) {
        let Some(at) = self.surfaces.iter().position(|t| t.surface.id() == id) else {
            return;
        };
        self.surfaces.remove(at);
        self.emit(out, SessionEvent::SurfaceGone { id, reason });
        let children: Vec<SurfaceId> = self
            .surfaces
            .iter()
            .filter(|t| matches!(t.surface.role(), Role::Popup { parent: p, .. } if p == id))
            .map(|t| t.surface.id())
            .collect();
        for child in children {
            self.destroy(child, GoneReason::ParentGone, out);
        }
    }

    /// How many living popups name `parent`.
    fn popup_count(&self, parent: SurfaceId) -> usize {
        self.surfaces
            .iter()
            .filter(|t| matches!(t.surface.role(), Role::Popup { parent: p, .. } if p == parent))
            .count()
    }

    /// The tracked record of an id the session keeps.
    fn tracked(&self, id: SurfaceId) -> Option<&Tracked> {
        self.surfaces.iter().find(|t| t.surface.id() == id)
    }

    /// The tracked record of an id the session keeps, mutably.
    fn tracked_mut(&mut self, id: SurfaceId) -> Option<&mut Tracked> {
        self.surfaces.iter_mut().find(|t| t.surface.id() == id)
    }

    /// True when `id` names a living toplevel of this session: the only thing a dialog's
    /// parent may be. A popup is nobody's dialog parent, and a gone surface is neither.
    fn is_living_toplevel(&self, id: SurfaceId) -> bool {
        self.tracked(id)
            .is_some_and(|tracked| matches!(tracked.surface.role(), Role::Toplevel))
    }

    /// Appends `event` unless the session is detached, in which case the state has moved on
    /// but the client learns it from the next [`Session::resume`].
    fn emit(&self, out: &mut Vec<SessionEvent>, event: SessionEvent) {
        if !self.detached {
            out.push(event);
        }
    }
}

impl Default for Session {
    /// An empty, attached session.
    fn default() -> Self {
        Self::new()
    }
}

impl Session {
    /// Builds the announcement of a tracked surface as it stands now.
    ///
    /// A popup announces the parent its role fixed, and its positioner with the size the
    /// popup has now: a popup that resized itself is placed at its new size. A toplevel
    /// announces the parent it was created with while that still names a living toplevel:
    /// dialogs do not follow their parents, so one can outlive its parent, and the
    /// announcement after that carries none — the wire never names a surface the session does
    /// not track.
    fn surface_new_event(&self, tracked: &Tracked) -> SessionEvent {
        let size = tracked.surface.size();
        let role = match tracked.surface.role() {
            Role::Popup { parent, positioner } => Role::Popup {
                parent,
                positioner: Positioner { size, ..positioner },
            },
            Role::Toplevel => Role::Toplevel,
        };
        let parent = match role {
            Role::Popup { parent, .. } => Some(parent),
            Role::Toplevel => tracked
                .surface
                .parent()
                .filter(|parent| self.is_living_toplevel(*parent)),
        };
        SessionEvent::SurfaceNew {
            id: tracked.surface.id(),
            role,
            parent,
            size,
            title: tracked.title.clone(),
            app_id: tracked.app_id.clone(),
            positioner: match role {
                Role::Popup { positioner, .. } => Some(positioner),
                Role::Toplevel => None,
            },
            scale: tracked.surface.scale(),
        }
    }
}

/// `size`, cut to the wire's surface caps (`MAX_SURFACE_WIDTH` x `MAX_SURFACE_HEIGHT`).
///
/// Tiles are cut inside a surface's bounds, so a window larger than the caps is streamed as
/// its top-left part rather than failing the encode that would end the session.
fn clamp_to_wire(size: Size) -> Size {
    Size::new(
        size.width.min(MAX_SURFACE_WIDTH),
        size.height.min(MAX_SURFACE_HEIGHT),
    )
}

#[cfg(test)]
mod tests {
    use appricot_proto::limits::{AppId, Title};

    use crate::{
        CursorImage, GoneReason, MAX_FRAME_CREDITS, MAX_POPUPS_PER_PARENT, MAX_SURFACES, Point,
        Positioner, Rect, Role, Scale, Session, SessionEvent, Size, SurfaceEvent, SurfaceId,
    };

    /// The credit count as the frame sequences count it.
    fn credits() -> u32 {
        u32::try_from(MAX_FRAME_CREDITS).expect("the limits table keeps it tiny")
    }

    fn created(id: u32) -> SurfaceEvent {
        SurfaceEvent::Created {
            id: SurfaceId::new(id),
            role: Role::Toplevel,
            size: Size::new(400, 300),
            parent: None,
        }
    }

    fn popup_created(id: u32, parent: u32) -> SurfaceEvent {
        SurfaceEvent::Created {
            id: SurfaceId::new(id),
            role: Role::Popup {
                parent: SurfaceId::new(parent),
                positioner: Positioner::at(Point::new(5, 5), Size::new(80, 40)),
            },
            size: Size::new(80, 40),
            parent: None,
        }
    }

    fn dialog_created(id: u32, parent: u32) -> SurfaceEvent {
        SurfaceEvent::Created {
            id: SurfaceId::new(id),
            role: Role::Toplevel,
            size: Size::new(200, 100),
            parent: Some(SurfaceId::new(parent)),
        }
    }

    fn damaged(id: u32, x: i32) -> SurfaceEvent {
        SurfaceEvent::Damaged {
            id: SurfaceId::new(id),
            rect: Rect::new(x, 0, 10, 10),
        }
    }

    fn metadata(id: u32, name: &str) -> SurfaceEvent {
        SurfaceEvent::Metadata {
            id: SurfaceId::new(id),
            title: Title::new(name).expect("short title"),
            app_id: AppId::new("app").expect("short app id"),
        }
    }

    fn title(s: &str) -> Title {
        Title::new(s).expect("short title")
    }

    fn app_id(s: &str) -> AppId {
        AppId::new(s).expect("short app id")
    }

    fn announced_toplevel(id: u32) -> SessionEvent {
        SessionEvent::SurfaceNew {
            id: SurfaceId::new(id),
            role: Role::Toplevel,
            parent: None,
            size: Size::new(400, 300),
            title: Title::default(),
            app_id: AppId::default(),
            positioner: None,
            scale: Scale::ONE,
        }
    }

    fn announced_dialog(id: u32, parent: Option<u32>) -> SessionEvent {
        SessionEvent::SurfaceNew {
            id: SurfaceId::new(id),
            role: Role::Toplevel,
            parent: parent.map(SurfaceId::new),
            size: Size::new(200, 100),
            title: Title::default(),
            app_id: AppId::default(),
            positioner: None,
            scale: Scale::ONE,
        }
    }

    #[test]
    fn a_surface_lives_created_metadata_resized_destroyed() {
        let mut s = Session::new();
        let mut out = Vec::new();
        s.apply_event(created(1), &mut out);
        s.apply_event(metadata(1, "Notes"), &mut out);
        // The same metadata again says nothing new.
        s.apply_event(metadata(1, "Notes"), &mut out);
        let serial = s
            .configure(SurfaceId::new(1), Size::new(800, 600), &mut out)
            .expect("lives");
        s.apply_event(
            SurfaceEvent::Resized {
                id: SurfaceId::new(1),
                size: Size::new(800, 500),
            },
            &mut out,
        );
        s.apply_event(
            SurfaceEvent::Resized {
                id: SurfaceId::new(1),
                size: Size::new(1024, 768),
            },
            &mut out,
        );
        s.apply_event(
            SurfaceEvent::Destroyed {
                id: SurfaceId::new(1),
            },
            &mut out,
        );
        assert_eq!(
            out,
            vec![
                announced_toplevel(1),
                SessionEvent::SurfaceMetadata {
                    id: SurfaceId::new(1),
                    title: Some(title("Notes")),
                    app_id: Some(app_id("app")),
                    scale: None,
                },
                SessionEvent::ConfigureAcked {
                    id: SurfaceId::new(1),
                    serial,
                    size: Size::new(800, 500),
                },
                SessionEvent::Resized {
                    id: SurfaceId::new(1),
                    size: Size::new(1024, 768),
                },
                SessionEvent::SurfaceGone {
                    id: SurfaceId::new(1),
                    reason: GoneReason::AppClosed,
                },
            ]
        );
    }

    #[test]
    fn a_resize_without_a_pending_configure_is_informational() {
        let mut s = Session::new();
        let mut out = Vec::new();
        s.apply_event(created(1), &mut out);
        out.clear();
        s.apply_event(
            SurfaceEvent::Resized {
                id: SurfaceId::new(1),
                size: Size::new(640, 480),
            },
            &mut out,
        );
        assert_eq!(
            out,
            vec![SessionEvent::Resized {
                id: SurfaceId::new(1),
                size: Size::new(640, 480),
            }]
        );
        assert_eq!(
            s.surface(SurfaceId::new(1)).expect("lives").size(),
            Size::new(640, 480)
        );
    }

    #[test]
    fn a_popup_is_announced_with_its_parent_and_positioner() {
        let mut s = Session::new();
        let mut out = Vec::new();
        s.apply_event(created(1), &mut out);
        out.clear();
        s.apply_event(popup_created(2, 1), &mut out);
        let positioner = Positioner::at(Point::new(5, 5), Size::new(80, 40));
        assert_eq!(
            out,
            vec![SessionEvent::SurfaceNew {
                id: SurfaceId::new(2),
                role: Role::Popup {
                    parent: SurfaceId::new(1),
                    positioner,
                },
                parent: Some(SurfaceId::new(1)),
                size: Size::new(80, 40),
                title: Title::default(),
                app_id: AppId::default(),
                positioner: Some(positioner),
                scale: Scale::ONE,
            }]
        );
    }

    #[test]
    fn a_dialog_is_announced_with_its_parent_and_a_dialog_may_have_one() {
        let mut s = Session::new();
        let mut out = Vec::new();
        s.apply_event(created(1), &mut out);
        out.clear();
        s.apply_event(dialog_created(2, 1), &mut out);
        // A dialog of the dialog: still a toplevel, still parented.
        s.apply_event(dialog_created(3, 2), &mut out);
        assert_eq!(
            out,
            vec![announced_dialog(2, Some(1)), announced_dialog(3, Some(2))]
        );
    }

    #[test]
    fn a_dialog_parent_that_names_no_living_toplevel_is_dropped_not_the_dialog() {
        let mut s = Session::new();
        let mut out = Vec::new();
        // The parent never came.
        s.apply_event(dialog_created(2, 1), &mut out);
        // The dialog names itself.
        s.apply_event(dialog_created(3, 3), &mut out);
        // The dialog names a popup: a popup is nobody's dialog parent.
        s.apply_event(created(4), &mut out);
        s.apply_event(popup_created(5, 4), &mut out);
        out.clear();
        s.apply_event(dialog_created(6, 5), &mut out);
        assert_eq!(out, vec![announced_dialog(6, None)]);
        assert_eq!(
            s.surface_count(),
            5,
            "every dialog is tracked, whatever it named"
        );
    }

    #[test]
    fn a_popups_parent_is_its_roles_even_when_the_event_field_names_another() {
        let mut s = Session::new();
        let mut out = Vec::new();
        s.apply_event(created(1), &mut out);
        s.apply_event(created(2), &mut out);
        out.clear();
        s.apply_event(
            SurfaceEvent::Created {
                id: SurfaceId::new(3),
                role: Role::Popup {
                    parent: SurfaceId::new(1),
                    positioner: Positioner::at(Point::new(5, 5), Size::new(80, 40)),
                },
                size: Size::new(80, 40),
                // A popup's parent lives in its role; this field is a toplevel's.
                parent: Some(SurfaceId::new(2)),
            },
            &mut out,
        );
        let positioner = Positioner::at(Point::new(5, 5), Size::new(80, 40));
        assert_eq!(
            out,
            vec![SessionEvent::SurfaceNew {
                id: SurfaceId::new(3),
                role: Role::Popup {
                    parent: SurfaceId::new(1),
                    positioner,
                },
                parent: Some(SurfaceId::new(1)),
                size: Size::new(80, 40),
                title: Title::default(),
                app_id: AppId::default(),
                positioner: Some(positioner),
                scale: Scale::ONE,
            }]
        );
    }

    #[test]
    fn a_dialog_does_not_follow_its_parent_and_announces_none_after_it() {
        let mut s = Session::new();
        let mut out = Vec::new();
        s.apply_event(created(1), &mut out);
        s.apply_event(dialog_created(2, 1), &mut out);
        out.clear();
        s.apply_event(
            SurfaceEvent::Destroyed {
                id: SurfaceId::new(1),
            },
            &mut out,
        );
        // Only the parent went; the dialog stays. A popup would have followed.
        assert_eq!(
            out,
            vec![SessionEvent::SurfaceGone {
                id: SurfaceId::new(1),
                reason: GoneReason::AppClosed,
            }]
        );
        assert_eq!(s.surface_count(), 1);

        // A resume re-announces the survivor with no parent: the link died with its target.
        out.clear();
        s.resume(&mut out);
        assert_eq!(
            out,
            vec![announced_dialog(2, None), SessionEvent::CursorGone]
        );
    }

    #[test]
    fn a_resume_re_announces_a_dialog_with_its_parent() {
        let mut s = Session::new();
        let mut out = Vec::new();
        s.apply_event(created(1), &mut out);
        s.apply_event(dialog_created(2, 1), &mut out);
        s.detach();
        out.clear();
        s.resume(&mut out);
        assert_eq!(
            out,
            vec![
                announced_toplevel(1),
                announced_dialog(2, Some(1)),
                SessionEvent::CursorGone,
            ]
        );
    }

    #[test]
    fn destroying_a_parent_takes_its_popups_and_theirs() {
        let mut s = Session::new();
        let mut out = Vec::new();
        s.apply_event(created(1), &mut out);
        s.apply_event(popup_created(2, 1), &mut out);
        // A popup of the popup.
        s.apply_event(popup_created(3, 2), &mut out);
        out.clear();
        s.apply_event(
            SurfaceEvent::Destroyed {
                id: SurfaceId::new(1),
            },
            &mut out,
        );
        assert_eq!(
            out,
            vec![
                SessionEvent::SurfaceGone {
                    id: SurfaceId::new(1),
                    reason: GoneReason::AppClosed,
                },
                SessionEvent::SurfaceGone {
                    id: SurfaceId::new(2),
                    reason: GoneReason::ParentGone,
                },
                SessionEvent::SurfaceGone {
                    id: SurfaceId::new(3),
                    reason: GoneReason::ParentGone,
                },
            ]
        );
        assert_eq!(s.surface_count(), 0);
    }

    #[test]
    fn the_first_frame_is_complete_and_the_rest_are_not() {
        let mut s = Session::new();
        s.apply_event(created(1), &mut Vec::new());
        let id = SurfaceId::new(1);
        let plan = s.plan_frame(id).expect("creation damage");
        assert_eq!(plan.sequence, 1);
        assert!(plan.full_redraw);
        assert_eq!(plan.rects, vec![Rect::new(0, 0, 400, 300)]);
        // Nothing changed since: no frame.
        assert_eq!(s.plan_frame(id), None);
        s.apply_event(damaged(1, 5), &mut Vec::new());
        let plan = s.plan_frame(id).expect("fresh damage");
        assert_eq!(plan.sequence, 2);
        assert!(!plan.full_redraw);
        assert_eq!(plan.rects, vec![Rect::new(5, 0, 10, 10)]);
    }

    #[test]
    fn frames_stop_at_the_credit_limit_until_an_ack_frees_one() {
        let mut s = Session::new();
        s.apply_event(created(1), &mut Vec::new());
        let id = SurfaceId::new(1);
        for expected in 1..=credits() {
            let plan = s.plan_frame(id).expect("a free credit");
            assert_eq!(plan.sequence, expected);
            if expected < credits() {
                s.apply_event(damaged(1, 0), &mut Vec::new());
            }
        }
        // Every credit is spent: damage waits, nothing is planned.
        s.apply_event(damaged(1, 0), &mut Vec::new());
        assert_eq!(s.plan_frame(id), None);
        s.frame_ack(id, 1);
        let plan = s.plan_frame(id).expect("the ack freed a credit");
        assert_eq!(plan.sequence, credits() + 1);
    }

    #[test]
    fn a_stale_or_duplicate_ack_changes_nothing() {
        let mut s = Session::new();
        s.apply_event(created(1), &mut Vec::new());
        let id = SurfaceId::new(1);
        assert_eq!(s.plan_frame(id).expect("first").sequence, 1);
        s.apply_event(damaged(1, 0), &mut Vec::new());
        assert_eq!(s.plan_frame(id).expect("second").sequence, 2);
        s.frame_ack(id, 99);
        s.frame_ack(id, 1);
        // The ack of 1 again must not free a second credit: exactly three more frames fit.
        s.frame_ack(id, 1);
        for expected in 3..=credits() + 1 {
            s.apply_event(damaged(1, 0), &mut Vec::new());
            assert_eq!(
                s.plan_frame(id).expect("one credit freed").sequence,
                expected
            );
        }
        s.apply_event(damaged(1, 0), &mut Vec::new());
        assert_eq!(s.plan_frame(id), None);
    }

    #[test]
    fn resume_re_announces_the_window_set_and_redraws_everything() {
        let mut s = Session::new();
        let mut out = Vec::new();
        s.apply_event(created(1), &mut out);
        s.apply_event(popup_created(2, 1), &mut out);
        s.apply_event(metadata(1, "Notes"), &mut out);
        out.clear();

        s.detach();
        // While detached the session tracks but tells nothing.
        s.apply_event(created(9), &mut out);
        s.apply_event(
            SurfaceEvent::Resized {
                id: SurfaceId::new(1),
                size: Size::new(640, 480),
            },
            &mut out,
        );
        assert!(out.is_empty());

        s.resume(&mut out);
        let popup_new = SessionEvent::SurfaceNew {
            id: SurfaceId::new(2),
            role: Role::Popup {
                parent: SurfaceId::new(1),
                positioner: Positioner::at(Point::new(5, 5), Size::new(80, 40)),
            },
            parent: Some(SurfaceId::new(1)),
            size: Size::new(80, 40),
            title: Title::default(),
            app_id: AppId::default(),
            positioner: Some(Positioner::at(Point::new(5, 5), Size::new(80, 40))),
            scale: Scale::ONE,
        };
        assert_eq!(
            out,
            vec![
                SessionEvent::SurfaceNew {
                    id: SurfaceId::new(1),
                    role: Role::Toplevel,
                    parent: None,
                    size: Size::new(640, 480),
                    title: title("Notes"),
                    app_id: app_id("app"),
                    positioner: None,
                    scale: Scale::ONE,
                },
                popup_new,
                announced_toplevel(9),
                SessionEvent::CursorGone,
            ]
        );
        let plan = s
            .plan_frame(SurfaceId::new(1))
            .expect("resume damaged everything");
        assert_eq!(plan.sequence, 1);
        assert!(plan.full_redraw);
        assert_eq!(plan.rects, vec![Rect::new(0, 0, 640, 480)]);
    }

    #[test]
    fn a_detached_session_plans_nothing() {
        let mut s = Session::new();
        s.apply_event(created(1), &mut Vec::new());
        s.detach();
        assert!(s.is_detached());
        assert_eq!(s.plan_frame(SurfaceId::new(1)), None);
    }

    #[test]
    fn the_sixty_fifth_surface_is_refused() {
        let mut s = Session::new();
        let mut out = Vec::new();
        let mut next_id = 1_u32;
        while s.surface_count() < MAX_SURFACES {
            s.apply_event(created(next_id), &mut out);
            next_id += 1;
        }
        assert_eq!(out.len(), MAX_SURFACES);
        out.clear();
        s.apply_event(created(next_id), &mut out);
        s.apply_event(metadata(next_id, "No"), &mut out);
        assert!(out.is_empty());
        assert_eq!(s.surface_count(), MAX_SURFACES);

        // A surface going frees its slot, and a fresh id takes it.
        s.apply_event(
            SurfaceEvent::Destroyed {
                id: SurfaceId::new(1),
            },
            &mut out,
        );
        s.apply_event(created(next_id), &mut out);
        assert_eq!(
            out,
            vec![
                SessionEvent::SurfaceGone {
                    id: SurfaceId::new(1),
                    reason: GoneReason::AppClosed,
                },
                announced_toplevel(next_id),
            ]
        );
        assert_eq!(s.surface_count(), MAX_SURFACES);
    }

    #[test]
    fn the_seventeenth_popup_of_one_parent_is_refused() {
        let mut s = Session::new();
        let mut out = Vec::new();
        s.apply_event(created(1), &mut out);
        let mut next_id = 2_u32;
        while s.surface_count() < 1 + MAX_POPUPS_PER_PARENT {
            s.apply_event(popup_created(next_id, 1), &mut out);
            next_id += 1;
        }
        out.clear();
        s.apply_event(popup_created(next_id, 1), &mut out);
        assert!(out.is_empty());
        assert_eq!(s.surface_count(), 1 + MAX_POPUPS_PER_PARENT);
    }

    #[test]
    fn a_popup_without_a_living_parent_is_refused() {
        let mut s = Session::new();
        let mut out = Vec::new();
        // Its parent never came.
        s.apply_event(popup_created(2, 1), &mut out);
        // It names itself.
        s.apply_event(popup_created(3, 3), &mut out);
        assert!(out.is_empty());
        assert_eq!(s.surface_count(), 0);
    }

    #[test]
    fn configure_serials_rise_across_the_session() {
        let mut session = Session::new();
        session.apply_event(created(1), &mut Vec::new());
        session.apply_event(created(2), &mut Vec::new());
        let mut out = Vec::new();
        let first = session
            .configure(SurfaceId::new(1), Size::new(10, 10), &mut out)
            .expect("lives");
        let second = session
            .configure(SurfaceId::new(2), Size::new(10, 10), &mut out)
            .expect("lives");
        let third = session
            .configure(SurfaceId::new(1), Size::new(20, 20), &mut out)
            .expect("lives");
        assert_eq!(first.get(), 1);
        assert!(first.get() < second.get());
        assert!(second.get() < third.get());
        // An unknown id spends no serial.
        assert_eq!(
            session.configure(SurfaceId::new(99), Size::new(1, 1), &mut out),
            None
        );
        let fourth = session
            .configure(SurfaceId::new(2), Size::new(30, 30), &mut out)
            .expect("lives");
        assert_eq!(fourth.get(), 4);
    }

    #[test]
    fn events_for_unknown_surfaces_are_dropped() {
        let mut s = Session::new();
        let mut out = Vec::new();
        s.apply_event(damaged(42, 0), &mut out);
        s.apply_event(metadata(42, "Ghost"), &mut out);
        s.apply_event(
            SurfaceEvent::Destroyed {
                id: SurfaceId::new(42),
            },
            &mut out,
        );
        s.apply_event(
            SurfaceEvent::FocusRequested {
                id: SurfaceId::new(42),
            },
            &mut out,
        );
        s.apply_event(
            SurfaceEvent::ResizeRequested {
                id: SurfaceId::new(42),
                size: Size::new(1, 1),
            },
            &mut out,
        );
        assert!(out.is_empty());
    }

    #[test]
    fn a_surface_id_is_never_reused() {
        let mut s = Session::new();
        let mut out = Vec::new();
        s.apply_event(created(1), &mut out);
        s.apply_event(
            SurfaceEvent::Destroyed {
                id: SurfaceId::new(1),
            },
            &mut out,
        );
        out.clear();
        s.apply_event(created(1), &mut out);
        assert!(out.is_empty());
        assert_eq!(s.surface_count(), 0);
    }

    #[test]
    fn asks_and_cursor_and_clipboard_pass_through() {
        let mut s = Session::new();
        let mut out = Vec::new();
        s.apply_event(created(1), &mut out);
        out.clear();
        s.apply_event(
            SurfaceEvent::FocusRequested {
                id: SurfaceId::new(1),
            },
            &mut out,
        );
        s.apply_event(
            SurfaceEvent::ResizeRequested {
                id: SurfaceId::new(1),
                size: Size::new(500, 400),
            },
            &mut out,
        );
        let cursor = CursorImage {
            serial: 3,
            size: Size::new(1, 1),
            hotspot: Point::new(0, 0),
            argb: vec![0, 0, 0, 0],
        };
        s.apply_event(
            SurfaceEvent::CursorChanged {
                cursor: cursor.clone(),
            },
            &mut out,
        );
        // The backend's serial 3 is its own; the session hands out 1, its first.
        let cursor = CursorImage {
            serial: 1,
            ..cursor
        };
        s.apply_event(SurfaceEvent::ClipboardRequested, &mut out);
        assert_eq!(
            out,
            vec![
                SessionEvent::FocusAsk {
                    id: SurfaceId::new(1),
                },
                SessionEvent::ResizeAsk {
                    id: SurfaceId::new(1),
                    size: Size::new(500, 400),
                },
                SessionEvent::CursorChanged { cursor },
                SessionEvent::ClipboardAsk,
            ]
        );
    }

    #[test]
    fn a_close_request_reports_whether_the_surface_lives() {
        let mut s = Session::new();
        s.apply_event(created(1), &mut Vec::new());
        assert!(s.close_request(SurfaceId::new(1)));
        s.apply_event(
            SurfaceEvent::Destroyed {
                id: SurfaceId::new(1),
            },
            &mut Vec::new(),
        );
        assert!(!s.close_request(SurfaceId::new(1)));
    }
}
