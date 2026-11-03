//! `tools/hands.ron`: carrying a light prop in the hands with the use key.
//!
//! Use on a prop light and small enough picks it up and puts the weapon away;
//! the prop is held in front, turning with the view; primary fire tosses it,
//! secondary fire or use again sets it down, and the weapon comes back. This is
//! HL2's `CPlayerPickupController`, a much weaker Gravity Gun that needs no
//! weapon. The carrying itself is `rl_sim::tools::hands`; this file holds its
//! numbers, each one HL2DM's (read from its `server.so`).

use serde::{Deserialize, Serialize};

use crate::validate::Validator;

/// Every number of carrying props in the hands.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename = "Hands", deny_unknown_fields)]
pub struct HandsConfig {
    /// What can be picked up.
    pub pickup: PickupLimits,
    /// Carrying it.
    pub hold: CarryHold,
    /// How fast a prop set down (or tossed, before the toss) may still be
    /// moving.
    pub release: ReleaseLimits,
    /// Tossing it.
    pub toss: Toss,
}

/// What the hands can pick up (`CBasePlayer::CanPickupObject`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PickupLimits {
    /// Heaviest prop that can be picked up, kg.
    pub max_mass: f32,
    /// Longest a prop may be along any of its own axes, units.
    pub max_size: f32,
}

/// Carrying a prop (`CGrabController::UpdateObject`).
///
/// The prop's centre of mass is steered to a point ahead of the eyes along
/// the view, its pitch clamped to [`max_pitch`](Self::max_pitch). How far
/// ahead depends on the prop's depth towards the player and the player's
/// width (`radius` below: half the player box's diagonal plus how far the
/// prop reaches back from its origin): [`gap`](Self::gap) + `radius`, or half
/// of `radius` when a wall is within half of `gap` + 2 × `radius`; and never
/// closer than `radius` to the player's upright axis.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CarryHold {
    /// The base of how far ahead the prop is held, units.
    pub gap: f32,
    /// The view's pitch is clamped to this, up and down, for placing the prop,
    /// degrees: looking straight down holds it in front of the feet.
    pub max_pitch: f32,
    /// Seconds the prop takes to close the gap to where it is held: HL2
    /// steers it there in one tick.
    pub follow_time: f32,
    /// Fastest the prop is moved, units/s...
    pub max_speed: f32,
    /// ...and turned to follow the view, degrees/s.
    pub max_angular_speed: f32,
    /// The prop is dropped when it has been held back from where it should be
    /// by this much, units. The distance is smoothed over about a second, and
    /// halved while it is greater than [`max_speed`](Self::max_speed) could
    /// close in a tick (the view swinging round, not something in the way).
    pub drop_error: f32,
    /// Seconds after the pickup before being held back counts.
    pub error_grace: f32,
}

/// How fast a prop let go of may move on: a faster one is slowed down to
/// this (`ClampPhysicsVelocity` in `CGrabController::DetachEntity`). A prop
/// set down touching something stops dead instead.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseLimits {
    /// Fastest, units/s: HL2DM's `hl2_normspeed` (190) times 1.5.
    pub max_speed: f32,
    /// Fastest spin, degrees/s.
    pub max_angular_speed: f32,
}

/// Tossing the held prop with primary fire (`CPlayerPickupController::Use`).
///
/// The prop gets an impulse along the view of [`force`](Self::force) times a
/// factor that grows with its mass, from the first of
/// [`factors`](Self::factors) at the first of [`masses`](Self::masses) to the
/// second at the second (clamped at both ends), so light props fly fast and
/// heavy ones go no faster than a few hundred units/s. A small random spin
/// goes with it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Toss {
    /// The impulse before the mass factor, kg·units/s (`player_throwforce`).
    pub force: f32,
    /// The masses the factor ramps between, kg.
    pub masses: (f32, f32),
    /// The factor at each of them.
    pub factors: (f32, f32),
    /// The spin's angular impulse is up to this times the mass factor about
    /// each axis, in Source's units (degrees, kg and metres).
    pub spin: f32,
}

impl Toss {
    /// The mass factor of a prop of `mass` kg.
    #[must_use]
    pub fn factor(&self, mass: f32) -> f32 {
        let (light, heavy) = self.masses;
        let (low, high) = self.factors;
        let span = heavy - light;
        if span <= 0.0 {
            return high;
        }
        let along = (mass.clamp(light, heavy) - light) / span;
        low + (high - low) * along
    }

    /// The impulse a toss gives a prop of `mass` kg, kg·units/s.
    #[must_use]
    pub fn impulse(&self, mass: f32) -> f32 {
        self.force * self.factor(mass)
    }
}

impl HandsConfig {
    /// Checks every value for sense, reporting each problem with its field
    /// path.
    pub(crate) fn validate(&self, check: &mut Validator) {
        check.positive("pickup.max_mass", self.pickup.max_mass);
        check.positive("pickup.max_size", self.pickup.max_size);

        let hold = &self.hold;
        check.non_negative("hold.gap", hold.gap);
        check.range("hold.max_pitch", hold.max_pitch, 0.0, 90.0);
        check.positive("hold.follow_time", hold.follow_time);
        check.positive("hold.max_speed", hold.max_speed);
        check.positive("hold.max_angular_speed", hold.max_angular_speed);
        check.positive("hold.drop_error", hold.drop_error);
        check.non_negative("hold.error_grace", hold.error_grace);

        check.positive("release.max_speed", self.release.max_speed);
        check.positive("release.max_angular_speed", self.release.max_angular_speed);

        let toss = &self.toss;
        check.non_negative("toss.force", toss.force);
        check.positive("toss.masses", toss.masses.0);
        if toss.masses.1 <= toss.masses.0 {
            check.fail("toss.masses", "the second mass must be the greater");
        }
        check.non_negative("toss.factors", toss.factors.0);
        check.non_negative("toss.factors", toss.factors.1);
        check.non_negative("toss.spin", toss.spin);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toss() -> Toss {
        Toss {
            force: 1000.0,
            masses: (0.5, 15.0),
            factors: (0.5, 4.0),
            spin: 10.0,
        }
    }

    #[test]
    fn the_toss_factor_ramps_with_mass_and_holds_at_the_ends() {
        let toss = toss();
        assert_eq!(toss.factor(0.1), 0.5);
        assert_eq!(toss.factor(0.5), 0.5);
        assert!((toss.factor(7.75) - 2.25).abs() < 1e-5);
        assert_eq!(toss.factor(15.0), 4.0);
        assert_eq!(toss.factor(35.0), 4.0);
    }

    #[test]
    fn light_props_fly_faster_than_heavy_ones() {
        let toss = toss();
        let speed = |mass: f32| toss.impulse(mass) / mass;
        // HL2DM: a 1 kg can at about 620 units/s, a 35 kg drum at about 114.
        assert!((speed(1.0) - 620.69).abs() < 0.1, "{}", speed(1.0));
        assert!((speed(35.0) - 114.29).abs() < 0.1, "{}", speed(35.0));
        assert!(speed(1.0) > speed(5.0) && speed(5.0) > speed(15.0));
    }

    #[test]
    fn masses_out_of_order_are_rejected() {
        let mut config = crate::GameData::builtin().expect("builtin").hands;
        config.toss.masses = (15.0, 15.0);
        let mut check = Validator::default();
        config.validate(&mut check);
        let problems = check.finish();
        assert_eq!(problems.len(), 1);
        assert_eq!(problems[0].field, "toss.masses");
    }
}
