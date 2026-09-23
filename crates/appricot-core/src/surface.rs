//! Surfaces: ids, scale, and the configure/ack handshake.

use std::fmt;

use crate::damage::Damage;
use crate::geometry::{Rect, Size};
use crate::role::Role;

/// The id of a surface, unique within one streamer session.
///
/// Ids are never reused within a session, so a late message about a gone surface cannot land
/// on a new one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SurfaceId(u32);

impl SurfaceId {
    /// Wraps a raw id.
    pub const fn new(raw: u32) -> Self {
        Self(raw)
    }

    /// The raw id, as the wire carries it.
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// Device pixels per logical pixel, in 120ths.
///
/// 120 is 1x, 180 is 1.5x, 240 is 2x. The unit is Wayland's: `wp_fractional_scale_v1` sends
/// "the numerator of a fraction with a denominator of 120"
/// (<https://wayland.app/protocols/fractional-scale-v1>). Zero is not a scale.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Scale(u32);

impl Scale {
    /// One device pixel per logical pixel.
    pub const ONE: Self = Self(120);

    /// A scale of `n` 120ths, or `None` for zero.
    pub const fn from_120ths(n: u32) -> Option<Self> {
        match n {
            0 => None,
            n => Some(Self(n)),
        }
    }

    /// The scale in 120ths.
    pub const fn as_120ths(self) -> u32 {
        self.0
    }
}

impl Default for Scale {
    fn default() -> Self {
        Self::ONE
    }
}

/// The serial of one configure. The ack names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ConfigureSerial(u32);

impl ConfigureSerial {
    /// Wraps a raw serial, as a session's shared counter produced it.
    pub const fn new(raw: u32) -> Self {
        Self(raw)
    }

    /// The raw serial, as the wire carries it.
    pub const fn get(self) -> u32 {
        self.0
    }
}

impl fmt::Display for ConfigureSerial {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

/// Most configures one surface keeps waiting for the app's answer.
///
/// A host that resizes a window live proposes sizes faster than the app answers them. Past
/// this many, the oldest proposal is dropped: the app's next answer acks a newer one, and a
/// newer ack answers every older proposal anyway.
pub const MAX_PENDING_CONFIGURES: usize = 16;

/// What a size report did to a surface's configures: see [`Surface::resolve`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Resolution {
    /// The report answers the configure named here, and every older one still waiting.
    Acked(ConfigureSerial),
    /// No configure was waiting and the size changed: the app resized itself.
    Unprompted,
    /// No configure was waiting and the size is the one the surface already had.
    Unchanged,
}

/// One streamed window: an id, a role, a size, a scale, and the damage not yet sent.
///
/// The host plays the compositor. It proposes sizes through the session, and the surface keeps
/// its old size until the backend reports the size the app really took. That report answers
/// the proposals still waiting: this is xdg-shell's configure and `ack_configure`, driven by
/// what the app did rather than by an ack the app sends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Surface {
    id: SurfaceId,
    role: Role,
    size: Size,
    scale: Scale,
    toplevel_parent: Option<SurfaceId>,
    damage: Damage,
    /// The proposals not yet answered, oldest first, at most [`MAX_PENDING_CONFIGURES`].
    pending: Vec<(ConfigureSerial, Size)>,
}

impl Surface {
    /// A new surface. Its whole area starts damaged, so the first frame is complete.
    pub fn new(id: SurfaceId, role: Role, size: Size, scale: Scale) -> Self {
        let mut surface = Self {
            id,
            role,
            size,
            scale,
            toplevel_parent: None,
            damage: Damage::default(),
            pending: Vec::new(),
        };
        surface.damage_all();
        surface
    }

    /// The surface's id.
    pub fn id(&self) -> SurfaceId {
        self.id
    }

    /// The surface's role.
    pub fn role(&self) -> Role {
        self.role
    }

    /// The size the app last applied.
    pub fn size(&self) -> Size {
        self.size
    }

    /// The surface's scale.
    pub fn scale(&self) -> Scale {
        self.scale
    }

    /// The surface's parent: a popup's from its role, or the parent set on a toplevel.
    pub fn parent(&self) -> Option<SurfaceId> {
        match self.role {
            Role::Popup { parent, .. } => Some(parent),
            Role::Toplevel => self.toplevel_parent,
        }
    }

    /// Makes this toplevel a dialog of `parent`, or clears that with `None`.
    ///
    /// Returns false, and changes nothing, for a popup (its parent is fixed by its role) or
    /// when `parent` is the surface itself. Longer cycles are the caller's to refuse.
    pub fn set_toplevel_parent(&mut self, parent: Option<SurfaceId>) -> bool {
        if matches!(self.role, Role::Popup { .. }) || parent == Some(self.id) {
            return false;
        }
        self.toplevel_parent = parent;
        true
    }

    /// Marks `rect`, in surface coordinates, as changed.
    ///
    /// The part outside the surface is dropped.
    pub fn add_damage(&mut self, rect: Rect) {
        if let Some(inside) = rect.intersection(self.bounds()) {
            self.damage.push(inside);
        }
    }

    /// True when something changed since the last [`Surface::take_damage`].
    pub fn has_damage(&self) -> bool {
        !self.damage.is_empty()
    }

    /// Takes the damage gathered since the last call, leaving none.
    pub fn take_damage(&mut self) -> Vec<Rect> {
        self.damage.take()
    }

    /// Damages the whole surface, so the next frame planned for it is complete.
    pub fn invalidate(&mut self) {
        self.damage_all();
    }

    /// Proposes `size` under `serial`, a serial the session's shared counter allocated.
    ///
    /// Returns true when the proposal changes nothing — no configure is waiting and `size` is
    /// the size the surface already has — so the caller acks it at once and nothing waits.
    /// Otherwise the proposal waits for the app's answer, behind any older one; past
    /// [`MAX_PENDING_CONFIGURES`] the oldest is dropped.
    pub(crate) fn propose(&mut self, serial: ConfigureSerial, size: Size) -> bool {
        if self.pending.is_empty() && size == self.size {
            return true;
        }
        if self.pending.len() >= MAX_PENDING_CONFIGURES {
            self.pending.remove(0);
        }
        self.pending.push((serial, size));
        false
    }

    /// Records that the app took `size`, and says which configure, if any, that answers.
    ///
    /// `size` is what the app really did, as a backend reports it with
    /// [`SurfaceEvent::Resized`](crate::SurfaceEvent::Resized): a proposal, or the app's own
    /// clamp of one, or a size it chose itself. The newest waiting proposal of exactly that
    /// size is answered, and every older one with it; when none has that size (the app
    /// clamped), the newest is answered, and all of them with it. A report of what happened
    /// cannot name a wrong serial, so nothing is refused. The surface takes `size`, and its
    /// whole area is damaged when the size changed.
    pub(crate) fn resolve(&mut self, size: Size) -> Resolution {
        let acked = self
            .pending
            .iter()
            .rposition(|&(_, proposed)| proposed == size)
            .or_else(|| self.pending.len().checked_sub(1))
            .map(|at| {
                let serial = self.pending[at].0;
                self.pending.drain(..=at);
                serial
            });
        let changed = size != self.size;
        if changed {
            self.size = size;
            self.damage_all();
        }
        match acked {
            Some(serial) => Resolution::Acked(serial),
            None if changed => Resolution::Unprompted,
            None => Resolution::Unchanged,
        }
    }

    /// Forgets every proposal still waiting: the connection that made them is gone, and the
    /// next one learns the surface's size from its re-announcement.
    pub(crate) fn forget_pending(&mut self) {
        self.pending.clear();
    }

    fn bounds(&self) -> Rect {
        Rect::new(0, 0, self.size.width, self.size.height)
    }

    fn damage_all(&mut self) {
        let bounds = self.bounds();
        self.damage.clear();
        self.damage.push(bounds);
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_PENDING_CONFIGURES, Resolution};
    use crate::{ConfigureSerial, Point, Positioner, Rect, Role, Scale, Size, Surface, SurfaceId};

    fn toplevel(id: u32) -> Surface {
        let size = Size::new(400, 300);
        Surface::new(SurfaceId::new(id), Role::Toplevel, size, Scale::ONE)
    }

    #[test]
    fn a_new_surface_is_fully_damaged() {
        let mut s = toplevel(1);
        assert_eq!(s.take_damage(), vec![Rect::new(0, 0, 400, 300)]);
        assert!(!s.has_damage());
    }

    #[test]
    fn damage_outside_the_surface_is_dropped() {
        let mut s = toplevel(1);
        s.take_damage();
        s.add_damage(Rect::new(390, 290, 50, 50));
        s.add_damage(Rect::new(500, 500, 10, 10));
        assert_eq!(s.take_damage(), vec![Rect::new(390, 290, 10, 10)]);
    }

    fn serial(raw: u32) -> ConfigureSerial {
        ConfigureSerial::new(raw)
    }

    #[test]
    fn the_size_changes_only_when_the_app_answers() {
        let mut s = toplevel(1);
        assert!(!s.propose(serial(1), Size::new(800, 600)));
        assert_eq!(s.size(), Size::new(400, 300));
        assert_eq!(s.resolve(Size::new(800, 600)), Resolution::Acked(serial(1)));
        assert_eq!(s.size(), Size::new(800, 600));
    }

    #[test]
    fn resolve_takes_the_size_the_app_really_took() {
        let mut s = toplevel(1);
        s.take_damage();
        assert!(!s.propose(serial(41), Size::new(800, 600)));
        // The app clamped the proposal to its own minimum.
        assert_eq!(
            s.resolve(Size::new(800, 500)),
            Resolution::Acked(serial(41))
        );
        assert_eq!(s.size(), Size::new(800, 500));
        assert_eq!(s.take_damage(), vec![Rect::new(0, 0, 800, 500)]);
        // No configure was waiting this time — the app resized itself — but the report is
        // still the truth: the size was taken and the whole surface is damaged again.
        assert_eq!(s.resolve(Size::new(640, 480)), Resolution::Unprompted);
        assert_eq!(s.size(), Size::new(640, 480));
        assert_eq!(s.take_damage(), vec![Rect::new(0, 0, 640, 480)]);
        // The same size again, with nothing waiting, is no change at all.
        assert_eq!(s.resolve(Size::new(640, 480)), Resolution::Unchanged);
        assert!(!s.has_damage());
    }

    #[test]
    fn a_proposal_of_the_current_size_with_nothing_waiting_is_answered_at_once() {
        let mut s = toplevel(1);
        assert!(s.propose(serial(1), Size::new(400, 300)));
        // Nothing waits for it.
        assert_eq!(s.resolve(Size::new(400, 300)), Resolution::Unchanged);
        // Behind another proposal it is not a no-op: the app will go there and back.
        assert!(!s.propose(serial(2), Size::new(800, 600)));
        assert!(!s.propose(serial(3), Size::new(400, 300)));
        assert_eq!(s.resolve(Size::new(800, 600)), Resolution::Acked(serial(2)));
        assert_eq!(s.resolve(Size::new(400, 300)), Resolution::Acked(serial(3)));
    }

    #[test]
    fn a_report_answers_the_newest_proposal_of_its_size_and_every_older_one() {
        let mut s = toplevel(1);
        s.propose(serial(1), Size::new(500, 400));
        s.propose(serial(2), Size::new(600, 400));
        s.propose(serial(3), Size::new(500, 400));
        s.propose(serial(4), Size::new(700, 400));
        // 500x400 matches serial 3 (the newest of that size), which takes 1 and 2 with it.
        assert_eq!(s.resolve(Size::new(500, 400)), Resolution::Acked(serial(3)));
        assert_eq!(s.resolve(Size::new(700, 400)), Resolution::Acked(serial(4)));
        assert_eq!(s.resolve(Size::new(710, 400)), Resolution::Unprompted);
    }

    #[test]
    fn a_report_matching_no_proposal_answers_the_newest() {
        let mut s = toplevel(1);
        s.propose(serial(1), Size::new(800, 600));
        s.propose(serial(2), Size::new(900, 700));
        // The app clamped the first proposal; the answer names the newest and takes both.
        assert_eq!(s.resolve(Size::new(800, 500)), Resolution::Acked(serial(2)));
        assert_eq!(s.resolve(Size::new(900, 500)), Resolution::Unprompted);
    }

    #[test]
    fn the_waiting_queue_is_bounded_and_drops_the_oldest() {
        let mut s = toplevel(1);
        let count = u32::try_from(MAX_PENDING_CONFIGURES).expect("small") + 3;
        for raw in 1..=count {
            s.propose(serial(raw), Size::new(400 + raw, 300));
        }
        // Serials 1 to 3 were dropped: 401x300 matches nothing, so the newest is answered.
        assert_eq!(
            s.resolve(Size::new(401, 300)),
            Resolution::Acked(serial(count))
        );
        // Filled again, the oldest proposal still kept (serial 104) is answered by its size.
        for raw in 1..=count {
            s.propose(serial(100 + raw), Size::new(400 + raw, 300));
        }
        assert_eq!(
            s.resolve(Size::new(404, 300)),
            Resolution::Acked(serial(104))
        );
        s.forget_pending();
        assert_eq!(s.resolve(Size::new(405, 300)), Resolution::Unprompted);
    }

    #[test]
    fn a_popup_takes_its_parent_from_its_role() {
        let parent = SurfaceId::new(1);
        let positioner = Positioner::at(Point::new(10, 10), Size::new(50, 20));
        let role = Role::Popup { parent, positioner };
        let mut popup = Surface::new(SurfaceId::new(2), role, Size::new(50, 20), Scale::ONE);
        assert_eq!(popup.parent(), Some(parent));
        assert!(!popup.set_toplevel_parent(Some(SurfaceId::new(3))));
    }

    #[test]
    fn a_toplevel_cannot_be_its_own_parent() {
        let mut dialog = toplevel(2);
        assert!(!dialog.set_toplevel_parent(Some(SurfaceId::new(2))));
        assert!(dialog.set_toplevel_parent(Some(SurfaceId::new(1))));
        assert_eq!(dialog.parent(), Some(SurfaceId::new(1)));
    }
}
