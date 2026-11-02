//! The Manipulator: rustlife's grab-and-launch physics tool.
//!
//! - **Secondary fire, held**, aims at a prop: within grab range it is
//!   picked up; farther out, but within pull range, it is drawn towards the
//!   player until it is close enough to grab. A pickup the player can use is
//!   drawn all the way in, to be collected on touching them; one they cannot use
//!   is grabbed like any prop. Pressing secondary fire again lets go.
//! - **Holding.** The held prop's centre of mass is steered to a point in
//!   front of the eyes ([`HoldConfig`](rl_data::HoldConfig)), and it turns
//!   with the view's yaw, keeping the orientation it had relative to the
//!   player when grabbed. It is steered by setting its velocity each tick,
//!   not by moving it, so it still collides: it shoves other props aside and
//!   cannot pass through walls. Held back too far for too long, the grip
//!   breaks.
//! - **Primary fire** throws the held prop along the view, or punts a prop
//!   in range that is not held; the speed falls off with mass
//!   ([`LaunchConfig`](rl_data::LaunchConfig)).
//!
//! Only props the physics world says are grabbable can be held or pulled;
//! any dynamic prop can be punted. The aim is thicker than a line, so that small
//! props are easy to take: see [`find_target`]. All tuning is in
//! `tools/manipulator.ron`.

use glam::{Quat, Vec3};
use rl_data::{GrabConfig, ManipulatorConfig};
use serde::{Deserialize, Serialize};

use crate::collision::contents::{MOVEABLE, SOLID, WINDOW};
use crate::collision::{CollisionWorld, Hull};
use crate::movement::{Buttons, view_direction};
use crate::physics::{BodyId, PhysicsWorld};

/// What the tool's aim stops at.
const AIM_MASK: u32 = SOLID | WINDOW | MOVEABLE;

/// The box a look for something to take tries when its ray finds nothing: 4
/// units to each side (`FindObject`, `CheckForTarget`).
const AIM_BOX: Hull = Hull {
    mins: Vec3::splat(-4.0),
    maxs: Vec3::splat(4.0),
};

/// The box a punt looks along its line with, 8 units to each side
/// (`PrimaryAttack`).
const PUNT_BOX: Hull = Hull {
    mins: Vec3::splat(-8.0),
    maxs: Vec3::splat(8.0),
};

/// Something the Manipulator did this tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolEvent {
    /// Picked a prop up.
    Grabbed(BodyId),
    /// Let go of the held prop.
    Dropped(BodyId),
    /// Lost the held prop because it was held back.
    GripBroke(BodyId),
    /// Threw the held prop.
    Launched(BodyId),
    /// Knocked away a prop that was not held.
    Punted(BodyId),
    /// Primary fire with nothing to throw or punt.
    DryFire,
}

/// How long the tool rests after acting: the intervals of the weapon's fire
/// modes (`weapons/manipulator.ron`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Intervals {
    /// Seconds after a launch, punt or dry fire before primary fire acts
    /// again.
    pub primary: f32,
    /// Seconds after a grab or drop before secondary fire acts again.
    pub secondary: f32,
}

/// Where a player's tool is aimed from this tick.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Aim {
    /// The eye position, Source space.
    pub eye: Vec3,
    /// View pitch, degrees (positive looks down).
    pub pitch: f32,
    /// View yaw, degrees.
    pub yaw: f32,
}

impl Aim {
    fn forward(&self) -> Vec3 {
        view_direction(self.pitch, self.yaw)
    }

    /// The rotation of the view's yaw about +Z.
    fn yaw_rotation(&self) -> Quat {
        Quat::from_rotation_z(self.yaw.to_radians())
    }
}

/// A held prop.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
struct Held {
    body: BodyId,
    /// The prop's orientation relative to the view's yaw when grabbed.
    relative_rotation: Quat,
    /// How long the prop has been held back beyond the break distance.
    blocked_secs: f32,
}

/// What reaching for a prop depends on besides the aim.
struct Reach<'a> {
    /// The reaching player's box.
    player_body: BodyId,
    /// Pickups to pull in rather than grab.
    collectable: &'a [BodyId],
    /// How long the tool rests after a grab.
    intervals: Intervals,
}

/// One player's Manipulator.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Manipulator {
    held: Option<Held>,
    /// Seconds until primary fire is ready.
    primary_cooldown: f32,
    /// Seconds until secondary fire is ready.
    secondary_cooldown: f32,
    /// Buttons held last tick, for detecting presses.
    old_buttons: Buttons,
    /// Secondary fire let go of a prop and must be released before it
    /// reaches for another, so holding it does not grab the prop straight
    /// back.
    secondary_spent: bool,
}

impl Manipulator {
    /// The prop being held, if any.
    #[must_use]
    pub fn held(&self) -> Option<BodyId> {
        self.held.map(|held| held.body)
    }

    /// Lets go of the held prop, if any, as when its holder dies or puts
    /// the tool away; returns it.
    pub fn drop_held(&mut self, physics: &mut PhysicsWorld) -> Option<BodyId> {
        let held = self.held.take()?;
        physics.set_held(held.body, None);
        Some(held.body)
    }

    /// Runs the tool for one tick of `dt` seconds for `player` (whose box
    /// is `player_body`), aiming along `aim` with `buttons` held, resting
    /// for `intervals` after acting. The props in `collectable` are pickups the
    /// player can use: they are pulled all the way in rather than grabbed, and
    /// the player collects them on touching them.
    pub fn tick(
        &mut self,
        player_body: BodyId,
        aim: &Aim,
        buttons: Buttons,
        intervals: Intervals,
        collectable: &[BodyId],
        dt: f32,
        world: &CollisionWorld,
        physics: &mut PhysicsWorld,
        config: &ManipulatorConfig,
    ) -> Option<ToolEvent> {
        self.primary_cooldown = (self.primary_cooldown - dt).max(0.0);
        self.secondary_cooldown = (self.secondary_cooldown - dt).max(0.0);
        let secondary_held = buttons.contains(Buttons::ATTACK2);
        let secondary_pressed = secondary_held && !self.old_buttons.contains(Buttons::ATTACK2);
        self.old_buttons = buttons;
        if !secondary_held {
            self.secondary_spent = false;
        }

        // A prop removed or frozen under the player's grip is let go.
        if let Some(held) = self.held
            && physics.holder(held.body) != Some(player_body)
        {
            self.held = None;
        }

        if self.held.is_some() {
            if buttons.contains(Buttons::ATTACK) && self.primary_cooldown == 0.0 {
                self.launch(aim, physics, config, intervals)
            } else if secondary_pressed && self.secondary_cooldown == 0.0 {
                self.secondary_spent = true;
                self.release(physics, intervals).map(ToolEvent::Dropped)
            } else {
                self.hold(aim, dt, physics, config, intervals)
            }
        } else if buttons.contains(Buttons::ATTACK) && self.primary_cooldown == 0.0 {
            Some(self.punt(aim, world, physics, config, intervals))
        } else if secondary_held && !self.secondary_spent && self.secondary_cooldown == 0.0 {
            let reach = Reach {
                player_body,
                collectable,
                intervals,
            };
            self.reach(&reach, aim, dt, world, physics, config)
        } else {
            None
        }
    }

    /// Lets go of the held prop, restoring its gravity.
    fn release(&mut self, physics: &mut PhysicsWorld, intervals: Intervals) -> Option<BodyId> {
        let held = self.held.take()?;
        physics.set_held(held.body, None);
        self.secondary_cooldown = intervals.secondary;
        Some(held.body)
    }

    /// Throws the held prop along the view.
    fn launch(
        &mut self,
        aim: &Aim,
        physics: &mut PhysicsWorld,
        config: &ManipulatorConfig,
        intervals: Intervals,
    ) -> Option<ToolEvent> {
        let body = self.release(physics, intervals)?;
        let mass = physics.mass(body).unwrap_or(1.0);
        let (_, spin) = physics.velocity(body).unwrap_or_default();
        physics.set_velocity(body, aim.forward() * config.launch.speed_for(mass), spin);
        self.primary_cooldown = intervals.primary;
        Some(ToolEvent::Launched(body))
    }

    /// Knocks away the dynamic prop in the aim's way within punt range.
    fn punt(
        &mut self,
        aim: &Aim,
        world: &CollisionWorld,
        physics: &mut PhysicsWorld,
        config: &ManipulatorConfig,
        intervals: Intervals,
    ) -> ToolEvent {
        self.primary_cooldown = intervals.primary;
        let forward = aim.forward();
        let Some((body, point)) = punt_target(aim, world, physics, config.punt.range) else {
            return ToolEvent::DryFire;
        };
        let mass = physics.mass(body).unwrap_or(1.0);
        physics.apply_impulse(body, forward * config.punt.speed_for(mass) * mass, point);
        ToolEvent::Punted(body)
    }

    /// Grabs the prop in the aim's way if it is in grab range, or pulls it
    /// closer if it is in pull range, or if the player collects it on touching
    /// it.
    fn reach(
        &mut self,
        reach: &Reach<'_>,
        aim: &Aim,
        dt: f32,
        world: &CollisionWorld,
        physics: &mut PhysicsWorld,
        config: &ManipulatorConfig,
    ) -> Option<ToolEvent> {
        let grab = &config.grab;
        let (body, point) = find_target(aim, world, physics, config, grab.reach())?;
        if point.distance(aim.eye) <= grab.range && !reach.collectable.contains(&body) {
            physics.set_held(body, Some(reach.player_body));
            let rotation = physics
                .pose(body)
                .map_or(Quat::IDENTITY, |pose| pose.rotation);
            self.held = Some(Held {
                body,
                relative_rotation: aim.yaw_rotation().inverse() * rotation,
                blocked_secs: 0.0,
            });
            self.secondary_cooldown = reach.intervals.secondary;
            return Some(ToolEvent::Grabbed(body));
        }
        let center = physics.center_of_mass(body)?;
        let towards = (aim.eye - center).normalize_or_zero();
        let (velocity, spin) = physics.velocity(body).unwrap_or_default();
        // Speed up towards the pull speed, not straight to it.
        let change = (towards * grab.pull_speed - velocity).clamp_length_max(grab.pull_accel * dt);
        physics.set_velocity(body, velocity + change, spin);
        None
    }

    /// Steers the held prop to the hold point and the view's yaw; breaks
    /// the grip if it has been held back too long.
    fn hold(
        &mut self,
        aim: &Aim,
        dt: f32,
        physics: &mut PhysicsWorld,
        config: &ManipulatorConfig,
        intervals: Intervals,
    ) -> Option<ToolEvent> {
        let tuning = &config.hold;
        let held = self.held.as_mut()?;
        let (Some(center), Some(radius), Some(pose)) = (
            physics.center_of_mass(held.body),
            physics.radius(held.body),
            physics.pose(held.body),
        ) else {
            return None;
        };
        let target = aim.eye + aim.forward() * (tuning.distance + radius);
        let error = target - center;
        if error.length() > tuning.break_distance {
            held.blocked_secs += dt;
            if held.blocked_secs > tuning.break_time {
                let body = held.body;
                self.release(physics, intervals);
                return Some(ToolEvent::GripBroke(body));
            }
        } else {
            held.blocked_secs = 0.0;
        }

        let linear = (error / tuning.follow_time).clamp_length_max(tuning.max_speed);
        let target_rotation = aim.yaw_rotation() * held.relative_rotation;
        let (axis, mut angle) = (target_rotation * pose.rotation.inverse()).to_axis_angle();
        // The shorter way round.
        if angle > std::f32::consts::PI {
            angle -= std::f32::consts::TAU;
        }
        let angular = (axis * (angle / tuning.follow_time))
            .clamp_length_max(tuning.max_angular_speed.to_radians());
        physics.set_velocity(held.body, linear, angular);
        None
    }
}

/// The prop the tool would take looking along `aim`, within `range` of the eyes,
/// with the point it was found at: grabbable, light enough and not already held.
/// Secondary fire reaches [`GrabConfig::reach`] for one; the prongs open on one
/// within the shorter [`open_range`](GrabConfig::open_range).
///
/// The aim is thick, so that small props need not be under the very crosshair
/// (`CWeaponPhysCannon::FindObject`). A ray comes first; if it finds nothing to
/// take, a small box along the same line; if that finds nothing to take either,
/// the nearest such prop in plain sight within the cone of `grab.aim_cone` about
/// the view and `grab.aim_cone_range` of the eyes.
#[must_use]
pub fn find_target(
    aim: &Aim,
    world: &CollisionWorld,
    physics: &PhysicsWorld,
    config: &ManipulatorConfig,
    range: f32,
) -> Option<(BodyId, Vec3)> {
    let grab = &config.grab;
    let end = aim.eye + aim.forward() * range;
    let liftable = |&(body, _): &(BodyId, Vec3)| is_liftable(body, physics, grab);
    first_prop(aim.eye, end, &Hull::POINT, world, physics)
        .filter(liftable)
        .or_else(|| first_prop(aim.eye, end, &AIM_BOX, world, physics).filter(liftable))
        .or_else(|| in_cone(aim, world, physics, grab, range))
}

/// Whether the tool could take a prop: grabbable, light enough and not already
/// held.
fn is_liftable(body: BodyId, physics: &PhysicsWorld, grab: &GrabConfig) -> bool {
    physics.is_grabbable(body)
        && physics.mass(body).is_some_and(|mass| mass <= grab.max_mass)
        && physics.holder(body).is_none()
}

/// The nearest prop to take whose centre is within the aim's cone and `range`,
/// which the line of sight to its centre reaches first
/// (`CWeaponPhysCannon::FindObjectInCone`); of those equally near, the one with
/// the lowest id. With the point the line of sight reaches it at.
fn in_cone(
    aim: &Aim,
    world: &CollisionWorld,
    physics: &PhysicsWorld,
    grab: &GrabConfig,
    range: f32,
) -> Option<(BodyId, Vec3)> {
    let forward = aim.forward();
    let reach = grab.aim_cone_range.min(range);
    let (mins, maxs) = cone_bounds(aim.eye, forward, reach, grab.aim_cone);
    let mut inside: Vec<(f32, BodyId, Vec3)> = physics
        .props_near(mins, maxs)
        .into_iter()
        .filter(|&body| is_liftable(body, physics, grab))
        .filter_map(|body| {
            let center = physics.center_of_mass(body)?;
            let to = center - aim.eye;
            let distance = to.length();
            (distance < reach && to.dot(forward) > grab.aim_cone * distance)
                .then_some((distance, body, center))
        })
        .collect();
    inside.sort_unstable_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    inside.into_iter().find_map(|(_, body, center)| {
        first_prop(aim.eye, center, &Hull::POINT, world, physics).filter(|&(hit, _)| hit == body)
    })
}

/// The bounds of the points within `reach` of `eye` and within the cone of
/// cosine `cone` about `forward`, for a cone at most a hemisphere: those of the
/// cylinder around the axis as wide as the cone gets.
fn cone_bounds(eye: Vec3, forward: Vec3, reach: f32, cone: f32) -> (Vec3, Vec3) {
    let tip = eye + forward * reach;
    let radius = reach * (1.0 - cone * cone).max(0.0).sqrt();
    (eye.min(tip) - radius, eye.max(tip) + radius)
}

/// The dynamic prop primary fire would knock away within `range`, with the point
/// it was found at: a box first, then a ray, which finds what is too close to
/// the map for the box to reach (`CWeaponPhysCannon::PrimaryAttack`).
fn punt_target(
    aim: &Aim,
    world: &CollisionWorld,
    physics: &PhysicsWorld,
    range: f32,
) -> Option<(BodyId, Vec3)> {
    let end = aim.eye + aim.forward() * range;
    let dynamic = |&(body, _): &(BodyId, Vec3)| physics.is_dynamic(body);
    first_prop(aim.eye, end, &PUNT_BOX, world, physics)
        .filter(dynamic)
        .or_else(|| first_prop(aim.eye, end, &Hull::POINT, world, physics).filter(dynamic))
}

/// The prop `hull` hits going from `start` to `end`, with where it stopped;
/// `None` if it hits the map first, or nothing. Debris — which is how pickups
/// lie — counts, so they can be grabbed and punted too.
fn first_prop(
    start: Vec3,
    end: Vec3,
    hull: &Hull,
    world: &CollisionWorld,
    physics: &PhysicsWorld,
) -> Option<(BodyId, Vec3)> {
    let map = world.trace_hull(start, end, hull, AIM_MASK);
    if map.all_solid {
        return None;
    }
    // Nothing beyond the map's hit matters: trace props only that far.
    let props = physics.trace_hull_including_debris(start, map.end, hull, AIM_MASK, None);
    let trace = if props.hit() || props.start_solid {
        // Rescale the props' fraction of the shortened move to the whole
        // move, as `Scene` does.
        let mut props = props;
        props.fraction *= map.fraction;
        if !props.hit() {
            props.end = map.end;
        }
        map.nearest(props)
    } else {
        map
    };
    Some((trace.body?, trace.end))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cones_bounds_hold_every_point_within_it() {
        let cone = 0.97_f32;
        let angle = cone.acos();
        let forwards = [
            Vec3::X,
            Vec3::NEG_Z,
            Vec3::new(0.6, -0.48, 0.64),
            view_direction(35.0, 200.0),
        ];
        let eye = Vec3::new(10.0, -20.0, 30.0);
        for forward in forwards {
            let (mins, maxs) = cone_bounds(eye, forward, 251.0, cone);
            let across = forward.any_orthonormal_vector();
            for turn in 0..32 {
                let around = Quat::from_axis_angle(forward, turn as f32 * 0.196);
                let edge = Quat::from_axis_angle(around * across, angle) * forward;
                for along in [0.0, 100.0, 250.0] {
                    let point = eye + edge * along;
                    assert!(
                        point.cmpge(mins).all() && point.cmple(maxs).all(),
                        "{point} outside {mins}..{maxs} along {forward}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_cones_bounds_are_much_smaller_than_a_box_around_the_eyes() {
        let (mins, maxs) = cone_bounds(Vec3::ZERO, Vec3::X, 251.0, 0.97);
        let size = maxs - mins;
        assert!(size.y < 125.0 && size.z < 125.0, "{size}");
        assert!(size.x < 251.0 + 125.0, "{size}");
    }
}
