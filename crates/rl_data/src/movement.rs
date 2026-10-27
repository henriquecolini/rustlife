//! `movement.ron`: how players move.
//!
//! The parameters mirror the quantities of Source's player movement (see
//! `docs/movement.md`), so the file reads like a set of engine convars, but
//! the values are rustlife's own tuning.

use serde::{Deserialize, Serialize};

use crate::validate::Validator;

/// Every movement parameter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename = "Movement", deny_unknown_fields)]
pub struct MovementConfig {
    /// Downward acceleration, units/s².
    pub gravity: f32,
    /// Upper bound on each velocity component, units/s.
    pub max_velocity: f32,
    /// Movement while standing on a floor.
    pub ground: GroundConfig,
    /// Movement in the air.
    pub air: AirConfig,
    /// Jumping.
    pub jump: JumpConfig,
    /// Collision boxes.
    pub hulls: HullsConfig,
    /// Crouching.
    pub duck: DuckConfig,
    /// Swimming.
    pub water: WaterConfig,
    /// Ladder climbing.
    pub ladder: LadderConfig,
    /// Fastest a player is pushed out of a prop that overlaps them, units/s.
    pub unstick_speed: f32,
}

/// Movement while standing on a floor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroundConfig {
    /// Top running speed, units/s.
    pub run_speed: f32,
    /// Top speed with the walk button held, units/s.
    pub walk_speed: f32,
    /// Acceleration towards the wished velocity, in wished speeds per second.
    pub accelerate: f32,
    /// Fraction of the speed lost to friction per second.
    pub friction: f32,
    /// Friction is computed as if moving at least this fast, units/s.
    pub stop_speed: f32,
    /// Tallest ledge walked up without jumping, units.
    pub step_size: f32,
    /// Minimum Z component of a floor's normal; steeper surfaces are walls.
    pub floor_normal_z: f32,
}

/// Movement in the air.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AirConfig {
    /// Acceleration towards the wished direction, in wished speeds per second.
    pub accelerate: f32,
    /// Air acceleration only adds speed along the wished direction while the
    /// speed along it is below this cap, units/s.
    pub speed_cap: f32,
}

/// Jumping.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JumpConfig {
    /// Height of a standing jump, units; the launch speed follows from it
    /// and gravity.
    pub height: f32,
    /// Whether holding the jump button jumps again on every landing.
    pub auto_hop: bool,
}

impl JumpConfig {
    /// Upward launch speed that reaches [`height`](Self::height) under
    /// `gravity`: `√(2·g·h)`.
    #[must_use]
    pub fn launch_speed(&self, gravity: f32) -> f32 {
        (2.0 * gravity * self.height).sqrt()
    }
}

/// The player's collision boxes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HullsConfig {
    /// Standing upright.
    pub standing: HullConfig,
    /// Fully crouched.
    pub ducked: HullConfig,
}

/// One collision box, relative to the player's origin at the feet.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HullConfig {
    /// Minimum corner; its Z must be 0, since the origin is at the feet.
    pub mins: [f32; 3],
    /// Maximum corner.
    pub maxs: [f32; 3],
    /// Eye height above the origin.
    pub eye_height: f32,
}

/// Crouching.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DuckConfig {
    /// Seconds to crouch fully on the ground.
    pub duck_time: f32,
    /// Seconds to stand back up on the ground.
    pub unduck_time: f32,
    /// Speed multiplier while crouched.
    pub speed_scale: f32,
}

/// Swimming.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WaterConfig {
    /// Acceleration towards the wished velocity, in wished speeds per second.
    pub accelerate: f32,
    /// Fraction of the speed lost per second.
    pub friction: f32,
    /// Speed multiplier while swimming.
    pub speed_scale: f32,
    /// Speed the player drifts down at without input, units/s.
    pub sink_speed: f32,
}

/// Ladder climbing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LadderConfig {
    /// Climbing speed, units/s.
    pub climb_speed: f32,
    /// Farthest horizontal distance from a ladder's axis it can be grabbed
    /// from, units.
    pub mount_distance: f32,
    /// Speed of the push away from the ladder when jumping off, units/s.
    pub jump_off_speed: f32,
    /// Farthest a dismount point may be from the climber for them to step
    /// off onto it, units.
    pub dismount_range: f32,
}

impl MovementConfig {
    /// Checks every value for sense, reporting each problem with its field
    /// path.
    pub(crate) fn validate(&self, check: &mut Validator) {
        check.positive("gravity", self.gravity);
        check.positive("max_velocity", self.max_velocity);

        let ground = &self.ground;
        check.positive("ground.run_speed", ground.run_speed);
        check.positive("ground.walk_speed", ground.walk_speed);
        check.positive("ground.accelerate", ground.accelerate);
        check.non_negative("ground.friction", ground.friction);
        check.non_negative("ground.stop_speed", ground.stop_speed);
        check.non_negative("ground.step_size", ground.step_size);
        check.range("ground.floor_normal_z", ground.floor_normal_z, 0.01, 1.0);

        check.non_negative("air.accelerate", self.air.accelerate);
        check.non_negative("air.speed_cap", self.air.speed_cap);
        check.non_negative("jump.height", self.jump.height);

        let standing = &self.hulls.standing;
        let ducked = &self.hulls.ducked;
        hull(check, "hulls.standing", standing);
        hull(check, "hulls.ducked", ducked);
        if ducked.maxs[2] >= standing.maxs[2] {
            check.fail(
                "hulls.ducked.maxs",
                "the ducked hull must be shorter than the standing hull",
            );
        }

        check.positive("duck.duck_time", self.duck.duck_time);
        check.positive("duck.unduck_time", self.duck.unduck_time);
        check.range("duck.speed_scale", self.duck.speed_scale, 0.0, 1.0);

        check.non_negative("water.accelerate", self.water.accelerate);
        check.non_negative("water.friction", self.water.friction);
        check.range("water.speed_scale", self.water.speed_scale, 0.0, 1.0);
        check.non_negative("water.sink_speed", self.water.sink_speed);

        check.positive("movement.unstick_speed", self.unstick_speed);
        check.positive("ladder.climb_speed", self.ladder.climb_speed);
        check.positive("ladder.mount_distance", self.ladder.mount_distance);
        check.non_negative("ladder.jump_off_speed", self.ladder.jump_off_speed);
        check.non_negative("ladder.dismount_range", self.ladder.dismount_range);
    }
}

/// Checks one hull: a non-empty box with its floor at the origin and the eye
/// inside it.
fn hull(check: &mut Validator, field: &str, hull: &HullConfig) {
    if hull.mins[2] != 0.0 {
        check.fail(
            &format!("{field}.mins"),
            "Z must be 0: the player origin is at the feet",
        );
    }
    if (0..3).any(|axis| hull.mins[axis] >= hull.maxs[axis]) {
        check.fail(field, "mins must be below maxs on every axis");
    }
    check.range(
        &format!("{field}.eye_height"),
        hull.eye_height,
        0.0,
        hull.maxs[2],
    );
}
