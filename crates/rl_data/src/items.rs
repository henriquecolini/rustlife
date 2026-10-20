//! `items/<id>.ron`: pickups that are not weapons — health, armor and ammo.
//!
//! An item sits where the map places it (see `entity_map.ron`), is taken by
//! the first player to touch it who can use it, and comes back after a
//! while. Weapons are picked up too, but describe their pickup in their own
//! file ([`crate::WeaponPickupConfig`]).

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
    /// What it gives.
    pub gives: ItemEffect,
    /// Seconds before it comes back once taken.
    pub respawn: f32,
    /// A player touches it when it is within this distance of their box,
    /// units.
    pub radius: f32,
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
    }
}
