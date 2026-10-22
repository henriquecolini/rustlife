//! Props striking props and the map: the impacts that make noise and break
//! things.
//!
//! Box3D reports a contact hit event when two shapes touch faster than the
//! world's hit-event threshold. The event carries the point and the
//! approach speed measured by the solver, without having to reconstruct the
//! velocities before the step. Contacts with players are not counted here:
//! [`super::contacts`] handles those.

use box3d_rust as b3;
use glam::Vec3;

use b3::world::World;

use super::{BodyId, BodyKind, PhysicsWorld, from_b3};

/// A prop striking another prop or the map.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ContactImpact {
    /// The prop.
    pub body: BodyId,
    /// The other prop, or `None` for the map.
    pub other: Option<BodyId>,
    /// How fast they were closing along the contact, units/s.
    pub speed: f32,
    /// Where they met, Source space.
    pub position: Vec3,
}

/// What a shape belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Party {
    /// The map or one of its movers.
    Map,
    /// A prop.
    Prop(BodyId),
    /// A player's kinematic box.
    Player,
}

/// A prop sliding along something.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Scrape {
    /// The prop.
    pub body: BodyId,
    /// Where it is, Source space.
    pub position: Vec3,
    /// How fast it slides, units/s.
    pub speed: f32,
}

/// Most scrapes reported at once: the loudest ones.
const MAX_SCRAPES: usize = 6;

impl PhysicsWorld {
    /// The props sliding along something right now, loudest first: awake
    /// props in contact that move faster than the configured scrape speed
    /// without rolling (a rolling prop spins about as fast as it travels).
    #[must_use]
    pub fn scrapes(&self) -> Vec<Scrape> {
        let threshold = self.config.breakables.impact.scrape_speed;
        let world = self.b3_world();
        let mut scrapes: Vec<Scrape> = self
            .props()
            .filter(|&body| self.holder(body).is_none())
            .filter_map(|body| {
                let handle = self.handle(body)?;
                if b3::body::body_get_type(&world, handle) != b3::BodyType::Dynamic
                    || !b3::body::body_is_awake(&world, handle)
                {
                    return None;
                }
                let speed = from_b3(b3::body::body_get_linear_velocity(&world, handle)).length();
                let radius = self.radius(body)?.max(1.0);
                let rolling = from_b3(b3::body::body_get_angular_velocity(&world, handle)).length()
                    * radius
                    > 0.65 * speed;
                let capacity = usize::try_from(b3::body::body_get_contact_capacity(&world, handle))
                    .unwrap_or(0)
                    .min(8);
                let touching = capacity > 0
                    && !b3::body::body_get_contact_data(&world, handle, capacity).is_empty();
                (speed > threshold && !rolling && touching).then(|| Scrape {
                    body,
                    position: from_b3(b3::body::body_get_world_center(&world, handle)),
                    speed,
                })
            })
            .collect();
        scrapes.sort_by(|a, b| b.speed.total_cmp(&a.speed));
        scrapes.truncate(MAX_SCRAPES);
        scrapes
    }

    /// The impacts of props on props and on the map that happened during
    /// the step just taken. Slower than `floor` units/s are left out.
    pub(super) fn contact_impacts(
        &self,
        _before: &[(BodyId, Vec3)],
        floor: f32,
    ) -> Vec<ContactImpact> {
        let world = self.b3_world();
        let events = b3::world::world_get_contact_events(&world);
        events
            .hit_events
            .iter()
            .filter_map(|event| {
                let first = self.contact_party(&world, event.shape_id_a)?;
                let second = self.contact_party(&world, event.shape_id_b)?;
                let (body, other) = match (first, second) {
                    (Party::Prop(body), Party::Prop(other)) => (body, Some(other)),
                    (Party::Prop(body), Party::Map | Party::Player) => (body, None),
                    (Party::Map | Party::Player, Party::Prop(body)) => (body, None),
                    _ => return None,
                };
                if matches!(first, Party::Player) || matches!(second, Party::Player) {
                    return None;
                }
                (event.approach_speed > floor).then_some(ContactImpact {
                    body,
                    other,
                    speed: event.approach_speed,
                    position: from_b3(event.point),
                })
            })
            .collect()
    }

    /// What a shape belongs to.
    fn contact_party(&self, world: &World, shape: b3::ShapeId) -> Option<Party> {
        let handle = b3::shape::shape_get_body(world, shape);
        let Some(id) = self.body_id(handle) else {
            return Some(Party::Map);
        };
        Some(match self.bodies.get(id.index())?.kind {
            BodyKind::Prop(_) => Party::Prop(id),
            BodyKind::Player { .. } => Party::Player,
        })
    }
}
