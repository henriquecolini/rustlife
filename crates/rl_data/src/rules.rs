//! `rules/deathmatch.ron`: the rules of a free-for-all deathmatch.
//!
//! Every player for themselves: a kill scores a frag, killing yourself costs
//! one, and the match ends at the frag or time limit, whichever comes
//! first. After a short intermission the next match starts.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::validate::Validator;

/// Path of the deathmatch rules, relative to the data directory.
pub const DEATHMATCH_FILE: &str = "rules/deathmatch.ron";

/// The rules of the game.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename = "Rules", deny_unknown_fields)]
pub struct RulesConfig {
    /// Frags that win the match; 0 for no limit.
    pub frag_limit: u32,
    /// Seconds a match lasts; 0 for no limit.
    pub time_limit: f32,
    /// Seconds between the end of a match and the start of the next.
    pub intermission: f32,
    /// Coming back after dying.
    pub respawn: RespawnConfig,
    /// What players spawn with.
    pub loadout: LoadoutConfig,
    /// Multiplier on damage players deal themselves (their own blasts).
    pub self_damage: f32,
    /// Frags lost for killing yourself (falls and props included).
    pub suicide_penalty: u32,
    /// Seconds after a player throws, punts or drops a prop during which
    /// the harm it does is theirs.
    pub prop_credit: f32,
    /// Damage from landing hard.
    pub fall_damage: FallDamageConfig,
}

/// Coming back after dying.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RespawnConfig {
    /// Seconds after death before a player may respawn (by pressing fire
    /// or jump).
    pub delay: f32,
    /// Seconds after death when a player respawns without pressing
    /// anything.
    pub force: f32,
    /// Spawn points are ranked by how far they are from the nearest other
    /// living player; one of this many best is picked at random.
    pub candidates: u32,
}

/// What players spawn with.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoadoutConfig {
    /// Weapons carried, by id; each starts with a full clip.
    pub weapons: Vec<String>,
    /// The weapon in hand.
    pub active: String,
    /// Carried ammo by type.
    pub ammo: BTreeMap<String, u32>,
}

/// Damage from landing hard: none up to a safe speed, rising linearly to
/// `fatal_damage` at `fatal_speed` (and beyond it, in proportion).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FallDamageConfig {
    /// Fastest landing that does no harm, units/s.
    pub safe_speed: f32,
    /// Landing speed that deals `fatal_damage`, units/s.
    pub fatal_speed: f32,
    /// Damage at `fatal_speed`.
    pub fatal_damage: f32,
}

impl FallDamageConfig {
    /// The damage for landing at `speed` units/s.
    #[must_use]
    pub fn damage(&self, speed: f32) -> f32 {
        let span = self.fatal_speed - self.safe_speed;
        if speed <= self.safe_speed || span <= 0.0 {
            return 0.0;
        }
        (speed - self.safe_speed) / span * self.fatal_damage
    }
}

impl RulesConfig {
    pub(crate) fn validate(&self, check: &mut Validator) {
        check.non_negative("time_limit", self.time_limit);
        check.non_negative("intermission", self.intermission);
        check.non_negative("respawn.delay", self.respawn.delay);
        if self.respawn.force < self.respawn.delay {
            check.fail("respawn.force", "must not come before respawn.delay");
        }
        if self.respawn.candidates == 0 {
            check.fail("respawn.candidates", "must be at least 1");
        }
        if !self.loadout.weapons.contains(&self.loadout.active) {
            check.fail("loadout.active", "must be one of loadout.weapons");
        }
        check.non_negative("self_damage", self.self_damage);
        check.non_negative("prop_credit", self.prop_credit);
        let fall = &self.fall_damage;
        check.non_negative("fall_damage.safe_speed", fall.safe_speed);
        if fall.fatal_speed <= fall.safe_speed {
            check.fail("fall_damage.fatal_speed", "must exceed safe_speed");
        }
        check.non_negative("fall_damage.fatal_damage", fall.fatal_damage);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fall_damage_rises_linearly_past_the_safe_speed() {
        let fall = FallDamageConfig {
            safe_speed: 600.0,
            fatal_speed: 1000.0,
            fatal_damage: 100.0,
        };
        assert_eq!(fall.damage(500.0), 0.0);
        assert_eq!(fall.damage(800.0), 50.0);
        assert_eq!(fall.damage(1200.0), 150.0);
    }
}
