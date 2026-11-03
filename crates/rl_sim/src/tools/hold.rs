//! Where a held prop goes: the point ahead of the player that the
//! [`manipulator`](super::manipulator) and the [`hands`](super::hands) both
//! steer what they hold to.
//!
//! HL2DM's Gravity Gun and its players' hands place what they hold with the
//! same routine, `CGrabController::UpdateObject` (server.so; both
//! `CWeaponPhysCannon::UpdateObject` and `CPlayerPickupController::Use` call
//! it), so a prop sits as far out in either. Each tool steers it there at its
//! own pace.

use glam::Vec3;
use rl_data::HoldPoint;

use super::manipulator::Aim;
use crate::collision::contents::MASK_PROP_SOLID;
use crate::collision::{CollisionWorld, Hull};
use crate::movement::view_direction;
use crate::physics::{BodyId, PhysicsWorld};

/// Where the player who holds a prop is, and looks, this tick.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Carrier {
    /// Their feet.
    pub origin: Vec3,
    /// Their box around [`origin`](Self::origin).
    pub hull: Hull,
    /// Where they look from and along.
    pub aim: Aim,
}

impl Carrier {
    /// The middle of their box.
    pub(crate) fn center(&self) -> Vec3 {
        self.origin + (self.hull.mins + self.hull.maxs) * 0.5
    }
}

/// Where `carrier` holds the middle of the box of `body`, as
/// `CGrabController::UpdateObject` places it: along the view, its pitch
/// clamped to [`HoldPoint::max_pitch`], [`HoldPoint::gap`] plus `radius`
/// ahead of the eyes, where `radius` is half the diagonal of the player's
/// box across plus how far the prop reaches back towards them from its
/// origin; only half of `radius` ahead when a wall is within half of `gap` +
/// 2 × `radius`; and at least `radius` from the upright line through the
/// middle of the player's box. `None` if the prop is gone.
#[must_use]
pub fn hold_point(
    carrier: &Carrier,
    body: BodyId,
    world: &CollisionWorld,
    physics: &PhysicsWorld,
    point: &HoldPoint,
) -> Option<Vec3> {
    let pitch = wrapped_degrees(carrier.aim.pitch).clamp(-point.max_pitch, point.max_pitch);
    let forward = view_direction(pitch, carrier.aim.yaw);
    let reach_back = physics.extent(body, -forward)?.abs();
    let player_radius = carrier.hull.maxs.truncate().length();
    let radius = player_radius + reach_back;
    let distance = point.gap + 2.0 * radius;
    let start = carrier.aim.eye;
    let wall = world.trace_ray(start, start + forward * distance, MASK_PROP_SOLID);
    let mut end = if wall.fraction < 0.5 {
        start + forward * radius * 0.5
    } else {
        start + forward * (distance - radius)
    };
    let center = carrier.center();
    let nearest = Vec3::new(center.x, center.y, end.z);
    let away = end - nearest;
    if away.length() < radius {
        end = nearest + away.normalize_or_zero() * radius;
    }
    Some(end)
}

/// Where `body`'s centre of mass goes for the middle of its box to be at the
/// [`hold_point`]: the tools steer the centre of mass, the game the box.
/// `None` if the prop is gone.
#[must_use]
pub fn hold_target(
    carrier: &Carrier,
    body: BodyId,
    world: &CollisionWorld,
    physics: &PhysicsWorld,
    point: &HoldPoint,
) -> Option<Vec3> {
    let end = hold_point(carrier, body, world, physics, point)?;
    Some(end + physics.center_of_mass(body)? - physics.box_center(body)?)
}

/// `degrees` as an angle in `-180..180`.
fn wrapped_degrees(degrees: f32) -> f32 {
    (degrees + 180.0).rem_euclid(360.0) - 180.0
}

#[cfg(test)]
mod tests {
    use glam::Quat;

    use super::*;
    use crate::collision::contents::SOLID;
    use crate::collision::{ConvexHull, WorldBuilder};
    use crate::physics::{Motion, PropDesc};

    /// HL2DM's own numbers.
    const POINT: HoldPoint = HoldPoint {
        gap: 24.0,
        max_pitch: 75.0,
    };

    /// Half the diagonal of a player's box across.
    const PLAYER_RADIUS: f32 = 22.627_417;

    /// A prop of `pieces` at `origin`, unturned.
    fn prop(pieces: Vec<ConvexHull>, origin: Vec3) -> PropDesc {
        let data = rl_data::GameData::builtin().expect("builtin");
        PropDesc {
            pieces,
            origin,
            rotation: Quat::IDENTITY,
            motion: Motion::Dynamic,
            mass: 10.0,
            material: *data.physics.materials.get("wood"),
            linear_damping: 0.0,
            angular_damping: 0.0,
            debris: false,
            start_asleep: true,
            grabbable: true,
            free_on_grab: false,
            breakable: None,
            surface: String::new(),
            contents: SOLID,
        }
    }

    /// `map` and a world holding `prop` in it.
    fn scene(map: WorldBuilder, prop: &PropDesc) -> (CollisionWorld, PhysicsWorld, BodyId) {
        let map = map.build();
        let data = rl_data::GameData::builtin().expect("builtin");
        let mut physics = PhysicsWorld::new(&map, &data.physics, 0.015);
        let body = physics.add_prop(prop);
        (map, physics, body)
    }

    /// A player standing at the origin, eyes 64 up, looking at `pitch`
    /// along +X.
    fn carrier(pitch: f32) -> Carrier {
        Carrier {
            origin: Vec3::ZERO,
            hull: Hull {
                mins: Vec3::new(-16.0, -16.0, 0.0),
                maxs: Vec3::new(16.0, 16.0, 72.0),
            },
            aim: Aim {
                eye: Vec3::new(0.0, 0.0, 64.0),
                pitch,
                yaw: 0.0,
            },
        }
    }

    /// A box 16 units deep, its origin at the middle of its bottom.
    fn crate_box() -> ConvexHull {
        ConvexHull::from_box(Vec3::new(-8.0, -8.0, 0.0), Vec3::new(8.0, 8.0, 16.0))
    }

    #[test]
    fn a_prop_is_held_the_gap_plus_its_depth_plus_half_the_player_out() {
        let desc = prop(vec![crate_box()], Vec3::new(200.0, 0.0, 0.0));
        let (map, physics, body) = scene(WorldBuilder::new(), &desc);
        let end = hold_point(&carrier(0.0), body, &map, &physics, &POINT).expect("prop");
        let expected = Vec3::new(24.0 + PLAYER_RADIUS + 8.0, 0.0, 64.0);
        assert!(end.abs_diff_eq(expected, 1e-3), "{end}");
    }

    #[test]
    fn looking_steeply_holds_the_prop_at_the_pitch_limit() {
        let desc = prop(vec![crate_box()], Vec3::new(200.0, 0.0, 0.0));
        let (map, physics, body) = scene(WorldBuilder::new(), &desc);
        let steep = hold_point(&carrier(89.0), body, &map, &physics, &POINT).expect("prop");
        let limit = hold_point(&carrier(75.0), body, &map, &physics, &POINT).expect("prop");
        assert!(steep.abs_diff_eq(limit, 1e-3), "{steep} {limit}");
        assert!(steep.z < 64.0, "below the eyes looking down: {steep}");
    }

    #[test]
    fn a_wall_close_ahead_brings_the_prop_in_but_not_into_the_player() {
        // The wall is 40 out: within half of the 24 + 2 x 30.6 the hold
        // looks along. Half the radius out would be inside the player's
        // reach, so the prop goes a radius from their middle.
        let wall = WorldBuilder::new().solid_box(
            Vec3::new(40.0, -100.0, -100.0),
            Vec3::new(60.0, 100.0, 200.0),
        );
        let desc = prop(vec![crate_box()], Vec3::new(-200.0, 0.0, 0.0));
        let (map, physics, body) = scene(wall, &desc);
        let end = hold_point(&carrier(0.0), body, &map, &physics, &POINT).expect("prop");
        let expected = Vec3::new(PLAYER_RADIUS + 8.0, 0.0, 64.0);
        assert!(end.abs_diff_eq(expected, 1e-3), "{end}");
    }

    #[test]
    fn the_middle_of_the_box_goes_to_the_hold_point_not_the_centre_of_mass() {
        // A crate with a small block on one side: the bulk weighs the centre
        // of mass towards the crate, off the middle of the whole box.
        let block = ConvexHull::from_box(Vec3::new(8.0, -2.0, 0.0), Vec3::new(24.0, 2.0, 4.0));
        let desc = prop(vec![crate_box(), block], Vec3::new(200.0, 0.0, 0.0));
        let (map, physics, body) = scene(WorldBuilder::new(), &desc);
        let carrier = carrier(0.0);
        let end = hold_point(&carrier, body, &map, &physics, &POINT).expect("prop");
        let target = hold_target(&carrier, body, &map, &physics, &POINT).expect("prop");
        let middle = physics.box_center(body).expect("prop");
        let mass = physics.center_of_mass(body).expect("prop");
        assert!(
            middle.distance(mass) > 2.0,
            "the fixture's middle and centre of mass differ"
        );
        assert!(
            (target - mass + middle).abs_diff_eq(end, 1e-3),
            "the centre of mass steered to {target} puts the middle at the hold point {end}"
        );
    }

    #[test]
    fn angles_wrap_into_a_half_turn_either_way() {
        assert_eq!(wrapped_degrees(350.0), -10.0);
        assert_eq!(wrapped_degrees(-190.0), 170.0);
        assert_eq!(wrapped_degrees(45.0), 45.0);
    }
}
