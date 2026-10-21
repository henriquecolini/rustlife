//! A map's entities, as plain key-value records.
//!
//! The BSP entity lump is a list of blocks of `"key" "value"` pairs. Most of
//! the format layer reads the few entities it understands itself (spawns,
//! props, ladders); this module hands every entity on to the simulation,
//! which decides what each class means for rustlife (`entity_map.ron`,
//! `trigger_hurt`, ...).

use vbsp::Bsp;

use crate::bsp::parse_vector;

/// One entity of a map.
#[derive(Debug, Clone, PartialEq)]
pub struct MapEntity {
    /// The entity class, lower case (e.g. `item_healthkit`).
    pub classname: String,
    /// Origin, Source space (zero if the entity has none).
    pub origin: [f32; 3],
    /// Orientation, `[pitch, yaw, roll]` in degrees.
    pub angles: [f32; 3],
    /// Every key-value pair, in file order, keys as written.
    pub properties: Vec<(String, String)>,
}

impl MapEntity {
    /// An entity of `classname` at `origin`, with no other properties.
    #[must_use]
    pub fn new(classname: &str, origin: [f32; 3]) -> Self {
        Self {
            classname: classname.to_ascii_lowercase(),
            origin,
            angles: [0.0; 3],
            properties: Vec::new(),
        }
    }

    /// This entity with property `key` set to `value`.
    #[must_use]
    pub fn with(mut self, key: &str, value: &str) -> Self {
        self.properties.push((key.to_owned(), value.to_owned()));
        self
    }

    /// The value of property `key` (case-insensitive, as the engine reads
    /// keys); the last one wins when a key repeats.
    #[must_use]
    pub fn property(&self, key: &str) -> Option<&str> {
        self.properties
            .iter()
            .rev()
            .find(|(name, _)| name.eq_ignore_ascii_case(key))
            .map(|(_, value)| value.as_str())
    }

    /// Property `key` parsed as a number.
    #[must_use]
    pub fn number(&self, key: &str) -> Option<f32> {
        self.property(key)?.trim().parse().ok()
    }

    /// Property `key` parsed as a vector, `"x y z"`.
    #[must_use]
    pub fn vector(&self, key: &str) -> Option<[f32; 3]> {
        parse_vector(self.property(key)?)
    }

    /// The BSP model of a brush entity (`model` is `*<n>`).
    #[must_use]
    pub fn brush_model(&self) -> Option<usize> {
        self.property("model")?.strip_prefix('*')?.parse().ok()
    }
}

/// Every entity of the map with a classname, in file order.
pub(crate) fn entities(bsp: &Bsp) -> Vec<MapEntity> {
    bsp.entities
        .iter()
        .filter_map(|entity| {
            let classname = entity.prop("classname")?.to_ascii_lowercase();
            let vector = |key| entity.prop(key).and_then(parse_vector).unwrap_or_default();
            Some(MapEntity {
                classname,
                origin: vector("origin"),
                angles: vector("angles"),
                properties: entity
                    .properties()
                    .map(|(key, value)| (key.to_owned(), value.to_owned()))
                    .collect(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn properties_are_found_case_insensitively_last_one_winning() {
        let entity = MapEntity::new("Trigger_Hurt", [1.0, 2.0, 3.0])
            .with("damage", "10")
            .with("model", "*4")
            .with("Damage", "20");
        assert_eq!(entity.classname, "trigger_hurt");
        assert_eq!(entity.number("DAMAGE"), Some(20.0));
        assert_eq!(entity.brush_model(), Some(4));
        assert_eq!(entity.property("missing"), None);
    }
}
