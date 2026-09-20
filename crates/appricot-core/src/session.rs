//! One streamer session: the surface set under the wire caps, and the frames planned over it.
//!
//! A [`Session`] is a pure state machine fed from two sides. The backend side is
//! [`Session::apply_event`], which takes what a backend reported and updates the set. The host
//! side is everything else: [`Session::configure`], [`Session::frame_ack`],
//! [`Session::close_request`], and the detach and resume pair that models a socket dying and
//! a client coming back. Both sides are decisions, not I/O: the session never opens anything,
//! never waits, and never reads a clock — every moment is a caller-supplied monotonic
//! millisecond — so a streamer drives it from its own event loop and every rule here is
//! deterministic and unit-testable.
//!
//! What comes out is [`SessionEvent`]: one variant per server-to-client message the session
//! itself decides. The streamer maps them onto the wire; core keeps no wire vocabulary beyond
//! the bounded strings a title and an app id already are.
//!
//! The counts and the grace window mirror the wire limits table
//! (`crates/appricot-proto/proto/appricot/v0/wire.proto`, "THE LIMITS TABLE"). The numbers
//! there win over these.

use appricot_proto::limits::{AppId, Title};

use crate::capture::SurfaceEvent;
use crate::frame::FrameCredits;
use crate::geometry::{Rect, Size};
use crate::pixels::CursorImage;
use crate::role::{Positioner, Role};
use crate::surface::{ConfigureSerial, Scale, Surface, SurfaceId};

/// Most living surfaces in one session (`MAX_SURFACES` on the wire). A `Created` past the cap
/// is refused: the surface is not tracked and nothing about it is ever announced.
pub const MAX_SURFACES: usize = 64;

/// Most living popups of one parent surface (`MAX_POPUPS_PER_PARENT` on the wire). A popup
/// `Created` past the cap is refused, like a surface past [`MAX_SURFACES`].
pub const MAX_POPUPS_PER_PARENT: usize = 16;

/// Most frames sent and not yet acked, per surface (`MAX_FRAME_CREDITS` on the wire).
pub const MAX_FRAME_CREDITS: u32 = 4;

/// How long a session outlives its dropped socket, in milliseconds (`RESUME_GRACE_MS` on the
/// wire). The clock is the caller's; the session only counts.
pub const RESUME_GRACE_MS: u64 = 10_000;

/// Why a surface is gone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum GoneReason {
    /// The app closed the window itself.
    AppClosed,
    /// Its parent went first; popups do not outlive parents.
    ParentGone,
    /// The session is ending and everything goes with it.
    SessionEnd,
}

/// What the session decided the streamer must put on the wire, one variant per
/// server-to-client message it owns.
///
/// Plain data: the streamer maps each variant onto the wire (core keeps no wire vocabulary
/// beyond the bounded strings a title and an app id already are). New variants may be added;
/// match with a wildcard arm.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
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
        /// How a popup is placed inside its parent; `None` for a toplevel.
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
    /// The app asked to resize itself. The host decides; the streamer reports.
    ResizeAsk {
        /// The surface that asked.
        id: SurfaceId,
        /// The size it asked for.
        size: Size,
    },
    /// The cursor image changed. One cursor per session, not per surface.
    CursorChanged {
        /// The new cursor image.
        cursor: CursorImage,
    },
    /// The app pasted and the backend holds no clipboard text. The host decides what, if
    /// anything, to send back.
    ClipboardAsk,
    /// The app applied the configure named by the serial, taking `size` — which may be its
    /// own clamp of what was proposed.
    ConfigureAcked {
        /// The surface.
        id: SurfaceId,
        /// The configure that was applied.
        serial: ConfigureSerial,
        /// The size the app really took.
        size: Size,
    },
    /// The surface resized itself, with no configure waiting. Informational: no ack follows.
    Resized {
        /// The surface.
        id: SurfaceId,
        /// The new size.
        size: Size,
    },
}

/// One frame the session decided to send.
///
/// Built by [`Session::plan_frame`]; the streamer cuts its rectangles into tiles and encodes
/// them.
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
            credits: FrameCredits::new(MAX_FRAME_CREDITS),
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
/// an unknown id. Ids are never reused: a `Created` naming an id the session has ever accepted
/// is refused too. A popup whose parent is not living is refused; when a surface goes, its
/// popups go with it, and theirs after them, each reported [`GoneReason::ParentGone`]. A
/// dialog's parent — a toplevel naming another toplevel, X11's `WM_TRANSIENT_FOR` — is advice,
/// not a lifetime link: it is kept only while it names a living toplevel, and it never takes
/// the dialog with it when it goes.
///
/// # Detach and resume
///
/// [`Session::detach`] models the socket dying: nothing is planned and nothing is emitted
/// while the session waits, but state moves on, for [`RESUME_GRACE_MS`] as the caller counts
/// them ([`Session::resume_expired`]). [`Session::resume`] models the client coming back: the
/// whole living window set is announced again, every surface is marked for a full redraw, and
/// each surface's frame pacing starts over, because the new connection holds nothing in
/// flight. [`Session::end`] is the end: every living surface goes with
/// [`GoneReason::SessionEnd`].
#[derive(Debug, Clone)]
pub struct Session {
    /// The living surfaces, in creation order.
    surfaces: Vec<Tracked>,
    /// Every id ever accepted, living or gone: ids are never reused.
    seen: Vec<SurfaceId>,
    /// The last configure serial handed out; the next is one more, wrapping.
    configure_serial: u32,
    /// While detached, the monotonic millisecond at which the grace window ends.
    detached_deadline: Option<u64>,
}

impl Session {
    /// An empty, attached session.
    pub fn new() -> Self {
        Self {
            surfaces: Vec::new(),
            seen: Vec::new(),
            configure_serial: 0,
            detached_deadline: None,
        }
    }

    /// Applies one backend event and appends what the streamer must send.
    ///
    /// Events about unknown or refused surfaces are dropped silently. While the session is
    /// detached nothing is appended: state still moves on, and [`Session::resume`]
    /// re-synchronises the client with the whole window set. Damage is never an event; it
    /// comes out through [`Session::plan_frame`].
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
                let Some(tracked) = self.tracked_mut(id) else {
                    return;
                };
                match tracked.surface.resolve_pending(size) {
                    Some(serial) => {
                        self.emit(out, SessionEvent::ConfigureAcked { id, serial, size });
                    }
                    None => self.emit(out, SessionEvent::Resized { id, size }),
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
                    self.emit(out, SessionEvent::ResizeAsk { id, size });
                }
            }
            SurfaceEvent::CursorChanged { cursor } => {
                self.emit(out, SessionEvent::CursorChanged { cursor });
            }
            SurfaceEvent::ClipboardRequested => {
                self.emit(out, SessionEvent::ClipboardAsk);
            }
        }
    }

    /// Proposes `size` for surface `id`, and returns the serial the app's answer will name,
    /// or `None` for an unknown id, where the request is dropped like every other.
    ///
    /// Serials come from one session-wide counter ([`Session::next_configure_serial`]), so
    /// they rise across surfaces. A newer proposal replaces one still waiting. The surface's
    /// size changes only when the app's answer arrives — a `Resized` backend event, which the
    /// session answers with [`SessionEvent::ConfigureAcked`] carrying the size the app really
    /// took.
    pub fn configure(&mut self, id: SurfaceId, size: Size) -> Option<ConfigureSerial> {
        let tracked = self.surfaces.iter_mut().find(|t| t.surface.id() == id)?;
        self.configure_serial = self.configure_serial.wrapping_add(1);
        let serial = ConfigureSerial::new(self.configure_serial);
        tracked.surface.configure_with_serial(serial, size);
        Some(serial)
    }

    /// The next configure serial: one shared, wrapping counter per session, starting at 1.
    ///
    /// Serials rise across every surface of the session; [`Session::configure`] draws from
    /// the same counter, and every draw is spent.
    pub fn next_configure_serial(&mut self) -> ConfigureSerial {
        self.configure_serial = self.configure_serial.wrapping_add(1);
        ConfigureSerial::new(self.configure_serial)
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
    /// `Some` comes back only when the surface lives, has damage, and a credit is free
    /// ([`MAX_FRAME_CREDITS`] in flight per surface). The rectangles leave the surface, so
    /// damage gathered while no credit is free coalesces into the next frame instead of
    /// piling up. `full_redraw` marks the first frame of a surface and the first planned
    /// after a [`Session::resume`].
    ///
    /// A detached session plans nothing, whatever `at` says: the socket that would carry the
    /// frame is gone. `at` is the caller's monotonic clock in milliseconds, read for nothing
    /// but the grace-window check; `None` means the caller keeps no clock.
    pub fn plan_frame(&mut self, id: SurfaceId, at: Option<u64>) -> Option<FramePlan> {
        if self.is_detached() || at.is_some_and(|now| self.resume_expired(now)) {
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

    /// Takes the host's request that the app close surface `id`.
    ///
    /// Returns whether the surface lives, so the streamer asks the backend only when it
    /// must. Nothing changes here: the app decides, and the surface goes when the backend
    /// reports it gone.
    pub fn close_request(&mut self, id: SurfaceId) -> bool {
        self.tracked(id).is_some()
    }

    /// Records the socket dying at `at_ms`, the caller's monotonic milliseconds.
    ///
    /// Planning and emission stop and the grace window of [`RESUME_GRACE_MS`] starts.
    /// Another `detach` re-arms the window from its own `at_ms`.
    pub fn detach(&mut self, at_ms: u64) {
        self.detached_deadline = Some(at_ms.saturating_add(RESUME_GRACE_MS));
    }

    /// Whether the grace window had already run out at `at_ms`.
    ///
    /// True only while detached, from the deadline on. A session that never detached, or has
    /// resumed or ended, has nothing to expire.
    pub fn resume_expired(&self, at_ms: u64) -> bool {
        self.detached_deadline
            .is_some_and(|deadline| at_ms >= deadline)
    }

    /// Records the client reattaching, and re-synchronises it: appends
    /// [`SessionEvent::SurfaceNew`] for every living surface, in creation order, with the
    /// metadata, the size and the parent each has now.
    ///
    /// Every surface is marked for a full redraw and its frame pacing starts over — the new
    /// connection holds nothing in flight — so the first frame planned after this carries the
    /// whole surface at sequence 1.
    pub fn resume(&mut self, out: &mut Vec<SessionEvent>) {
        self.detached_deadline = None;
        for tracked in &mut self.surfaces {
            tracked.frames = FrameState::fresh();
            tracked.surface.invalidate();
        }
        for tracked in &self.surfaces {
            out.push(self.surface_new_event(tracked));
        }
    }

    /// Ends the session: appends [`SessionEvent::SurfaceGone`] with
    /// [`GoneReason::SessionEnd`] for every living surface, in creation order, and keeps
    /// none.
    ///
    /// Unlike [`Session::apply_event`] this appends even while detached: it is the streamer
    /// tearing the session down, attached or not, and these events are the record of what
    /// went.
    pub fn end(&mut self, out: &mut Vec<SessionEvent>) {
        self.detached_deadline = None;
        for tracked in &self.surfaces {
            out.push(SessionEvent::SurfaceGone {
                id: tracked.surface.id(),
                reason: GoneReason::SessionEnd,
            });
        }
        self.surfaces.clear();
    }

    /// Whether the socket is gone and the grace window is running.
    pub fn is_detached(&self) -> bool {
        self.detached_deadline.is_some()
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
    /// Refused — tracked nowhere, announced nowhere — when its id was ever used before, when
    /// a popup has no living parent or its parent already keeps
    /// [`MAX_POPUPS_PER_PARENT`] popups, or when [`MAX_SURFACES`] surfaces already live.
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
        if self.seen.contains(&id) {
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
        self.seen.push(id);
        // A dialog's parent is kept only while it names a living toplevel — the id itself
        // cannot be one yet, so the self case falls out of the same check, and
        // `set_toplevel_parent` refuses a popup's (whose parent is its role's) regardless.
        let parent = parent.filter(|parent| self.is_living_toplevel(*parent));
        let mut surface = Surface::new(id, role, size, Scale::ONE);
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
        if self.detached_deadline.is_none() {
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
    /// A popup announces the parent its role fixed. A toplevel announces the parent it was
    /// created with while that still names a living toplevel: dialogs do not follow their
    /// parents, so one can outlive its parent, and the announcement after that carries none
    /// — the wire never names a surface the session does not track.
    fn surface_new_event(&self, tracked: &Tracked) -> SessionEvent {
        let role = tracked.surface.role();
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
            size: tracked.surface.size(),
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

#[cfg(test)]
mod tests {
    use appricot_proto::limits::{AppId, Title};

    use crate::{
        CursorImage, GoneReason, MAX_FRAME_CREDITS, MAX_POPUPS_PER_PARENT, MAX_SURFACES, Point,
        Positioner, RESUME_GRACE_MS, Rect, Role, Scale, Session, SessionEvent, Size, SurfaceEvent,
        SurfaceId,
    };

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
            .configure(SurfaceId::new(1), Size::new(800, 600))
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
        assert_eq!(out, vec![announced_dialog(2, None)]);
    }

    #[test]
    fn a_resume_re_announces_a_dialog_with_its_parent() {
        let mut s = Session::new();
        let mut out = Vec::new();
        s.apply_event(created(1), &mut out);
        s.apply_event(dialog_created(2, 1), &mut out);
        s.detach(0);
        out.clear();
        s.resume(&mut out);
        assert_eq!(
            out,
            vec![announced_toplevel(1), announced_dialog(2, Some(1))]
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
        let plan = s.plan_frame(id, None).expect("creation damage");
        assert_eq!(plan.sequence, 1);
        assert!(plan.full_redraw);
        assert_eq!(plan.rects, vec![Rect::new(0, 0, 400, 300)]);
        // Nothing changed since: no frame.
        assert_eq!(s.plan_frame(id, None), None);
        s.apply_event(damaged(1, 5), &mut Vec::new());
        let plan = s.plan_frame(id, None).expect("fresh damage");
        assert_eq!(plan.sequence, 2);
        assert!(!plan.full_redraw);
        assert_eq!(plan.rects, vec![Rect::new(5, 0, 10, 10)]);
    }

    #[test]
    fn frames_stop_at_the_credit_limit_until_an_ack_frees_one() {
        let mut s = Session::new();
        s.apply_event(created(1), &mut Vec::new());
        let id = SurfaceId::new(1);
        for expected in 1..=MAX_FRAME_CREDITS {
            let plan = s.plan_frame(id, None).expect("a free credit");
            assert_eq!(plan.sequence, expected);
            if expected < MAX_FRAME_CREDITS {
                s.apply_event(damaged(1, 0), &mut Vec::new());
            }
        }
        // Every credit is spent: damage waits, nothing is planned.
        s.apply_event(damaged(1, 0), &mut Vec::new());
        assert_eq!(s.plan_frame(id, None), None);
        s.frame_ack(id, 1);
        let plan = s.plan_frame(id, None).expect("the ack freed a credit");
        assert_eq!(plan.sequence, MAX_FRAME_CREDITS + 1);
    }

    #[test]
    fn a_stale_or_duplicate_ack_changes_nothing() {
        let mut s = Session::new();
        s.apply_event(created(1), &mut Vec::new());
        let id = SurfaceId::new(1);
        assert_eq!(s.plan_frame(id, None).expect("first").sequence, 1);
        s.apply_event(damaged(1, 0), &mut Vec::new());
        assert_eq!(s.plan_frame(id, None).expect("second").sequence, 2);
        s.frame_ack(id, 99);
        s.frame_ack(id, 1);
        // The ack of 1 again must not free a second credit: exactly three more frames fit.
        s.frame_ack(id, 1);
        for expected in 3..=MAX_FRAME_CREDITS + 1 {
            s.apply_event(damaged(1, 0), &mut Vec::new());
            assert_eq!(
                s.plan_frame(id, None).expect("one credit freed").sequence,
                expected
            );
        }
        s.apply_event(damaged(1, 0), &mut Vec::new());
        assert_eq!(s.plan_frame(id, None), None);
    }

    #[test]
    fn resume_re_announces_the_window_set_and_redraws_everything() {
        let mut s = Session::new();
        let mut out = Vec::new();
        s.apply_event(created(1), &mut out);
        s.apply_event(popup_created(2, 1), &mut out);
        s.apply_event(metadata(1, "Notes"), &mut out);
        out.clear();

        s.detach(1_000);
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
        assert!(!s.resume_expired(999 + RESUME_GRACE_MS));
        assert!(s.resume_expired(1_000 + RESUME_GRACE_MS));

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
            ]
        );
        let plan = s
            .plan_frame(SurfaceId::new(1), None)
            .expect("resume damaged everything");
        assert_eq!(plan.sequence, 1);
        assert!(plan.full_redraw);
        assert_eq!(plan.rects, vec![Rect::new(0, 0, 640, 480)]);
    }

    #[test]
    fn a_detached_session_plans_nothing() {
        let mut s = Session::new();
        s.apply_event(created(1), &mut Vec::new());
        s.detach(0);
        assert!(s.is_detached());
        assert_eq!(s.plan_frame(SurfaceId::new(1), Some(50)), None);
        assert_eq!(s.plan_frame(SurfaceId::new(1), None), None);
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
        let first = session
            .configure(SurfaceId::new(1), Size::new(10, 10))
            .expect("lives");
        let second = session
            .configure(SurfaceId::new(2), Size::new(10, 10))
            .expect("lives");
        let third = session
            .configure(SurfaceId::new(1), Size::new(20, 20))
            .expect("lives");
        assert_eq!(first.get(), 1);
        assert!(first.get() < second.get());
        assert!(second.get() < third.get());
        // An unknown id spends no serial.
        assert_eq!(session.configure(SurfaceId::new(99), Size::new(1, 1)), None);
        let fourth = session
            .configure(SurfaceId::new(2), Size::new(30, 30))
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

    #[test]
    fn ending_a_session_takes_every_surface_with_it() {
        let mut s = Session::new();
        let mut out = Vec::new();
        s.apply_event(created(1), &mut out);
        s.apply_event(popup_created(2, 1), &mut out);
        out.clear();
        s.detach(0);
        s.end(&mut out);
        assert_eq!(
            out,
            vec![
                SessionEvent::SurfaceGone {
                    id: SurfaceId::new(1),
                    reason: GoneReason::SessionEnd,
                },
                SessionEvent::SurfaceGone {
                    id: SurfaceId::new(2),
                    reason: GoneReason::SessionEnd,
                },
            ]
        );
        assert_eq!(s.surface_count(), 0);
        assert!(!s.is_detached());
        assert!(!s.resume_expired(1_000_000));
    }
}
