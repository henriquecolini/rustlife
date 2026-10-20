//! `map_logic.ron`: how the map's own logic plays — doors, buttons,
//! triggers and traps.
//!
//! Maps wire their entities together with outputs and inputs; what each
//! entity does is the map's. This file holds the few numbers the map
//! cannot give: how far players reach to use things, how hard the map's
//! explosions throw players and props, and the most outputs a tick fires.

use serde::{Deserialize, Serialize};

use crate::validate::Validator;

/// Tuning of the map's logic.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename = "MapLogic", deny_unknown_fields)]
pub struct MapLogicConfig {
    /// How far from the eyes a player reaches to press a button or open a
    /// door with use, units.
    pub use_range: f32,
    /// How far off the view, degrees, a station (a wall charger) may be
    /// and still be used.
    pub use_cone: f32,
    /// The map's explosions (`env_explosion`).
    pub explosion: MapExplosionConfig,
    /// Most outputs fired in one tick: a map whose relays trigger each
    /// other in a loop stops there for the tick instead of hanging.
    pub max_events_per_tick: u32,
}

/// How the map's explosions throw what they reach; their damage and reach
/// are the map's.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MapExplosionConfig {
    /// Reach of an explosion whose entity gives none, as a multiple of its
    /// magnitude (its damage at the centre).
    pub radius_per_damage: f32,
    /// Speed given to a player at the centre per point of damage, units/s.
    pub knockback_per_damage: f32,
    /// Impulse given to a prop at the centre per point of damage,
    /// kg·units/s.
    pub force_per_damage: f32,
}

impl MapLogicConfig {
    pub(crate) fn validate(&self, check: &mut Validator) {
        check.positive("use_range", self.use_range);
        check.range("use_cone", self.use_cone, 0.0, 90.0);
        check.positive(
            "explosion.radius_per_damage",
            self.explosion.radius_per_damage,
        );
        check.non_negative(
            "explosion.knockback_per_damage",
            self.explosion.knockback_per_damage,
        );
        check.non_negative(
            "explosion.force_per_damage",
            self.explosion.force_per_damage,
        );
        if self.max_events_per_tick == 0 {
            check.fail("max_events_per_tick", "must be at least 1");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> MapLogicConfig {
        MapLogicConfig {
            use_range: 80.0,
            use_cone: 30.0,
            explosion: MapExplosionConfig {
                radius_per_damage: 2.5,
                knockback_per_damage: 7.0,
                force_per_damage: 700.0,
            },
            max_events_per_tick: 1024,
        }
    }

    #[test]
    fn sensible_values_pass() {
        let mut check = Validator::default();
        config().validate(&mut check);
        assert!(check.finish().is_empty());
    }

    #[test]
    fn every_bad_value_is_reported() {
        let mut check = Validator::default();
        MapLogicConfig {
            use_range: 0.0,
            use_cone: 120.0,
            explosion: MapExplosionConfig {
                radius_per_damage: -1.0,
                knockback_per_damage: -1.0,
                force_per_damage: -1.0,
            },
            max_events_per_tick: 0,
        }
        .validate(&mut check);
        assert_eq!(check.finish().len(), 6);
    }
}
