//! `tools/manipulator.ron`: the Manipulator, rustlife's grab-and-launch
//! physics tool.
//!
//! Secondary fire grabs the prop under the crosshair (or pulls a distant one
//! closer); the held prop floats in front of the player, following the view;
//! primary fire launches it, or punts a prop that is not held. The tool
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
    pub punt: LaunchConfig,
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
    /// Speed props are pulled at, units/s: they speed up towards it, so a
    /// far prop starts off slowly...
    pub pull_speed: f32,
    /// ...gaining this much speed a second.
    pub pull_accel: f32,
}

/// Carrying a held prop.
///
/// The prop is steered, not teleported: each tick its velocity is set to
/// close the gap to the hold point over [`follow_time`](Self::follow_time),
/// capped at [`max_speed`](Self::max_speed). Its collisions stay active, so
/// a held prop pushes others aside and cannot pass through walls.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HoldConfig {
    /// Gap between the player's eyes and the near side of the held prop,
    /// units: the prop's centre is held this far plus its own radius ahead.
    pub distance: f32,
    /// Seconds the prop takes to close (most of) the gap to the hold point.
    pub follow_time: f32,
    /// Fastest the prop is moved, units/s.
    pub max_speed: f32,
    /// Fastest the prop is turned to follow the view, degrees/s.
    pub max_angular_speed: f32,
    /// The grip breaks when the prop is held back this far from the hold
    /// point, units...
    pub break_distance: f32,
    /// ...for this long, seconds.
    pub break_time: f32,
}

/// Throwing a prop: its speed falls off with its mass.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchConfig {
    /// Farthest a prop can be punted from, units from the eyes; unused for
    /// launching the held prop.
    pub range: f32,
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
        check.non_negative("grab.pull_speed", grab.pull_speed);
        check.non_negative("grab.pull_accel", grab.pull_accel);

        let hold = &self.hold;
        check.non_negative("hold.distance", hold.distance);
        check.positive("hold.follow_time", hold.follow_time);
        check.positive("hold.max_speed", hold.max_speed);
        check.positive("hold.max_angular_speed", hold.max_angular_speed);
        check.positive("hold.break_distance", hold.break_distance);
        check.non_negative("hold.break_time", hold.break_time);

        launch(check, "launch", &self.launch);
        launch(check, "punt", &self.punt);
        check.positive("punt.range", self.punt.range);
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
            range: 0.0,
            speed: 1000.0,
            mass_curve: vec![(0.0, 1.0), (100.0, 0.5)],
        };
        assert_eq!(launch.speed_for(50.0), 750.0);
    }

    #[test]
    fn unsorted_curves_are_rejected() {
        let mut check = Validator::default();
        launch(
            &mut check,
            "launch",
            &LaunchConfig {
                range: 0.0,
                speed: 1.0,
                mass_curve: vec![(5.0, 1.0), (5.0, 0.5)],
            },
        );
        let problems = check.finish();
        assert_eq!(problems.len(), 1);
        assert_eq!(problems[0].field, "launch.mass_curve");
    }
}
