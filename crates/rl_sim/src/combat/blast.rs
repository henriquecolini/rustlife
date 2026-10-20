//! Explosions: who and what a blast reaches, and how hard.
//!
//! A blast's strength falls linearly from its centre to its radius
//! ([`ExplosionConfig::scale`]), measured to the nearest point of a
//! player's box, so a rocket at someone's feet hits them at full strength.
//! Walls shelter players: a player is reached only if the map does not
//! block the line from the centre to their box. Props are pushed without
//! that check (a blast tumbling the crate behind a pillar reads as right).

use glam::Vec3;
use rl_data::ExplosionConfig;

use super::Target;
use crate::collision::CollisionWorld;
use crate::collision::contents::{SOLID, WINDOW};
use crate::logic::mover::{Mover, MoverClass};
use crate::physics::{BodyId, PhysicsWorld};
use crate::simulation::PlayerId;

/// What blocks a blast: the map's solid brushes and windows.
const SHELTER_MASK: u32 = SOLID | WINDOW;

/// A player a blast reaches.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlayerReach {
    /// The player.
    pub player: PlayerId,
    /// Strength of the blast where it reaches them, `0..=1`.
    pub scale: f32,
    /// Direction from the blast to them (unit length).
    pub direction: Vec3,
}

/// A prop a blast reaches.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PropReach {
    /// The prop.
    pub body: BodyId,
    /// Strength of the blast at its centre of mass, `0..=1`.
    pub scale: f32,
    /// Direction from the blast to it (unit length).
    pub direction: Vec3,
    /// Its centre of mass, where the push is applied.
    pub center: Vec3,
}

/// The players a blast at `center` reaches.
#[must_use]
pub fn players(
    center: Vec3,
    explosion: &ExplosionConfig,
    targets: &[Target],
    world: &CollisionWorld,
) -> Vec<PlayerReach> {
    targets
        .iter()
        .filter_map(|target| {
            let nearest = target.nearest_point(center);
            let scale = explosion.scale(nearest.distance(center));
            if scale <= 0.0 {
                return None;
            }
            // Sheltered unless the map leaves a clear line to the box's
            // centre or its nearest point.
            let clear = |point: Vec3| !world.trace_ray(center, point, SHELTER_MASK).hit();
            if !(clear(target.center()) || clear(nearest)) {
                return None;
            }
            Some(PlayerReach {
                player: target.player,
                scale,
                direction: (target.center() - center).normalize_or(Vec3::Z),
            })
        })
        .collect()
}

/// The dynamic props a blast at `center` reaches.
#[must_use]
pub fn props(center: Vec3, explosion: &ExplosionConfig, physics: &PhysicsWorld) -> Vec<PropReach> {
    physics
        .props()
        .filter(|&body| physics.is_dynamic(body))
        .filter_map(|body| {
            let prop_center = physics.center_of_mass(body)?;
            let radius = physics.radius(body).unwrap_or(0.0);
            let distance = (prop_center.distance(center) - radius).max(0.0);
            let scale = explosion.scale(distance);
            (scale > 0.0).then(|| PropReach {
                body,
                scale,
                direction: (prop_center - center).normalize_or(Vec3::Z),
                center: prop_center,
            })
        })
        .collect()
}

/// The movers a blast at `center` damages — breakables and buttons that
/// are still there — with the blast's strength at each: `(BSP model,
/// scale)`. Measured to the nearest point of the mover's bounds; walls do
/// not shelter them.
#[must_use]
pub fn brushes(
    center: Vec3,
    explosion: &ExplosionConfig,
    world: &CollisionWorld,
    movers: &[Mover],
) -> Vec<(usize, f32)> {
    movers
        .iter()
        .filter(|mover| {
            matches!(mover.class, MoverClass::Breakable | MoverClass::Button) && !mover.gone()
        })
        .filter_map(|mover| {
            let (mins, maxs) = world.mover_bounds(mover.model)?;
            let scale = explosion.scale(center.clamp(mins, maxs).distance(center));
            (scale > 0.0).then_some((mover.model, scale))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collision::WorldBuilder;

    const BLAST: ExplosionConfig = ExplosionConfig {
        radius: 200.0,
        damage: 100.0,
        knockback: 500.0,
        force: 1000.0,
    };

    fn target(index: usize, x: f32) -> Target {
        Target {
            player: PlayerId::from_index(index),
            mins: Vec3::new(x - 16.0, -16.0, 0.0),
            maxs: Vec3::new(x + 16.0, 16.0, 72.0),
        }
    }

    #[test]
    fn blasts_weaken_with_distance_to_the_box() {
        let world = WorldBuilder::new().build();
        let targets = [target(0, 0.0), target(1, 116.0), target(2, 400.0)];
        let reached = players(Vec3::new(0.0, 0.0, 1.0), &BLAST, &targets, &world);
        assert_eq!(reached.len(), 2, "the far player is out of reach");
        assert_eq!(reached[0].scale, 1.0, "inside the box: full strength");
        // The near face of the second box is 100 units away.
        assert!((reached[1].scale - 0.5).abs() < 1e-5);
        assert!(reached[1].direction.x > 0.9);
    }

    #[test]
    fn walls_shelter_players() {
        let world = WorldBuilder::new()
            .solid_box(
                Vec3::new(40.0, -500.0, -10.0),
                Vec3::new(60.0, 500.0, 500.0),
            )
            .build();
        let targets = [target(0, 116.0)];
        assert!(players(Vec3::new(0.0, 0.0, 36.0), &BLAST, &targets, &world).is_empty());
    }
}
