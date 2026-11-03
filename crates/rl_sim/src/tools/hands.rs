//! Bare hands: carrying a light prop with the use key, as HL2DM's players do
//! (`CPlayerPickupController`, which steers the prop with the Gravity Gun's
//! `CGrabController`).
//!
//! - **Use** on a prop picks it up if it is light and small enough
//!   ([`PickupLimits`]) and the weapon in hand can be
//!   put away: the simulation puts it away for as long as the prop is carried.
//!   [`find_prop`] is what use finds (`CBasePlayer::FindUseEntity`).
//! - **Carrying.** The prop is steered to the point ahead of the eyes where
//!   the Manipulator would hold it ([`hold_target`]), turning with the view's
//!   yaw and keeping the orientation it had relative to it when picked up,
//!   like the Manipulator's, but faster. Held
//!   back from that point (a smoothed distance, [`CarryHold::drop_error`]),
//!   it falls from the hands.
//! - **Primary fire** tosses it along the view: a gentle impulse that grows
//!   with its mass, so light props go fast and heavy ones barely leave the
//!   hands ([`Toss`]). **Secondary fire**, or **use** again, sets it down: it
//!   keeps going at no more than a walk, or stops if it is touching
//!   something.
//!
//! All tuning is in `tools/hands.ron`.

use glam::{Quat, Vec3};
use rl_data::{CarryHold, HandsConfig, PickupLimits, ReleaseLimits, Toss};
use serde::{Deserialize, Serialize};

use super::hold::{Carrier, hold_target};
use super::manipulator::{first_hit, hold_rotation};
use crate::collision::contents::{GRATE, MONSTER, MOVEABLE, PLAYER_CLIP, SOLID, WINDOW};
use crate::collision::{CollisionWorld, Hull};
use crate::movement::Buttons;
use crate::physics::{BodyId, PhysicsWorld};
use crate::rng::Rng;

/// What use traces stop at: `MASK_SOLID`, debris and player clips
/// (`FindUseEntity`'s `useableContents`).
const USE_MASK: u32 = SOLID | WINDOW | GRATE | MOVEABLE | MONSTER | PLAYER_CLIP;

/// How far use's first look, a ray straight along the view, reaches, units.
const USE_RAY_LENGTH: f32 = 1024.0;

/// The box use's later looks sweep, 16 units to each side...
const USE_BOX: Hull = Hull {
    mins: Vec3::splat(-16.0),
    maxs: Vec3::splat(16.0),
};

/// ...this far...
const USE_BOX_LENGTH: f32 = 72.0;

/// ...tilted down from the view by these tangents: 45, 30, 20, 15 and 10
/// degrees, then 10 and 15 degrees up.
const USE_BOX_TILTS: [f32; 7] = [
    1.0,
    0.577_350_26,
    0.363_970_23,
    0.267_949_2,
    0.176_326_98,
    -0.176_326_98,
    -0.267_949_2,
];

/// Kilogram square metres per kilogram square unit: Source's angular impulses
/// are in metres, the physics world's in units.
const SQUARE_METRES_PER_SQUARE_UNIT: f32 = 0.0254 * 0.0254;

/// Something the hands did this tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HandsEvent {
    /// Picked a prop up.
    PickedUp(BodyId),
    /// Set the prop down, or lost it.
    Dropped(BodyId),
    /// Tossed the prop.
    Tossed(BodyId),
}

/// What use finds looking for something to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UseTarget {
    /// A prop the hands can carry.
    Prop(BodyId),
    /// A brush entity (a button, a door) in plain sight within reach: the
    /// map's to use, not the hands'.
    Mover,
    /// Nothing the hands can take.
    Nothing,
}

/// The prop in the hands.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
struct Carried {
    body: BodyId,
    /// Its orientation relative to the view's yaw when picked up.
    relative_rotation: Quat,
    /// Where its centre of mass was last steered to.
    target: Vec3,
    /// How far it has been held back from there, smoothed (`m_error`).
    error: f32,
    /// Seconds since the error was last measured; below zero during the grace
    /// after the pickup (`m_errorTime`).
    error_time: f32,
}

/// One player's hands.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Hands {
    carried: Option<Carried>,
}

impl Hands {
    /// The prop being carried, if any.
    #[must_use]
    pub fn held(&self) -> Option<BodyId> {
        self.carried.map(|carried| carried.body)
    }

    /// The carried prop's orientation relative to the view's yaw when picked
    /// up, if one is carried: see [`hold_rotation`].
    #[must_use]
    pub fn held_rotation(&self) -> Option<Quat> {
        self.carried.map(|carried| carried.relative_rotation)
    }

    /// Picks `body` up for `carrier`, whose box is `player_body`. The caller
    /// has checked it with [`can_carry`] and put the weapon away.
    pub fn pick_up(
        &mut self,
        body: BodyId,
        player_body: BodyId,
        carrier: &Carrier,
        physics: &mut PhysicsWorld,
        config: &HandsConfig,
    ) {
        physics.set_held(body, Some(player_body));
        let rotation = physics
            .pose(body)
            .map_or(Quat::IDENTITY, |pose| pose.rotation);
        let target = physics.center_of_mass(body).unwrap_or(carrier.aim.eye);
        self.carried = Some(Carried {
            body,
            relative_rotation: Quat::from_rotation_z(carrier.aim.yaw.to_radians()).inverse()
                * rotation,
            target,
            error: 0.0,
            error_time: -config.hold.error_grace,
        });
    }

    /// Sets the carried prop down, if any, as use pressed again or its carrier
    /// dying does; returns it.
    pub fn drop_held(
        &mut self,
        physics: &mut PhysicsWorld,
        release: &ReleaseLimits,
    ) -> Option<BodyId> {
        self.let_go(physics, release, false)
    }

    /// Carries the prop for one tick of `dt` seconds with `buttons` held, for
    /// `carrier` (whose box is `player_body`): tosses it, sets it down, lets it
    /// fall when it is held back, or steers it on. `rng` draws a toss's spin.
    pub fn tick(
        &mut self,
        player_body: BodyId,
        carrier: &Carrier,
        buttons: Buttons,
        dt: f32,
        rng: &mut Rng,
        world: &CollisionWorld,
        physics: &mut PhysicsWorld,
        config: &HandsConfig,
    ) -> Option<HandsEvent> {
        let held = self.carried.as_mut()?;
        let body = held.body;
        // Taken from the hands: broken, removed or frozen.
        if physics.holder(body) != Some(player_body) {
            self.carried = None;
            return Some(HandsEvent::Dropped(body));
        }
        held.error_time += dt;
        let error = measure_error(held, physics, &config.hold);
        if buttons.contains(Buttons::ATTACK2)
            || error > config.hold.drop_error
            || !physics.is_dynamic(body)
        {
            return self
                .let_go(physics, &config.release, false)
                .map(HandsEvent::Dropped);
        }
        if buttons.contains(Buttons::ATTACK) {
            return self.toss(carrier, rng, physics, config);
        }
        if let Some(target) = hold_target(carrier, body, world, physics, &config.hold.point) {
            held.target = target;
        }
        steer(held, carrier.aim.yaw, physics, &config.hold);
        None
    }

    /// Lets go of the carried prop: it stops if it is set down touching
    /// something, and otherwise goes on no faster than `release` allows.
    fn let_go(
        &mut self,
        physics: &mut PhysicsWorld,
        release: &ReleaseLimits,
        tossed: bool,
    ) -> Option<BodyId> {
        let body = self.carried.take()?.body;
        physics.set_held(body, None);
        let (linear, angular) = physics.velocity(body).unwrap_or_default();
        if !tossed && physics.in_contact(body) {
            physics.set_velocity(body, Vec3::ZERO, Vec3::ZERO);
        } else {
            physics.set_velocity(
                body,
                linear.clamp_length_max(release.max_speed),
                angular.clamp_length_max(release.max_angular_speed.to_radians()),
            );
        }
        Some(body)
    }

    /// Tosses the carried prop along the view, with a little spin.
    fn toss(
        &mut self,
        carrier: &Carrier,
        rng: &mut Rng,
        physics: &mut PhysicsWorld,
        config: &HandsConfig,
    ) -> Option<HandsEvent> {
        let body = self.let_go(physics, &config.release, true)?;
        let toss = &config.toss;
        let mass = physics.mass(body).unwrap_or(1.0);
        if let Some(center) = physics.center_of_mass(body) {
            let impulse = carrier.aim.forward() * toss.impulse(mass);
            physics.apply_impulse(body, impulse, center);
        }
        physics.apply_angular_impulse(body, toss_spin(toss, mass, rng));
        Some(HandsEvent::Tossed(body))
    }
}

/// The random spin a toss gives a prop of `mass` kg, kg·units²·radians/s:
/// up to [`Toss::spin`] times the mass factor about each axis, in Source's
/// degrees and metres (`RandomAngularImpulse`).
fn toss_spin(toss: &Toss, mass: f32, rng: &mut Rng) -> Vec3 {
    let mut axis = || (rng.unit() * 2.0 - 1.0) * toss.spin;
    let degrees = Vec3::new(axis(), axis(), axis()) * toss.factor(mass);
    Vec3::new(
        degrees.x.to_radians(),
        degrees.y.to_radians(),
        degrees.z.to_radians(),
    ) / SQUARE_METRES_PER_SQUARE_UNIT
}

/// How far the carried prop is held back from where it was last steered,
/// smoothed: each measurement weighs in by the seconds since the last
/// (at most one), and counts half while it is more than the prop could
/// cover at full speed in that time. Nothing counts during the grace after
/// the pickup (`CGrabController::ComputeError`).
fn measure_error(carried: &mut Carried, physics: &PhysicsWorld, hold: &CarryHold) -> f32 {
    if carried.error_time <= 0.0 {
        return 0.0;
    }
    let Some(center) = physics.center_of_mass(carried.body) else {
        return carried.error;
    };
    let weight = carried.error_time.min(1.0);
    let mut distance = carried.target.distance(center);
    if distance / weight > hold.max_speed {
        distance *= 0.5;
    }
    carried.error = (1.0 - weight) * carried.error + distance * weight;
    carried.error_time = 0.0;
    carried.error
}

/// Sets the carried prop's velocity to bring its centre of mass to its target
/// and its orientation to the view's yaw over [`CarryHold::follow_time`].
fn steer(carried: &Carried, yaw: f32, physics: &mut PhysicsWorld, hold: &CarryHold) {
    let (Some(center), Some(pose)) = (
        physics.center_of_mass(carried.body),
        physics.pose(carried.body),
    ) else {
        return;
    };
    let linear = carry_velocity(carried.target - center, hold);
    let target_rotation = hold_rotation(yaw, carried.relative_rotation);
    let (axis, mut angle) = (target_rotation * pose.rotation.inverse()).to_axis_angle();
    // The shorter way round.
    if angle > std::f32::consts::PI {
        angle -= std::f32::consts::TAU;
    }
    let angular =
        (axis * (angle / hold.follow_time)).clamp_length_max(hold.max_angular_speed.to_radians());
    physics.set_velocity(carried.body, linear, angular);
}

/// The velocity that steers a carried prop's centre of mass to a target
/// `error` away. Set each tick, it closes the gap over about
/// [`CarryHold::follow_time`].
#[must_use]
pub fn carry_velocity(error: Vec3, hold: &CarryHold) -> Vec3 {
    (error / hold.follow_time).clamp_length_max(hold.max_speed)
}

/// Whether the hands could pick `body` up: a prop players may pick up, light
/// and small enough, and in nobody's hands (`CBasePlayer::CanPickupObject`).
#[must_use]
pub fn can_carry(body: BodyId, physics: &PhysicsWorld, limits: &PickupLimits) -> bool {
    physics.is_grabbable(body)
        && physics.holder(body).is_none()
        && physics
            .mass(body)
            .is_some_and(|mass| mass <= limits.max_mass)
        && physics
            .size(body)
            .is_some_and(|size| size.max_element() <= limits.max_size)
}

/// What use finds for `carrier` within `reach` (`CBasePlayer::FindUseEntity`,
/// as far as props go). A ray along the view comes first: a prop it meets that
/// the hands can carry is taken, and a brush entity it meets within reach is
/// left to the map. Then boxes swept a short way along lines tilted down (and
/// then a little up) from the view, until one meets a prop to carry. A prop
/// counts when the point where the look met it is within `reach` of the eyes,
/// heights within the player's own counting as level with them.
#[must_use]
pub fn find_prop(
    carrier: &Carrier,
    reach: f32,
    world: &CollisionWorld,
    physics: &PhysicsWorld,
    limits: &PickupLimits,
) -> UseTarget {
    let aim = &carrier.aim;
    let forward = aim.forward();
    let up = view_up(aim.pitch, aim.yaw);
    let within = |point: Vec3| use_distance(carrier, point) < reach;
    let carriable = |body| can_carry(body, physics, limits);

    let end = aim.eye + forward * USE_RAY_LENGTH;
    if let Some(trace) = first_hit(aim.eye, end, &Hull::POINT, USE_MASK, world, physics) {
        match (trace.body, trace.model) {
            (Some(body), _) if carriable(body) && within(trace.end) => {
                return UseTarget::Prop(body);
            }
            (None, Some(_)) if aim.eye.distance(trace.end) <= reach => return UseTarget::Mover,
            _ => {}
        }
    }
    for tilt in USE_BOX_TILTS {
        let direction = (forward - up * tilt).normalize_or_zero();
        let end = aim.eye + direction * USE_BOX_LENGTH;
        let Some(trace) = first_hit(aim.eye, end, &USE_BOX, USE_MASK, world, physics) else {
            continue;
        };
        if let Some(body) = trace.body
            && carriable(body)
            && within(trace.end)
        {
            return UseTarget::Prop(body);
        }
    }
    UseTarget::Nothing
}

/// How far `point` is from `carrier`'s eyes as use measures it: the height
/// counts only beyond a band as tall as the player's box, starting at its
/// middle (Source adds the box's own bounds to its centre).
fn use_distance(carrier: &Carrier, point: Vec3) -> f32 {
    let center_z = carrier.center().z;
    let (low, high) = (
        center_z + carrier.hull.mins.z,
        center_z + carrier.hull.maxs.z,
    );
    let mut delta = point - carrier.aim.eye;
    delta.z = if point.z < low {
        low - point.z
    } else if point.z > high {
        point.z - high
    } else {
        0.0
    };
    delta.length()
}

/// The view's up vector at `pitch` and `yaw` degrees, no roll (Source's
/// `AngleVectors`).
fn view_up(pitch: f32, yaw: f32) -> Vec3 {
    let (sin_pitch, cos_pitch) = pitch.to_radians().sin_cos();
    let (sin_yaw, cos_yaw) = yaw.to_radians().sin_cos();
    Vec3::new(sin_pitch * cos_yaw, sin_pitch * sin_yaw, cos_pitch)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::movement::view_direction;
    use crate::tools::manipulator::Aim;

    #[test]
    fn the_view_up_is_square_to_the_view() {
        for (pitch, yaw) in [(0.0, 0.0), (35.0, 120.0), (-80.0, 300.0), (89.0, 45.0)] {
            let up = view_up(pitch, yaw);
            assert!((up.length() - 1.0).abs() < 1e-5);
            assert!(up.dot(view_direction(pitch, yaw)).abs() < 1e-5);
            assert!(up.z >= 0.0, "up points up");
        }
        assert!(view_up(0.0, 90.0).abs_diff_eq(Vec3::Z, 1e-6));
    }

    fn carrier() -> Carrier {
        Carrier {
            origin: Vec3::ZERO,
            hull: Hull {
                mins: Vec3::new(-16.0, -16.0, 0.0),
                maxs: Vec3::new(16.0, 16.0, 72.0),
            },
            aim: Aim {
                eye: Vec3::new(0.0, 0.0, 64.0),
                pitch: 0.0,
                yaw: 0.0,
            },
        }
    }

    #[test]
    fn heights_within_the_band_are_level_with_the_eyes() {
        let carrier = carrier();
        // The band runs from the box's middle (36) to 72 above it (108);
        // below it, 40 under the band and 30 out is 50 away.
        assert_eq!(use_distance(&carrier, Vec3::new(50.0, 0.0, 40.0)), 50.0);
        assert_eq!(use_distance(&carrier, Vec3::new(50.0, 0.0, 100.0)), 50.0);
        assert_eq!(use_distance(&carrier, Vec3::new(30.0, 0.0, -4.0)), 50.0);
    }

    #[test]
    fn the_error_is_smoothed_and_waits_out_the_grace() {
        let hold = CarryHold {
            point: rl_data::HoldPoint {
                gap: 24.0,
                max_pitch: 75.0,
            },
            follow_time: 0.015,
            max_speed: 1000.0,
            max_angular_speed: 1000.0,
            drop_error: 12.0,
            error_grace: 1.0,
        };
        let physics = PhysicsWorld::new(
            &crate::collision::WorldBuilder::new().build(),
            &rl_data::GameData::builtin().expect("builtin").physics,
            0.015,
        );
        // No prop: the error stays as it was, past the grace or not.
        let mut carried = Carried {
            body: BodyId::from_index(7),
            relative_rotation: Quat::IDENTITY,
            target: Vec3::ZERO,
            error: 3.0,
            error_time: -0.5,
        };
        assert_eq!(measure_error(&mut carried, &physics, &hold), 0.0);
        assert_eq!(carried.error_time, -0.5, "the grace is left to run");
        carried.error_time = 0.015;
        assert_eq!(measure_error(&mut carried, &physics, &hold), 3.0);
    }

    #[test]
    fn spin_grows_with_the_mass_factor() {
        let toss = Toss {
            force: 1000.0,
            masses: (0.5, 15.0),
            factors: (0.5, 4.0),
            spin: 10.0,
        };
        let light = toss_spin(&toss, 0.5, &mut Rng::seeded(&[1]));
        let heavy = toss_spin(&toss, 15.0, &mut Rng::seeded(&[1]));
        assert!(heavy.abs_diff_eq(light * 8.0, 1e-2), "{light} {heavy}");
        let most = 10.0_f32.to_radians() * 4.0 / SQUARE_METRES_PER_SQUARE_UNIT;
        assert!(heavy.max_element() <= most && heavy.min_element() >= -most);
    }
}
