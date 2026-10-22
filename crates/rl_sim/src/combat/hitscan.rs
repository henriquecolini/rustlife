//! Instant shots: bullets and swings.
//!
//! A shot is a trace from the shooter's eyes: a ray for a bullet, a small
//! box for a swing. It stops at the first of the map, a prop, or another
//! player's box ([`trace`]). Bullets fan out within the weapon's spread by a
//! deterministic draw ([`spread`]), so a client predicting the shot sees the
//! same pattern the server does.

use glam::Vec3;
use serde::{Deserialize, Serialize};

use super::Target;
use crate::collision::contents::MASK_SHOT;
use crate::collision::{CollisionQuery, Hull};
use crate::physics::BodyId;
use crate::rng::Rng;
use crate::scene::Scene;
use crate::simulation::PlayerId;

/// What a shot stopped at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Struck {
    /// Nothing within range.
    Nothing,
    /// The map.
    World,
    /// A prop.
    Prop(BodyId),
    /// A brush entity map logic runs (a button, a breakable...), by BSP
    /// model.
    Brush(u32),
    /// A player.
    Player(PlayerId),
}

/// Where a shot went.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ShotTrace {
    /// Where it stopped (the end of its range if it hit nothing).
    pub end: Vec3,
    /// Normal of the surface hit; zero if nothing was.
    pub normal: Vec3,
    /// What it stopped at.
    pub struck: Struck,
    /// The material of the surface hit, as the map's collision model
    /// indexes it; `None` for players, props and misses.
    pub material: Option<u16>,
}

/// Traces `hull` from `start` to `end` through `scene` and every target
/// but `shooter`'s own box, stopping at whatever comes first.
#[must_use]
pub fn trace(
    scene: &Scene,
    targets: &[Target],
    shooter: PlayerId,
    start: Vec3,
    end: Vec3,
    hull: &Hull,
) -> ShotTrace {
    let map = scene.trace_hull(start, end, hull, MASK_SHOT);
    let mut best = ShotTrace {
        end: map.end,
        normal: map.normal,
        struck: if map.hit() {
            struck_by(&map)
        } else {
            Struck::Nothing
        },
        material: map.material,
    };
    let mut best_fraction = map.fraction;
    for target in targets.iter().filter(|target| target.player != shooter) {
        if let Some((fraction, normal)) = target.sweep(start, end, hull.extents())
            && fraction < best_fraction
        {
            best_fraction = fraction;
            best = ShotTrace {
                end: start + (end - start) * fraction,
                normal,
                struck: Struck::Player(target.player),
                material: None,
            };
        }
    }
    best
}

/// What a trace that hit something hit: a prop, a brush entity or the
/// world.
#[must_use]
pub fn struck_by(trace: &crate::collision::Trace) -> Struck {
    match (trace.body, trace.model) {
        (Some(body), _) => Struck::Prop(body),
        (None, Some(model)) => Struck::Brush(u32::try_from(model).unwrap_or(u32::MAX)),
        (None, None) => Struck::World,
    }
}

/// A bullet direction within `spread` degrees of `forward` (unit length),
/// spread evenly over the cone's cross-section.
#[must_use]
pub fn spread(forward: Vec3, spread: f32, rng: &mut Rng) -> Vec3 {
    if spread <= 0.0 {
        return forward;
    }
    let (right, up) = forward.any_orthonormal_pair();
    let (x, y) = rng.in_disc();
    let radius = spread.to_radians().tan();
    (forward + (right * x + up * y) * radius).normalize_or(forward)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spread_stays_inside_the_cone() {
        let mut rng = Rng::seeded(&[3]);
        let forward = Vec3::new(1.0, 2.0, -0.5).normalize();
        let mut widest: f32 = 0.0;
        for _ in 0..500 {
            let direction = spread(forward, 5.0, &mut rng);
            assert!((direction.length() - 1.0).abs() < 1e-5);
            widest = widest.max(direction.angle_between(forward).to_degrees());
        }
        assert!(widest <= 5.0 + 1e-3, "{widest}");
        assert!(widest > 4.0, "the cone is used to its edge: {widest}");
        assert_eq!(spread(forward, 0.0, &mut rng), forward);
    }
}
