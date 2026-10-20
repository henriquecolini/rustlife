//! `entity_map.ron`: what the entities a map places become in rustlife.
//!
//! Maps made for Source games place their pickups as entities of that game's
//! classes (`weapon_shotgun`, `item_healthkit`, ...). rustlife does not
//! implement those classes; this file turns each into one of rustlife's own
//! weapons or items, so stock and community maps come stocked with
//! rustlife's arsenal. Classnames not listed (and not otherwise understood,
//! like spawn points and props) are ignored.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::validate::Validator;

/// Map entity classes and what they become.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename = "EntityMap", deny_unknown_fields)]
pub struct EntityMapConfig {
    /// What each classname becomes, by lower-case classname.
    pub classes: BTreeMap<String, MappedEntity>,
}

/// What a map entity becomes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum MappedEntity {
    /// A pickup of the weapon with this id.
    Weapon(String),
    /// A pickup of the item with this id.
    Item(String),
    /// Nothing: the entity is deliberately left out.
    Ignore,
}

impl EntityMapConfig {
    /// What entities of `classname` become, if listed. Case is ignored.
    #[must_use]
    pub fn get(&self, classname: &str) -> Option<&MappedEntity> {
        self.classes.get(&classname.to_ascii_lowercase())
    }

    pub(crate) fn validate(&self, check: &mut Validator) {
        for classname in self.classes.keys() {
            if classname.is_empty() || classname.chars().any(|c| c.is_ascii_uppercase()) {
                check.fail(
                    &format!("classes.{classname}"),
                    "classnames must be non-empty and lower case",
                );
            }
        }
    }
}
