//! Contacts between props and players: who may touch whom, and the impacts
//! that hurt.
//!
//! A prop held by a player does not collide with that player (or it would
//! push them around while they carry it): [`HeldFilter`] drops those contact
//! pairs before the solver sees them.
//!
//! A prop that *starts* touching a player during a step has hit them. The
//! impact's speed is the prop's speed before the step relative to the
//! player's, along the contact normal; `physics.ron`'s impact damage turns
//! it and the prop's mass into damage. Props held by someone deal none.

use glam::Vec3;
use rapier3d::prelude::{PairFilterContext, PhysicsHooks, RigidBodyHandle, SolverFlags};

use super::{BodyId, BodyKind, PhysicsWorld};

/// A prop hitting a player.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Impact {
    /// The player's body.
    pub player: BodyId,
    /// The prop.
    pub prop: BodyId,
    /// Closing speed along the contact normal, units/s.
    pub speed: f32,
    /// Damage dealt, from `physics.ron`.
    pub damage: f32,
}

/// Drops the contacts between held props and their holders.
pub(super) struct HeldFilter {
    /// `(player, prop)` rigid bodies that must not collide.
    pub(super) pairs: Vec<(RigidBodyHandle, RigidBodyHandle)>,
}

impl PhysicsHooks for HeldFilter {
    fn filter_contact_pair(&self, context: &PairFilterContext) -> Option<SolverFlags> {
        let (a, b) = (context.rigid_body1?, context.rigid_body2?);
        if self
            .pairs
            .iter()
            .any(|&(player, prop)| (a, b) == (player, prop) || (a, b) == (prop, player))
        {
            return None;
        }
        // A hook replaces rapier's own filtering, which skips pairs of two
        // non-dynamic bodies (a player's box against a fixed prop).
        let dynamic = |handle| {
            context
                .bodies
                .get(handle)
                .is_some_and(rapier3d::prelude::RigidBody::is_dynamic)
        };
        (dynamic(a) || dynamic(b)).then_some(SolverFlags::COMPUTE_RIGID_IMPULSES)
    }
}

impl PhysicsWorld {
    /// The impacts of props on players that began this step, given each
    /// awake prop's velocity before it; updates the touching set.
    pub(super) fn detect_impacts(&mut self, before: &[(BodyId, Vec3)]) -> Vec<Impact> {
        let mut touching = Vec::new();
        let mut impacts = Vec::new();
        for (index, body) in self.bodies.iter().enumerate() {
            let BodyKind::Player { velocity, .. } = body.kind else {
                continue;
            };
            let player = BodyId(index as u32);
            for pair in self.rapier.contact_pairs_with(body.collider) {
                if !pair.has_any_active_contact() {
                    continue;
                }
                let other = if pair.collider1 == body.collider {
                    pair.collider2
                } else {
                    pair.collider1
                };
                let Some(collider) = self.rapier.colliders.get(other) else {
                    continue;
                };
                let prop = BodyId(collider.user_data as u32);
                let Some(mass) = self.mass(prop) else {
                    continue;
                };
                touching.push((player, prop));
                let was_touching = self.touching.contains(&(player, prop));
                if was_touching || self.holder(prop).is_some() {
                    continue;
                }
                let Some(normal) = pair
                    .manifolds()
                    .first()
                    .map(|manifold| manifold.data.normal)
                else {
                    continue;
                };
                let prop_velocity = before
                    .iter()
                    .find(|(id, _)| *id == prop)
                    .map_or(Vec3::ZERO, |&(_, velocity)| velocity);
                let speed = (prop_velocity - velocity).dot(normal).abs();
                let damage = self.config.impact_damage.damage(mass, speed);
                if damage > 0.0 {
                    impacts.push(Impact {
                        player,
                        prop,
                        speed,
                        damage,
                    });
                }
            }
        }
        self.touching = touching;
        impacts
    }
}
