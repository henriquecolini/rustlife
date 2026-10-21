//! What breaks, and how: `scripts/propdata.txt` and the `prop_data` block
//! of a model's own key-values.
//!
//! A model's `$keyvalues` may name a base class from `propdata.txt`
//! (`prop_data { "base" "Wooden.Medium" }`) and override anything in it
//! (`"health" "20" "explosive_damage" "120" "explosive_radius" "256"`).
//! Classes derive from each other with `"base"`; a value a class does not
//! set comes from its base.
//!
//! | Key | Meaning |
//! |-----|---------|
//! | `health` | Damage the prop takes before breaking; none means it does not break |
//! | `dmg.bullets`, `dmg.club`, `dmg.explosive` | Damage multipliers by kind (default 1) |
//! | `explosive_damage`, `explosive_radius` | The prop explodes when it breaks |
//! | `breakable_model`, `breakable_count` | The gibs it breaks into (a `BreakableModels` list) |

use std::collections::HashMap;

use source_fs::keyvalues::{self, Block};

/// How deep `"base"` chains are followed, to stop a loop in malformed data.
const MAX_BASE_DEPTH: usize = 8;

/// A prop's breaking properties, resolved through its base classes.
#[derive(Debug, Clone, PartialEq)]
pub struct PropData {
    /// Damage the prop takes before breaking; 0 for a prop that does not
    /// break.
    pub health: f32,
    /// Multiplier on damage from bullets.
    pub bullets: f32,
    /// Multiplier on damage from melee weapons.
    pub club: f32,
    /// Multiplier on damage from explosions.
    pub explosive: f32,
    /// Damage of the explosion when it breaks; 0 for none.
    pub explosive_damage: f32,
    /// Radius of that explosion, units.
    pub explosive_radius: f32,
    /// The name of the gib list it breaks into, if any.
    pub breakable_model: Option<String>,
    /// How many gibs it breaks into.
    pub breakable_count: u32,
}

impl Default for PropData {
    fn default() -> Self {
        Self {
            health: 0.0,
            bullets: 1.0,
            club: 1.0,
            explosive: 1.0,
            explosive_damage: 0.0,
            explosive_radius: 0.0,
            breakable_model: None,
            breakable_count: 0,
        }
    }
}

impl PropData {
    /// Whether the prop can be broken.
    #[must_use]
    pub fn breakable(&self) -> bool {
        self.health > 0.0
    }

    /// Whether breaking it sets off an explosion.
    #[must_use]
    pub fn explosive(&self) -> bool {
        self.explosive_damage > 0.0 && self.explosive_radius > 0.0
    }
}

/// The classes of `propdata.txt` and its gib lists.
#[derive(Debug, Clone, Default)]
pub struct PropDataTable {
    /// Each class's own keys, lower-cased, by lower-cased class name.
    classes: HashMap<String, Vec<(String, String)>>,
    /// Gib model paths by lower-cased list name.
    gibs: HashMap<String, Vec<String>>,
}

impl PropDataTable {
    /// Parses `propdata.txt`. Malformed text gives an empty table: props
    /// then simply do not break.
    #[must_use]
    pub fn parse(text: &str) -> Self {
        let Ok(document) = keyvalues::parse(text) else {
            return Self::default();
        };
        let mut table = Self::default();
        let Some(root) = document.root.iter().find_map(|pair| pair.value.as_block()) else {
            return table;
        };
        for pair in root.iter() {
            let Some(block) = pair.value.as_block() else {
                continue;
            };
            if pair.key.eq_ignore_ascii_case("BreakableModels") {
                for list in block.iter() {
                    if let Some(models) = list.value.as_block() {
                        table.gibs.insert(
                            list.key.to_ascii_lowercase(),
                            models
                                .iter()
                                .map(|model| normalize_path(&model.key))
                                .collect(),
                        );
                    }
                }
            } else {
                table
                    .classes
                    .insert(pair.key.to_ascii_lowercase(), entries(block));
            }
        }
        table
    }

    /// The class `name`, resolved through its bases.
    #[must_use]
    pub fn class(&self, name: &str) -> Option<PropData> {
        self.classes
            .contains_key(&name.to_ascii_lowercase())
            .then(|| {
                let mut data = PropData::default();
                self.apply_class(name, &mut data, 0);
                data
            })
    }

    /// The breaking properties of a model, from its `$keyvalues` text:
    /// its `prop_data` block applied on top of the class it names. `None`
    /// when the model has no `prop_data`.
    #[must_use]
    pub fn for_model(&self, model_key_values: &str) -> Option<PropData> {
        let document = keyvalues::parse(model_key_values).ok()?;
        // Compiled models wrap their blocks in `mdlkeyvalue { ... }`.
        let root = document
            .root
            .get_block("mdlkeyvalue")
            .unwrap_or(&document.root);
        let block = root.get_block("prop_data")?;
        let mut data = PropData::default();
        let own = entries(block);
        if let Some((_, base)) = own.iter().find(|(key, _)| key == "base") {
            self.apply_class(base, &mut data, 0);
        }
        apply_entries(&own, &mut data);
        Some(data)
    }

    /// The gib models of list `name`.
    #[must_use]
    pub fn gibs(&self, name: &str) -> &[String] {
        self.gibs
            .get(&name.to_ascii_lowercase())
            .map_or(&[], Vec::as_slice)
    }

    fn apply_class(&self, name: &str, data: &mut PropData, depth: usize) {
        if depth >= MAX_BASE_DEPTH {
            return;
        }
        let Some(own) = self.classes.get(&name.to_ascii_lowercase()) else {
            return;
        };
        if let Some((_, base)) = own.iter().find(|(key, _)| key == "base") {
            self.apply_class(base, data, depth + 1);
        }
        apply_entries(own, data);
    }
}

/// A block's string pairs, with lower-cased keys.
fn entries(block: &Block) -> Vec<(String, String)> {
    block
        .iter()
        .filter_map(|pair| {
            Some((
                pair.key.to_ascii_lowercase(),
                pair.value.as_str()?.to_owned(),
            ))
        })
        .collect()
}

fn apply_entries(entries: &[(String, String)], data: &mut PropData) {
    let number = |value: &str| value.trim().parse::<f32>().ok().filter(|n| n.is_finite());
    for (key, value) in entries {
        match key.as_str() {
            "health" => data.health = number(value).unwrap_or(data.health).max(0.0),
            "dmg.bullets" => data.bullets = number(value).unwrap_or(data.bullets).max(0.0),
            "dmg.club" => data.club = number(value).unwrap_or(data.club).max(0.0),
            "dmg.explosive" => data.explosive = number(value).unwrap_or(data.explosive).max(0.0),
            "explosive_damage" => {
                data.explosive_damage = number(value).unwrap_or(data.explosive_damage).max(0.0);
            }
            "explosive_radius" => {
                data.explosive_radius = number(value).unwrap_or(data.explosive_radius).max(0.0);
            }
            "breakable_model" => data.breakable_model = Some(value.trim().to_owned()),
            "breakable_count" => {
                data.breakable_count = number(value).map_or(data.breakable_count, |n| n as u32);
            }
            _ => {}
        }
    }
}

/// A model path with forward slashes, lower case.
fn normalize_path(path: &str) -> String {
    path.trim().replace('\\', "/").to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape of the real file, abridged.
    const TABLE: &str = r#"
"PropData.txt"
{
    "Wooden.Base"
    {
        "dmg.bullets" "1.5"
        "dmg.club" "0.75"
    }
    "Wooden.Small"
    {
        "base" "Wooden.Base"
        "health" "10"
        "breakable_model" "WoodChunks"
        "breakable_count" "4"
    }
    "Wooden.Medium"
    {
        "base" "Wooden.Small"
        "health" "20"
    }
    "Loop.A" { "base" "Loop.B" "health" "1" }
    "Loop.B" { "base" "Loop.A" "dmg.club" "3" }
    "BreakableModels"
    {
        "WoodChunks"
        {
            "models\Gibs\Wood_gib01e.mdl" "1"
            "models\Gibs\wood_gib01d.mdl" "1"
        }
    }
}
"#;

    #[test]
    fn classes_inherit_from_their_bases() {
        let table = PropDataTable::parse(TABLE);
        let medium = table.class("wooden.medium").unwrap();
        assert_eq!(medium.health, 20.0, "its own value wins");
        assert_eq!(medium.bullets, 1.5, "from the base of its base");
        assert_eq!(medium.club, 0.75);
        assert_eq!(medium.explosive, 1.0, "default");
        assert_eq!(medium.breakable_model.as_deref(), Some("WoodChunks"));
        assert_eq!(medium.breakable_count, 4);
        assert!(medium.breakable() && !medium.explosive());
        assert!(table.class("Nope").is_none());
        assert!(!table.class("Wooden.Base").unwrap().breakable());
    }

    #[test]
    fn base_loops_in_bad_data_end() {
        let table = PropDataTable::parse(TABLE);
        let looped = table.class("Loop.A").unwrap();
        assert_eq!(looped.health, 1.0);
        assert_eq!(looped.club, 3.0);
    }

    #[test]
    fn gib_lists_are_normalised() {
        let table = PropDataTable::parse(TABLE);
        assert_eq!(
            table.gibs("woodchunks"),
            ["models/gibs/wood_gib01e.mdl", "models/gibs/wood_gib01d.mdl"]
        );
        assert!(table.gibs("Nope").is_empty());
    }

    #[test]
    fn a_models_prop_data_overrides_its_class() {
        let table = PropDataTable::parse(TABLE);
        let crate_data = table
            .for_model("prop_data {\n\"base\" \"Wooden.Medium\"  }\nphysgun_interactions {\n\"onfirstimpact\" \"break\"  }\n")
            .unwrap();
        assert_eq!(crate_data.health, 20.0);

        let barrel = table
            .for_model(
                "prop_data {\n\"dmg.bullets\" \"1.0\" \"health\" \"20\" \"explosive_damage\" \"120\" \"explosive_radius\" \"256\"  }\n",
            )
            .unwrap();
        assert_eq!(barrel.health, 20.0);
        assert!(barrel.explosive());
        assert_eq!(
            (barrel.explosive_damage, barrel.explosive_radius),
            (120.0, 256.0)
        );
        assert_eq!(barrel.bullets, 1.0);

        // As compiled into a model.
        let wrapped = table
            .for_model("mdlkeyvalue\n{\nprop_data {\n\"base\" \"Wooden.Small\" }\n}\n")
            .unwrap();
        assert_eq!(wrapped.health, 10.0);

        assert_eq!(table.for_model("physgun_interactions { }"), None);
        assert_eq!(table.for_model("not key values {{{"), None);
    }

    #[test]
    fn garbage_gives_an_empty_table() {
        let table = PropDataTable::parse("}{ nonsense");
        assert!(table.class("Wooden.Small").is_none());
    }
}
