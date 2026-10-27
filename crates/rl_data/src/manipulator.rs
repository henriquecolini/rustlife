//! `tools/manipulator.ron`: the Manipulator, rustlife's grab-and-launch
//! physics tool.
//!
//! Secondary fire grabs the prop under the crosshair (or pulls a distant one
//! closer); the held prop floats in front of the player, following the view;
//! primary fire launches it, or punts a prop that is not held. The numbers
//! come from HL2DM's `server.so` (see `docs/physics.md`). The tool
//! itself is `rl_sim::tools::manipulator`; this file holds its tuning. How
//! often it acts is set like any weapon's rate of fire, in
//! `weapons/manipulator.ron`.

use serde::{Deserialize, Serialize};

use crate::validate::Validator;

/// Every Manipulator parameter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename = "Manipulator", deny_unknown_fields)]
pub struct ManipulatorConfig {
    /// Picking props up.
    pub grab: GrabConfig,
    /// Carrying a held prop.
    pub hold: HoldConfig,
    /// Throwing the held prop.
    pub launch: LaunchConfig,
    /// Knocking away a prop that is not held.
    pub punt: PuntConfig,
}

/// Picking props up.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrabConfig {
    /// Farthest a prop can be grabbed from, units from the eyes.
    pub range: f32,
    /// Heaviest prop that can be grabbed, kg.
    pub max_mass: f32,
    /// Props beyond `range` but within this distance are pulled towards
    /// the player while secondary fire is held, units.
    pub pull_range: f32,
    /// With nothing under the crosshair, the prop within `range` closest to
    /// the view direction is grabbed if the cosine of the angle to it is at
    /// least this.
    pub cone: f32,
    /// The impulse a pull gives a prop every reference tick, kg·units/s.
    pub pull_impulse: f32,
    /// Props lighter than this are pulled with their impulse scaled by
    /// `(mass + 0.5) * pull_light_gain`, kg...
    pub pull_light_mass: f32,
    /// ...with this gain.
    pub pull_light_gain: f32,
    /// The tick length the pull impulse is given for, seconds: a pull
    /// gives `pull_impulse` per this long, however often the world ticks.
    pub reference_tick: f32,
    /// Seconds secondary fire rests after grabbing (to drop the prop again
    /// it must be pressed afresh anyway).
    pub grab_rest: f32,
}

/// Carrying a held prop.
///
/// The prop is steered, not teleported: each tick its velocity is set to
/// close the gap to the hold point over [`follow_time`](Self::follow_time),
/// capped at [`max_speed`](Self::max_speed). Its collisions stay active, so
/// a held prop pushes others aside and cannot pass through walls.
///
/// The hold point lies `clearance + 2 * reach` ahead of the eyes, less the
/// reach (so `clearance + reach`), where the reach is the player's radius
/// plus how far the prop extends along the view. An obstacle nearer than
/// half the first distance pulls the point in to half the reach.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HoldConfig {
    /// Slack added to the hold distance, units.
    pub clearance: f32,
    /// The player's radius in the horizontal plane, units: how far the
    /// prop is kept from the player's vertical axis.
    pub player_radius: f32,
    /// The view pitch the hold point follows is clamped to this many
    /// degrees up and down.
    pub pitch_limit: f32,
    /// Seconds the prop takes to close (most of) the gap to the hold point.
    pub follow_time: f32,
    /// Fastest the prop is moved, units/s.
    pub max_speed: f32,
    /// Fastest the prop is turned to follow the view, degrees/s.
    pub max_angular_speed: f32,
    /// The grip breaks when the prop's distance from the hold point,
    /// averaged over about a second (each tick takes the tick's length in
    /// seconds of the new distance), exceeds this, units.
    pub break_distance: f32,
}

/// Throwing the held prop: its speed falls off with its mass.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchConfig {
    /// Speed given to a weightless prop, units/s.
    pub speed: f32,
    /// Multiplier on [`speed`](Self::speed) by mass: `(kg, factor)` points,
    /// sorted by mass, interpolated linearly and held flat past the ends.
    pub mass_curve: Vec<(f32, f32)>,
}

impl LaunchConfig {
    /// The speed given to a prop of `mass` kg.
    #[must_use]
    pub fn speed_for(&self, mass: f32) -> f32 {
        self.speed * curve(&self.mass_curve, mass)
    }
}

/// Knocking away a prop that is not held.
///
/// The prop gets an impulse through its centre of mass of `force` and one
/// at the point hit of `offset_force` per kg of its mass (counted up to
/// `offset_mass_cap`), both along the view; so a light prop flies off at
/// `force / mass` and a heavy one at about `offset_force` plus that.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PuntConfig {
    /// Farthest a prop can be punted from, units from the eyes.
    pub range: f32,
    /// The impulse through the centre of mass, kg·units/s.
    pub force: f32,
    /// The impulse at the point hit, per kg, kg·units/s each.
    pub offset_force: f32,
    /// Props heavier than this count as this much for `offset_force`, kg.
    pub offset_mass_cap: f32,
    /// A punt aimed downwards has its downward part scaled by this
    /// (negative: it lifts instead).
    pub downward_scale: f32,
}

impl PuntConfig {
    /// The impulses a punt gives a prop of `mass` kg, as multiples of the
    /// punt's direction: through its centre, and at the point hit.
    #[must_use]
    pub fn impulses(&self, mass: f32) -> (f32, f32) {
        (
            self.force,
            self.offset_force * mass.min(self.offset_mass_cap),
        )
    }

    /// The vertical part of the punt's direction when the view's is `z`:
    /// a downward aim lifts instead, by `downward_scale`.
    #[must_use]
    pub fn direction_z(&self, z: f32) -> f32 {
        if z < 0.0 { z * self.downward_scale } else { z }
    }
}

/// Piecewise-linear interpolation through `points` (sorted by x), flat past
/// the ends; 1 for an empty curve.
fn curve(points: &[(f32, f32)], x: f32) -> f32 {
    let (Some(&(first_x, first_y)), Some(&(last_x, last_y))) = (points.first(), points.last())
    else {
        return 1.0;
    };
    if x <= first_x {
        return first_y;
    }
    if x >= last_x {
        return last_y;
    }
    points
        .windows(2)
        .find(|pair| x <= pair[1].0)
        .map_or(last_y, |pair| {
            let ((x0, y0), (x1, y1)) = (pair[0], pair[1]);
            let span = x1 - x0;
            if span <= 0.0 {
                y1
            } else {
                y0 + (y1 - y0) * (x - x0) / span
            }
        })
}

impl ManipulatorConfig {
    /// Checks every value for sense, reporting each problem with its field
    /// path.
    pub(crate) fn validate(&self, check: &mut Validator) {
        let grab = &self.grab;
        check.positive("grab.range", grab.range);
        check.positive("grab.max_mass", grab.max_mass);
        check.non_negative("grab.pull_range", grab.pull_range);
        if !(0.0..=1.0).contains(&grab.cone) {
            check.fail("grab.cone", "must be a cosine between 0 and 1");
        }
        check.non_negative("grab.pull_impulse", grab.pull_impulse);
        check.non_negative("grab.pull_light_mass", grab.pull_light_mass);
        check.non_negative("grab.pull_light_gain", grab.pull_light_gain);
        check.positive("grab.reference_tick", grab.reference_tick);
        check.non_negative("grab.grab_rest", grab.grab_rest);

        let hold = &self.hold;
        check.non_negative("hold.clearance", hold.clearance);
        check.non_negative("hold.player_radius", hold.player_radius);
        check.positive("hold.pitch_limit", hold.pitch_limit);
        check.positive("hold.follow_time", hold.follow_time);
        check.positive("hold.max_speed", hold.max_speed);
        check.positive("hold.max_angular_speed", hold.max_angular_speed);
        check.positive("hold.break_distance", hold.break_distance);

        launch(check, "launch", &self.launch);
        let punt = &self.punt;
        check.positive("punt.range", punt.range);
        check.non_negative("punt.force", punt.force);
        check.non_negative("punt.offset_force", punt.offset_force);
        check.non_negative("punt.offset_mass_cap", punt.offset_mass_cap);
    }
}

fn launch(check: &mut Validator, field: &str, launch: &LaunchConfig) {
    check.non_negative(&format!("{field}.speed"), launch.speed);
    let curve_field = format!("{field}.mass_curve");
    if launch.mass_curve.is_empty() {
        check.fail(&curve_field, "needs at least one point");
    }
    for &(mass, factor) in &launch.mass_curve {
        check.non_negative(&curve_field, mass);
        check.non_negative(&curve_field, factor);
    }
    if launch
        .mass_curve
        .windows(2)
        .any(|pair| pair[1].0 <= pair[0].0)
    {
        check.fail(&curve_field, "points must be sorted by increasing mass");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curves_interpolate_and_hold_flat_past_the_ends() {
        let points = [(10.0, 1.0), (20.0, 0.5), (40.0, 0.0)];
        assert_eq!(curve(&points, 0.0), 1.0);
        assert_eq!(curve(&points, 15.0), 0.75);
        assert_eq!(curve(&points, 30.0), 0.25);
        assert_eq!(curve(&points, 100.0), 0.0);
        assert_eq!(curve(&[], 5.0), 1.0);
        assert_eq!(curve(&[(1.0, 0.3)], 5.0), 0.3);
    }

    #[test]
    fn launch_speed_scales_with_the_curve() {
        let launch = LaunchConfig {
            speed: 1000.0,
            mass_curve: vec![(0.0, 1.0), (100.0, 0.5)],
        };
        assert_eq!(launch.speed_for(50.0), 750.0);
    }

    #[test]
    fn punts_add_a_mass_proportional_push_up_to_the_cap() {
        let punt = PuntConfig {
            range: 250.0,
            force: 15000.0,
            offset_force: 600.0,
            offset_mass_cap: 250.0,
            downward_scale: -0.65,
        };
        assert_eq!(punt.impulses(10.0), (15000.0, 6000.0));
        assert_eq!(punt.impulses(1000.0), (15000.0, 150_000.0));
        assert_eq!(punt.direction_z(0.5), 0.5);
        assert!((punt.direction_z(-1.0) - 0.65).abs() < 1e-6);
    }

    #[test]
    fn unsorted_curves_are_rejected() {
        let mut check = Validator::default();
        launch(
            &mut check,
            "launch",
            &LaunchConfig {
                speed: 1.0,
                mass_curve: vec![(5.0, 1.0), (5.0, 0.5)],
            },
        );
        let problems = check.finish();
        assert_eq!(problems.len(), 1);
        assert_eq!(problems[0].field, "launch.mass_curve");
    }
}
