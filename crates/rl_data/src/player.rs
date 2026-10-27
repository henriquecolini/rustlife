//! `player.ron`: a player's health, armor and looks, and how their model
//! is animated.

use serde::{Deserialize, Serialize};

use crate::validate::Validator;

/// For serde defaults.
fn one() -> f32 {
    1.0
}

/// Everything about a player that is not movement.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename = "Player", deny_unknown_fields)]
pub struct PlayerConfig {
    /// Health.
    pub health: HealthConfig,
    /// Armor.
    pub armor: ArmorConfig,
    /// What a bullet that hits a player's head deals, as a multiple of its
    /// damage (HL2's `sk_player_head`).
    #[serde(default = "one")]
    pub head_scale: f32,
    /// Models players are drawn with; each player gets one in turn.
    pub models: Vec<String>,
    /// How player models are animated.
    pub animation: PlayerAnimationConfig,
}

/// How a player model's sequences are chosen and paced. Sequences are
/// named by the model's own scheme: `<stance>_<hold>` (`run_smg1`,
/// `cidle_pistol`), the hold coming from the weapon in hand.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlayerAnimationConfig {
    /// Below this speed (units/s) a player stands still.
    pub still_speed: f32,
    /// The speed at which a run plays at its own pace; faster or slower
    /// running speeds it up or slows it down.
    pub run_speed: f32,
    /// The same for crouched walking.
    pub crouch_speed: f32,
    /// Stance prefixes: standing, running, crouched, crouch-walking and in
    /// the air.
    pub idle: String,
    /// See [`PlayerAnimationConfig::idle`].
    pub run: String,
    /// See [`PlayerAnimationConfig::idle`].
    pub crouch: String,
    /// See [`PlayerAnimationConfig::idle`].
    pub crouch_walk: String,
    /// See [`PlayerAnimationConfig::idle`].
    pub jump: String,
    /// The hold of weapons that name none.
    pub default_hold: String,
    /// Seconds over which one sequence fades into the next.
    pub fade: f32,
}

/// Health.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HealthConfig {
    /// Health on spawning.
    pub spawn: f32,
    /// Most health pickups can raise it to.
    pub max: f32,
}

/// Armor: a pool that soaks up part of the damage a player takes.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArmorConfig {
    /// Armor on spawning.
    pub spawn: f32,
    /// Most pickups can raise it to.
    pub max: f32,
    /// Fraction of incoming damage the armor takes instead of health,
    /// while it lasts.
    pub absorb: f32,
    /// Armor used up per point of damage it takes.
    pub cost: f32,
}

impl ArmorConfig {
    /// Splits `damage` between armor and health for a player with `armor`
    /// points: returns `(health_damage, armor_used)`.
    #[must_use]
    pub fn split(&self, damage: f32, armor: f32) -> (f32, f32) {
        if damage <= 0.0 || armor <= 0.0 || self.absorb <= 0.0 {
            return (damage.max(0.0), 0.0);
        }
        let cost = self.cost.max(f32::EPSILON);
        // What the armor would take, limited by what is left of it.
        let absorbed = (damage * self.absorb).min(armor / cost);
        (damage - absorbed, (absorbed * cost).min(armor))
    }
}

impl PlayerConfig {
    pub(crate) fn validate(&self, check: &mut Validator) {
        check.positive("health.spawn", self.health.spawn);
        check.positive("health.max", self.health.max);
        check.non_negative("armor.spawn", self.armor.spawn);
        check.non_negative("armor.max", self.armor.max);
        check.range("armor.absorb", self.armor.absorb, 0.0, 1.0);
        check.positive("armor.cost", self.armor.cost);
        check.positive("head_scale", self.head_scale);
        if self.models.is_empty() {
            check.fail("models", "needs at least one model");
        }
        let animation = &self.animation;
        check.non_negative("animation.still_speed", animation.still_speed);
        check.positive("animation.run_speed", animation.run_speed);
        check.positive("animation.crouch_speed", animation.crouch_speed);
        check.non_negative("animation.fade", animation.fade);
        for (field, prefix) in [
            ("animation.idle", &animation.idle),
            ("animation.run", &animation.run),
            ("animation.crouch", &animation.crouch),
            ("animation.crouch_walk", &animation.crouch_walk),
            ("animation.jump", &animation.jump),
            ("animation.default_hold", &animation.default_hold),
        ] {
            if prefix.trim().is_empty() {
                check.fail(field, "must not be empty");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ARMOR: ArmorConfig = ArmorConfig {
        spawn: 0.0,
        max: 100.0,
        absorb: 0.6,
        cost: 1.0,
    };

    fn assert_split(actual: (f32, f32), expected: (f32, f32)) {
        assert!(
            (actual.0 - expected.0).abs() < 1e-4 && (actual.1 - expected.1).abs() < 1e-4,
            "{actual:?} != {expected:?}"
        );
    }

    #[test]
    fn armor_soaks_its_share_while_it_lasts() {
        assert_split(ARMOR.split(50.0, 100.0), (20.0, 30.0));
        // Only 10 points left: they take 10, health the other 40.
        assert_split(ARMOR.split(50.0, 10.0), (40.0, 10.0));
        assert_split(ARMOR.split(50.0, 0.0), (50.0, 0.0));
    }

    #[test]
    fn costly_armor_runs_out_sooner() {
        let armor = ArmorConfig { cost: 2.0, ..ARMOR };
        // Absorbing 30 would cost 60; 20 points absorb only 10.
        assert_split(armor.split(50.0, 20.0), (40.0, 20.0));
    }
}
