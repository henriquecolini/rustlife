//! `ammo.ron`: the kinds of ammunition players carry.
//!
//! Weapons, pickups and the spawn loadout refer to ammo types by the keys of
//! this file; each type caps how much of it a player can carry.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::validate::Validator;

/// Every ammo type, by id.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename = "Ammo", deny_unknown_fields)]
pub struct AmmoConfig {
    /// The types, by id.
    pub types: BTreeMap<String, AmmoType>,
}

/// One ammo type.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AmmoType {
    /// Name shown on the HUD.
    pub name: String,
    /// Most rounds a player can carry (not counting loaded clips).
    pub max: u32,
}

impl AmmoConfig {
    /// Whether `id` is a known ammo type.
    #[must_use]
    pub fn contains(&self, id: &str) -> bool {
        self.types.contains_key(id)
    }

    pub(crate) fn validate(&self, check: &mut Validator) {
        if self.types.is_empty() {
            check.fail("types", "needs at least one ammo type");
        }
        for (id, ammo) in &self.types {
            if ammo.max == 0 {
                check.fail(&format!("types.{id}.max"), "must be at least 1");
            }
            if ammo.name.trim().is_empty() {
                check.fail(&format!("types.{id}.name"), "must not be empty");
            }
        }
    }
}
