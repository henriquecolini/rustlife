//! `rules/deathmatch.ron`: the rules of a free-for-all deathmatch.
//!
//! Every player for themselves: a kill scores points, more for the harder or
//! showier ways to kill ([`PointsConfig`]), killing yourself costs some, and
//! the match ends at the score or time limit, whichever comes first. The
//! intermission shows the scoreboard while players vote for the next map.
//! Before a match, the game waits for enough players and counts down.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::validate::Validator;

/// Path of the deathmatch rules, relative to the data directory.
pub const DEATHMATCH_FILE: &str = "rules/deathmatch.ron";

/// The rules of the game.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename = "Rules", deny_unknown_fields)]
pub struct RulesConfig {
    /// Points that win the match; 0 for no limit.
    pub score_limit: u32,
    /// Seconds a match lasts; 0 for no limit.
    pub time_limit: f32,
    /// Seconds between the end of a match and the start of the next: the
    /// scoreboard, and the vote for the next map.
    pub intermission: f32,
    /// Players needed before a match starts; until then everyone may move
    /// and shoot, but nobody is hurt and nothing scores.
    pub min_players: u32,
    /// Seconds of countdown once there are enough players.
    pub warmup: f32,
    /// What kills are worth.
    pub points: PointsConfig,
    /// Coming back after dying.
    pub respawn: RespawnConfig,
    /// What players spawn with.
    pub loadout: LoadoutConfig,
    /// Multiplier on damage players deal themselves (their own blasts).
    pub self_damage: f32,
    /// Points lost for killing yourself (falls and props included).
    pub suicide_penalty: u32,
    /// Seconds after a player throws, punts or drops a prop during which
    /// the harm it does is theirs.
    pub prop_credit: f32,
    /// Damage from landing hard.
    pub fall_damage: FallDamageConfig,
}

/// What a kill is worth, by how it was done.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PointsConfig {
    /// A kill with a weapon not listed in `weapons`.
    pub kill: u32,
    /// Kills with particular weapons, by weapon id.
    pub weapons: BTreeMap<String, u32>,
    /// A kill with a thrown, punted or dropped prop.
    pub prop: u32,
    /// A kill by an exploding barrel.
    pub barrel: u32,
    /// A kill by pushing someone to their fall or into a hazard.
    pub environment: u32,
}

impl PointsConfig {
    /// The points for a kill with `weapon` (a weapon id).
    #[must_use]
    pub fn for_weapon(&self, weapon: &str) -> u32 {
        self.weapons.get(weapon).copied().unwrap_or(self.kill)
    }
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

/// Damage from landing hard: none up to a safe speed, a fixed amount past it
/// (HL2DM's `mp_falldamage 0`: `FlPlayerFallDamage` returns 10).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FallDamageConfig {
    /// Fastest landing that does no harm, units/s.
    pub safe_speed: f32,
    /// Damage for landing faster than that.
    pub damage: f32,
}

impl FallDamageConfig {
    /// The damage for landing at `speed` units/s.
    #[must_use]
    pub fn damage(&self, speed: f32) -> f32 {
        if speed > self.safe_speed {
            self.damage
        } else {
            0.0
        }
    }
}

impl RulesConfig {
    pub(crate) fn validate(&self, check: &mut Validator) {
        check.non_negative("time_limit", self.time_limit);
        check.non_negative("intermission", self.intermission);
        check.non_negative("warmup", self.warmup);
        if self.min_players == 0 {
            check.fail("min_players", "must be at least 1");
        }
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
        check.non_negative("fall_damage.damage", fall.damage);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fall_damage_is_fixed_past_the_safe_speed() {
        let fall = FallDamageConfig {
            safe_speed: 580.0,
            damage: 10.0,
        };
        assert_eq!(fall.damage(500.0), 0.0);
        assert_eq!(fall.damage(580.0), 0.0);
        assert_eq!(fall.damage(581.0), 10.0);
        assert_eq!(fall.damage(1500.0), 10.0);
    }
}
