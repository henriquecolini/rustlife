//! `items/<id>.ron`: pickups that are not weapons — health, armor and ammo.
//!
//! An item sits where the map places it (see `entity_map.ron`), is taken by
//! the first player to touch it who can use it, and comes back after a
//! while. Weapons are picked up too, but describe their pickup in their own
//! file ([`crate::WeaponPickupConfig`]).
//!
//! An item with a `station` is not picked up but used, as wall chargers
//! are: a player holding use at it draws its health or armor a little at a
//! time, until its charge is spent; it then recharges.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::validate::Validator;

/// One item.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename = "Item", deny_unknown_fields)]
pub struct ItemConfig {
    /// Name shown when it is picked up.
    pub name: String,
    /// Model drawn for it.
    pub model: String,
    /// Degrees added to the yaw the map places it at, for models that do not
    /// face along their entity's forward axis (the wall chargers' backs face
    /// it).
    #[serde(default)]
    pub model_yaw: f32,
    /// What it gives.
    pub gives: ItemEffect,
    /// Seconds before it comes back once taken.
    pub respawn: f32,
    /// A player touches it when it is within this distance of their box,
    /// units.
    pub radius: f32,
    /// Used rather than picked up, if set: it dispenses its `gives` while
    /// held at, and `respawn` is how long it takes to recharge once spent.
    #[serde(default)]
    pub station: Option<StationConfig>,
    /// Soundscript names (or files under `sound/`) by purpose: `pickup`
    /// when taken, `use` while a station dispenses.
    #[serde(default)]
    pub sounds: BTreeMap<String, String>,
}

/// What an item's sounds are for.
pub const ITEM_SOUNDS: [&str; 2] = ["pickup", "use"];

/// How a station dispenses.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StationConfig {
    /// Points of health or armor given per second of use.
    pub rate: f32,
}

/// What an item gives.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum ItemEffect {
    /// Health, up to the player's maximum.
    Health(f32),
    /// Armor, up to the player's maximum.
    Armor(f32),
    /// Rounds of each listed ammo type, up to each type's maximum.
    Ammo(BTreeMap<String, u32>),
}

impl ItemConfig {
    pub(crate) fn validate(&self, check: &mut Validator) {
        if self.name.trim().is_empty() {
            check.fail("name", "must not be empty");
        }
        if self.model.trim().is_empty() {
            check.fail("model", "must name a model");
        }
        match &self.gives {
            ItemEffect::Health(amount) | ItemEffect::Armor(amount) => {
                check.positive("gives", *amount);
            }
            ItemEffect::Ammo(rounds) => {
                if rounds.is_empty() || rounds.values().any(|&count| count == 0) {
                    check.fail("gives", "must give some rounds of at least one type");
                }
            }
        }
        check.non_negative("respawn", self.respawn);
        check.non_negative("radius", self.radius);
        for purpose in self.sounds.keys() {
            if !ITEM_SOUNDS.contains(&purpose.as_str()) {
                check.fail(
                    &format!("sounds.{purpose}"),
                    &format!("unknown purpose; known: {}", ITEM_SOUNDS.join(", ")),
                );
            }
        }
        if let Some(station) = &self.station {
            check.positive("station.rate", station.rate);
            if matches!(self.gives, ItemEffect::Ammo(_)) {
                check.fail("station", "a station dispenses health or armor, not ammo");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn station(gives: ItemEffect, rate: f32) -> ItemConfig {
        ItemConfig {
            name: "Station".to_owned(),
            model: "models/station.mdl".to_owned(),
            model_yaw: 0.0,
            gives,
            respawn: 60.0,
            radius: 40.0,
            station: Some(StationConfig { rate }),
            sounds: BTreeMap::from([(
                "use".to_owned(),
                "WallHealth.LoopingContinueCharge".to_owned(),
            )]),
        }
    }

    #[test]
    fn stations_dispense_health_or_armor_at_a_positive_rate() {
        let mut check = Validator::default();
        station(ItemEffect::Health(50.0), 25.0).validate(&mut check);
        assert!(check.finish().is_empty());

        let mut check = Validator::default();
        station(
            ItemEffect::Ammo(BTreeMap::from([("smg".to_owned(), 10)])),
            0.0,
        )
        .validate(&mut check);
        let fields: Vec<String> = check.finish().into_iter().map(|p| p.field).collect();
        assert_eq!(fields, ["station.rate", "station"]);
    }
}
