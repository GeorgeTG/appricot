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

/// Why an ack was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AckError {
    /// No configure is waiting for an ack.
    NothingPending,
    /// The ack names a serial that is not the one waiting: an older one, or one never sent.
    WrongSerial {
        /// The serial the ack named.
        acked: ConfigureSerial,
        /// The serial waiting for an ack.
        pending: ConfigureSerial,
    },
}

impl fmt::Display for AckError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NothingPending => f.write_str("ack with no configure pending"),
            Self::WrongSerial { acked, pending } => {
                write!(f, "ack names serial {acked}, but {pending} is pending")
            }
        }
    }
}

impl std::error::Error for AckError {}

/// One streamed window: an id, a role, a size, a scale, and the damage not yet sent.
///
/// The host plays the compositor. It proposes a size with [`Surface::configure`], and the
/// surface keeps its old size until the backend reports, with [`Surface::ack_configure`], that
/// the app applied it. This is xdg-shell's configure and `ack_configure`, with one
/// simplification: only the latest configure can be acked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Surface {
    id: SurfaceId,
    role: Role,
    size: Size,
    scale: Scale,
    toplevel_parent: Option<SurfaceId>,
    damage: Damage,
    last_serial: u32,
    pending: Option<(ConfigureSerial, Size)>,
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
            last_serial: 0,
            pending: None,
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

    /// Proposes a new size, and returns the serial the ack must name.
    ///
    /// A newer configure replaces one that is still waiting.
    pub fn configure(&mut self, size: Size) -> ConfigureSerial {
        self.last_serial = self.last_serial.wrapping_add(1);
        let serial = ConfigureSerial(self.last_serial);
        self.pending = Some((serial, size));
        serial
    }

    /// Proposes a new size under a serial the caller allocated.
    ///
    /// A session that shares one serial counter across its surfaces hands the serial in; a
    /// lone surface uses [`Surface::configure`], which allocates its own. The caller keeps
    /// serials rising. Like [`Surface::configure`], a newer proposal replaces one that is
    /// still waiting.
    pub fn configure_with_serial(&mut self, serial: ConfigureSerial, size: Size) {
        self.last_serial = serial.get();
        self.pending = Some((serial, size));
    }

    /// Records that the app applied the configure named by `acked`, and returns the new
    /// size.
    ///
    /// The surface takes the proposed size and its whole area is damaged. Fails with
    /// [`AckError::NothingPending`] when no configure waits, and with
    /// [`AckError::WrongSerial`] when `acked` is not the latest serial sent.
    pub fn ack_configure(&mut self, acked: ConfigureSerial) -> Result<Size, AckError> {
        let Some((pending, size)) = self.pending else {
            return Err(AckError::NothingPending);
        };
        if acked != pending {
            return Err(AckError::WrongSerial { acked, pending });
        }
        self.pending = None;
        self.size = size;
        self.damage_all();
        Ok(size)
    }

    /// Records that the app took `size`, and returns the serial that was pending, if any.
    ///
    /// `size` is what the app really did: the proposal, or the app's own clamp of it, which a
    /// backend reports with [`SurfaceEvent::Resized`](crate::SurfaceEvent::Resized). The
    /// surface takes `size` and its whole area is damaged, like
    /// [`Surface::ack_configure`], whether a configure was waiting or the app resized itself.
    /// Unlike an ack, a report of what happened cannot name a wrong serial, so nothing is
    /// refused.
    pub fn resolve_pending(&mut self, size: Size) -> Option<ConfigureSerial> {
        let serial = self.pending.take().map(|(pending, _)| pending);
        self.size = size;
        self.damage_all();
        serial
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
    use crate::{
        AckError, ConfigureSerial, Point, Positioner, Rect, Role, Scale, Size, Surface, SurfaceId,
    };

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

    #[test]
    fn the_size_changes_only_when_the_configure_is_acked() {
        let mut s = toplevel(1);
        let serial = s.configure(Size::new(800, 600));
        assert_eq!(s.size(), Size::new(400, 300));
        assert_eq!(s.ack_configure(serial), Ok(Size::new(800, 600)));
        assert_eq!(s.size(), Size::new(800, 600));
    }

    #[test]
    fn an_ack_for_a_replaced_configure_is_refused() {
        let mut s = toplevel(1);
        let acked = s.configure(Size::new(800, 600));
        let pending = s.configure(Size::new(1024, 768));
        let got = s.ack_configure(acked);
        assert_eq!(got, Err(AckError::WrongSerial { acked, pending }));
        assert_eq!(s.size(), Size::new(400, 300));
    }

    #[test]
    fn an_ack_with_nothing_pending_is_refused() {
        let mut s = toplevel(1);
        let serial = s.configure(Size::new(800, 600));
        s.ack_configure(serial).expect("first ack");
        assert_eq!(s.ack_configure(serial), Err(AckError::NothingPending));
    }

    #[test]
    fn resolve_pending_takes_the_size_the_app_really_took() {
        let mut s = toplevel(1);
        s.configure_with_serial(ConfigureSerial::new(41), Size::new(800, 600));
        // The app clamped the proposal to its own minimum.
        assert_eq!(
            s.resolve_pending(Size::new(800, 500)),
            Some(ConfigureSerial::new(41))
        );
        assert_eq!(s.size(), Size::new(800, 500));
        assert_eq!(s.take_damage(), vec![Rect::new(0, 0, 800, 500)]);
        // No configure was waiting this time — the app resized itself — but the report is
        // still the truth: the size was taken and the whole surface is damaged again.
        assert_eq!(s.resolve_pending(Size::new(640, 480)), None);
        assert_eq!(s.size(), Size::new(640, 480));
        assert_eq!(s.take_damage(), vec![Rect::new(0, 0, 640, 480)]);
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
