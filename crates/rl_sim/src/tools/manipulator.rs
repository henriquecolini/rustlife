//! The Manipulator: rustlife's grab-and-launch physics tool.
//!
//! The numbers are the Gravity Gun's in HL2DM's `server.so`
//! (`CWeaponPhysCannon`, `CGrabController`); see `docs/physics.md`.
//!
//! - **Secondary fire, held**, aims at a prop: within grab range it is
//!   picked up; farther out, but within pull range, it is drawn towards the
//!   player (an impulse every tick) until it is close enough to grab. With
//!   nothing under the crosshair, the prop nearest the view direction
//!   within grab range is taken. Pressing secondary fire again lets go.
//! - **Holding.** The held prop's centre of mass is steered to a point in
//!   front of the eyes ([`HoldConfig`]), and it turns
//!   with the view's yaw, keeping the orientation it had relative to the
//!   player when grabbed. It is steered by setting its velocity each tick,
//!   not by moving it, so it still collides: it shoves other props aside and
//!   cannot pass through walls. Held back too far for too long, the grip
//!   breaks.
//! - **Primary fire** throws the held prop along the view, or punts a prop
//!   in range that is not held
//!   ([`LaunchConfig`](rl_data::LaunchConfig),
//!   [`PuntConfig`](rl_data::PuntConfig)).
//!
//! Only props the physics world says are grabbable can be held or pulled;
//! any dynamic prop can be punted. All tuning is in `tools/manipulator.ron`.

use glam::{Quat, Vec3};
use rl_data::{GrabConfig, HoldConfig, ManipulatorConfig};
use serde::{Deserialize, Serialize};

use crate::collision::contents::{MOVEABLE, SOLID, WINDOW};
use crate::collision::{CollisionWorld, Hull};
use crate::movement::{Buttons, view_direction};
use crate::physics::{BodyId, PhysicsWorld};

/// What the tool's aiming ray stops at.
const AIM_MASK: u32 = SOLID | WINDOW | MOVEABLE;

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
    /// The height of the sole of the player's box...
    pub feet: f32,
    /// ...and of its top: the vertical axis a held prop is kept off.
    pub top: f32,
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
    /// How far the prop has been from its hold point, averaged over about
    /// a second (`CGrabController::ComputeError`).
    error: f32,
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
    /// for `intervals` after acting.
    pub fn tick(
        &mut self,
        player_body: BodyId,
        aim: &Aim,
        buttons: Buttons,
        intervals: Intervals,
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
                let dropped = self.release(physics, intervals);
                self.rest(intervals.secondary);
                dropped.map(ToolEvent::Dropped)
            } else {
                self.hold(aim, dt, world, physics, config, intervals)
            }
        } else if buttons.contains(Buttons::ATTACK) && self.primary_cooldown == 0.0 {
            Some(self.punt(aim, world, physics, config, intervals))
        } else if secondary_held && !self.secondary_spent && self.secondary_cooldown == 0.0 {
            self.reach(player_body, aim, dt, world, physics, config, intervals)
        } else {
            None
        }
    }

    /// Keeps both fire buttons idle for `seconds`: a launch, punt or drop
    /// makes the tool rest as a whole.
    fn rest(&mut self, seconds: f32) {
        self.primary_cooldown = seconds;
        self.secondary_cooldown = seconds;
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
        self.rest(intervals.primary);
        Some(ToolEvent::Launched(body))
    }

    /// Knocks away the dynamic prop under the crosshair within punt range.
    fn punt(
        &mut self,
        aim: &Aim,
        world: &CollisionWorld,
        physics: &mut PhysicsWorld,
        config: &ManipulatorConfig,
        intervals: Intervals,
    ) -> ToolEvent {
        self.primary_cooldown = intervals.primary;
        let mut direction = aim.forward();
        let Some((body, point)) = aim_at(aim, config.punt.range, world, physics) else {
            return ToolEvent::DryFire;
        };
        if !physics.is_dynamic(body) {
            return ToolEvent::DryFire;
        }
        let Some(center) = physics.center_of_mass(body) else {
            return ToolEvent::DryFire;
        };
        let mass = physics.mass(body).unwrap_or(1.0);
        // Both impulses go along the view (a downward aim lifts instead);
        // the second, at the point hit, also spins the prop.
        direction.z = config.punt.direction_z(direction.z);
        let (through_center, at_point) = config.punt.impulses(mass);
        physics.apply_impulse(body, direction * through_center, center);
        physics.apply_impulse(body, direction * at_point, point);
        self.rest(intervals.primary);
        ToolEvent::Punted(body)
    }

    /// Looks for a prop to take: the one under the crosshair within pull
    /// range, or failing that the one nearest the view direction within
    /// grab range. Grabs it if it is in grab range, pulls it closer if not;
    /// a fruitless look rests the tool.
    fn reach(
        &mut self,
        player_body: BodyId,
        aim: &Aim,
        dt: f32,
        world: &CollisionWorld,
        physics: &mut PhysicsWorld,
        config: &ManipulatorConfig,
        intervals: Intervals,
    ) -> Option<ToolEvent> {
        let grab = &config.grab;
        let liftable = |physics: &PhysicsWorld, body: BodyId| {
            physics.is_grabbable(body)
                && physics.mass(body).is_some_and(|mass| mass <= grab.max_mass)
                && physics.holder(body).is_none()
        };
        let found = match aim_at(aim, grab.pull_range.max(grab.range), world, physics) {
            Some((body, point)) => Some((body, point.distance(aim.eye))),
            None => find_in_cone(aim, grab, world, physics, &liftable),
        };
        let Some((body, distance)) = found.filter(|&(body, _)| liftable(physics, body)) else {
            self.secondary_cooldown = intervals.secondary;
            return None;
        };
        if distance <= grab.range {
            physics.set_held(body, Some(player_body));
            let rotation = physics
                .pose(body)
                .map_or(Quat::IDENTITY, |pose| pose.rotation);
            self.held = Some(Held {
                body,
                relative_rotation: aim.yaw_rotation().inverse() * rotation,
                error: 0.0,
            });
            self.secondary_cooldown = grab.grab_rest;
            return Some(ToolEvent::Grabbed(body));
        }
        pull(body, aim, dt, physics, grab);
        None
    }

    /// Steers the held prop to the hold point and the view's yaw; breaks
    /// the grip if it has been held back too long.
    fn hold(
        &mut self,
        aim: &Aim,
        dt: f32,
        world: &CollisionWorld,
        physics: &mut PhysicsWorld,
        config: &ManipulatorConfig,
        intervals: Intervals,
    ) -> Option<ToolEvent> {
        let tuning = &config.hold;
        let held = self.held.as_mut()?;
        let (Some(center), Some(pose)) =
            (physics.center_of_mass(held.body), physics.pose(held.body))
        else {
            return None;
        };
        let target = hold_point(aim, held.body, world, physics, tuning);
        let error = target - center;

        // `CGrabController::ComputeError`: a running average of the
        // distance, each tick weighing in by its length in seconds.
        let mut distance = error.length();
        if distance > tuning.max_speed {
            distance *= 0.5;
        }
        let weight = dt.min(1.0);
        held.error = (1.0 - weight) * held.error + weight * distance;
        if held.error > tuning.break_distance {
            let body = held.body;
            self.release(physics, intervals);
            return Some(ToolEvent::GripBroke(body));
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

/// Gives a pulled prop this tick's impulse towards the eyes.
fn pull(body: BodyId, aim: &Aim, dt: f32, physics: &mut PhysicsWorld, grab: &GrabConfig) {
    let (Some(center), Some(mass)) = (physics.center_of_mass(body), physics.mass(body)) else {
        return;
    };
    let towards = (aim.eye - center).normalize_or_zero();
    physics.apply_impulse(body, towards * pull_impulse(grab, mass, dt), center);
}

/// The size of the impulse a pull gives a prop of `mass` kg over a tick of
/// `dt` seconds: `pull_impulse` per reference tick, scaled down for light
/// props.
fn pull_impulse(grab: &GrabConfig, mass: f32, dt: f32) -> f32 {
    let light = if mass < grab.pull_light_mass {
        (mass + 0.5) * grab.pull_light_gain
    } else {
        1.0
    };
    grab.pull_impulse * light * dt / grab.reference_tick
}

/// Where the centre of the held `body` is steered
/// (`CGrabController::UpdateObject`): ahead of the eyes along the view, as
/// far as its reach and the clearance say, brought in if a wall is in the
/// way, and kept off the player's own vertical axis.
fn hold_point(
    aim: &Aim,
    body: BodyId,
    world: &CollisionWorld,
    physics: &PhysicsWorld,
    tuning: &HoldConfig,
) -> Vec3 {
    let limit = tuning.pitch_limit;
    let forward = view_direction(aim.pitch.clamp(-limit, limit), aim.yaw);
    // How far the prop reaches back towards the player, plus the player's
    // own radius.
    let behind = physics
        .extent(body, -forward)
        .or_else(|| physics.radius(body))
        .unwrap_or(0.0);
    let reach = tuning.player_radius + behind;
    let distance = tuning.clearance + 2.0 * reach;
    let probe = world.trace_hull(
        aim.eye,
        aim.eye + forward * distance,
        &Hull::POINT,
        AIM_MASK,
    );
    let along = if probe.fraction < 0.5 {
        0.5 * reach
    } else {
        distance - reach
    };
    let target = aim.eye + forward * along;

    // A sphere of the reach, at the target, must not overlap the player's
    // axis.
    let nearest = Vec3::new(aim.eye.x, aim.eye.y, target.z.clamp(aim.feet, aim.top));
    let away = target - nearest;
    let gap = away.length();
    if gap < reach {
        let outward = if gap > 1e-4 { away / gap } else { forward };
        nearest + outward * reach
    } else {
        target
    }
}

/// The liftable prop within grab range nearest the view direction, with its
/// distance, if it is within the cone and in sight (`FindObjectInCone`).
fn find_in_cone(
    aim: &Aim,
    grab: &GrabConfig,
    world: &CollisionWorld,
    physics: &PhysicsWorld,
    liftable: &impl Fn(&PhysicsWorld, BodyId) -> bool,
) -> Option<(BodyId, f32)> {
    let forward = aim.forward();
    physics
        .props()
        .filter(|&body| liftable(physics, body))
        .filter_map(|body| {
            let to = physics.center_of_mass(body)? - aim.eye;
            let distance = to.length();
            if distance < 1e-3 || distance > grab.range {
                return None;
            }
            let alignment = to.dot(forward) / distance;
            if alignment < grab.cone {
                return None;
            }
            // Walls in between hide it.
            let sight = world.trace_hull(aim.eye, aim.eye + to, &Hull::POINT, AIM_MASK);
            (!sight.all_solid && sight.fraction >= 1.0).then_some((body, distance, alignment))
        })
        .max_by(|a, b| a.2.total_cmp(&b.2))
        .map(|(body, distance, _)| (body, distance))
}

/// The prop the aiming ray hits within `range`, with the point hit; `None`
/// if it hits the map first, or nothing. Debris — which is how pickups lie —
/// counts, so they can be grabbed and punted too.
fn aim_at(
    aim: &Aim,
    range: f32,
    world: &CollisionWorld,
    physics: &PhysicsWorld,
) -> Option<(BodyId, Vec3)> {
    let end = aim.eye + aim.forward() * range;
    let map = world.trace_hull(aim.eye, end, &Hull::POINT, AIM_MASK);
    if map.all_solid {
        return None;
    }
    // Nothing beyond the map's hit matters: trace props only that far.
    let props = physics.trace_hull_including_debris(aim.eye, map.end, &Hull::POINT, AIM_MASK, None);
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

    fn grab() -> GrabConfig {
        GrabConfig {
            range: 250.0,
            max_mass: 250.0,
            pull_range: 1000.0,
            cone: 0.97,
            pull_impulse: 4000.0,
            pull_light_mass: 50.0,
            pull_light_gain: 0.02,
            reference_tick: 0.015,
            grab_rest: 0.1,
        }
    }

    #[test]
    fn heavy_props_get_the_full_pull_impulse() {
        assert_eq!(pull_impulse(&grab(), 50.0, 0.015), 4000.0);
        assert_eq!(pull_impulse(&grab(), 200.0, 0.015), 4000.0);
    }

    #[test]
    fn light_props_get_a_scaled_pull_impulse() {
        // (10 + 0.5) * 0.02 = 0.21.
        let impulse = pull_impulse(&grab(), 10.0, 0.015);
        assert!((impulse - 840.0).abs() < 1e-3, "{impulse}");
        // Which still speeds a light prop up about as much as a 50 kg one.
        let light = impulse / 10.0;
        let medium = pull_impulse(&grab(), 49.9, 0.015) / 49.9;
        assert!((light - medium).abs() < 6.0, "{light} vs {medium}");
    }

    #[test]
    fn the_pull_impulse_follows_the_tick_length() {
        let fast = pull_impulse(&grab(), 100.0, 0.0075);
        assert!((fast - 2000.0).abs() < 1e-3, "{fast}");
    }
}
