//! Contacts between props and players: who may touch whom, and the impacts
//! that hurt.
//!
//! A prop held by a player does not collide with that player (or it would
//! push them around while they carry it), nor, once let go, until the two
//! have come apart: a `Box3D` *filter joint* disables contacts between that
//! specific pair before the solver sees them.
//!
//! A prop that *starts* touching a player during a step may have hit them.
//! The impact's speed is how fast the prop was moving *towards the player*
//! before the step, limited by its speed relative to the player along the
//! contact; `physics.ron`'s impact damage turns it and the prop's mass into
//! damage. So a player walking or falling onto a prop that is not moving
//! towards them is not hurt (a hard landing is fall damage), however heavy
//! it is. Props held by someone deal none, and a prop never hits the player
//! who just let go of it.

use box3d_rust as b3;
use glam::Vec3;

use super::{BodyId, BodyKind, PhysicsWorld, from_b3};

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

/// A prop shoving a player.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Shove {
    /// The player's body.
    pub player: BodyId,
    /// The change to give the player's velocity, units/s.
    pub velocity: Vec3,
}

impl PhysicsWorld {
    /// The impacts of props on players that began this step, given each
    /// awake prop's velocity before it; updates the touching set.
    pub(super) fn detect_impacts(&mut self, before: &[(BodyId, Vec3)]) -> Vec<Impact> {
        let mut touching = Vec::new();
        let mut impacts = Vec::new();
        let mut shoves: Vec<Shove> = Vec::new();
        for (index, body) in self.bodies.iter().enumerate() {
            let BodyKind::Player { velocity, .. } = body.kind else {
                continue;
            };
            let player = BodyId(index as u32);
            let capacity = usize::try_from(b3::body::body_get_contact_capacity(
                &self.b3_world(),
                body.handle,
            ))
            .unwrap_or(0)
            .min(128);
            if capacity == 0 {
                continue;
            }
            let contacts = b3::body::body_get_contact_data(&self.b3_world(), body.handle, capacity);
            for contact in contacts {
                let body_a = b3::shape::shape_get_body(&self.b3_world(), contact.shape_id_a);
                let body_b = b3::shape::shape_get_body(&self.b3_world(), contact.shape_id_b);
                let other = if body_a == body.handle {
                    body_b
                } else if body_b == body.handle {
                    body_a
                } else {
                    continue;
                };
                let Some(prop) = self.body_id(other) else {
                    continue;
                };
                let Some(mass) = self.mass(prop) else {
                    continue;
                };
                if touching.contains(&(player, prop)) {
                    continue;
                }
                touching.push((player, prop));
                if self.holder(prop).is_some() || self.ignores(player, prop) {
                    continue;
                }
                let Some(manifold) = contact.manifolds.first() else {
                    continue;
                };
                let normal = from_b3(manifold.normal);
                // Box3D's normal points from shape A to shape B; this one
                // points from the prop to the player.
                let towards_player = if body_a == body.handle {
                    -normal
                } else {
                    normal
                };
                if towards_player == Vec3::ZERO {
                    continue;
                }
                let prop_velocity = before
                    .iter()
                    .find(|(id, _)| *id == prop)
                    .map_or(Vec3::ZERO, |&(_, velocity)| velocity);
                let closing = (prop_velocity - velocity).dot(towards_player);
                if closing > 0.0 && self.is_dynamic(prop) {
                    // The prop meets the player as a body of its mass in an
                    // inelastic collision, as the player meets it in `push`.
                    let push = &self.config.push;
                    let change =
                        (closing * mass / (push.player_mass + mass)).min(push.max_speed_change);
                    shoves.push(Shove {
                        player,
                        velocity: towards_player * change,
                    });
                }
                if self.touching.contains(&(player, prop)) {
                    continue;
                }
                let speed = prop_velocity.dot(towards_player).min(closing);
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
        self.shoves = sum_shoves(shoves);
        impacts
    }
}

/// One shove per player: the largest from each direction would be fussy, so
/// they simply add.
fn sum_shoves(shoves: Vec<Shove>) -> Vec<Shove> {
    let mut summed: Vec<Shove> = Vec::new();
    for shove in shoves {
        match summed.iter_mut().find(|s| s.player == shove.player) {
            Some(total) => total.velocity += shove.velocity,
            None => summed.push(shove),
        }
    }
    summed
}
