//! Hazards: places in the map that hurt (`trigger_hurt`).
//!
//! Mappers mark pits, crushers and kill zones with `trigger_hurt` brush
//! entities: anyone inside takes the entity's `damage` per second. Like
//! Source, the damage comes in two doses a second rather than a trickle
//! every tick, so a hazard reads as a few clear hits.
//!
//! A hazard's volume is its brush model's bounding box. Hurt triggers are
//! nearly always boxes, and a box needs no clipping against the model's
//! (often missing) trigger brushes.

use glam::Vec3;
use source_formats::entities::MapEntity;

use crate::collision::CollisionWorld;

/// Seconds between doses of a hazard's damage (Source's `trigger_hurt`
/// hurts twice a second).
pub const DOSE_INTERVAL: f32 = 0.5;

/// A volume that hurts whoever is inside.
#[derive(Debug, Clone, PartialEq)]
pub struct Hazard {
    /// Minimum corner, Source space.
    pub mins: Vec3,
    /// Maximum corner, Source space.
    pub maxs: Vec3,
    /// Damage per second.
    pub damage: f32,
}

impl Hazard {
    /// Whether a player's box `mins..maxs` is (partly) inside.
    #[must_use]
    pub fn touches(&self, mins: Vec3, maxs: Vec3) -> bool {
        mins.cmplt(self.maxs).all() && self.mins.cmplt(maxs).all()
    }

    /// The damage of one dose.
    #[must_use]
    pub fn dose(&self) -> f32 {
        self.damage * DOSE_INTERVAL
    }
}

/// The enabled, harmful `trigger_hurt` entities of a map.
#[must_use]
pub fn from_entities(entities: &[MapEntity], world: &CollisionWorld) -> Vec<Hazard> {
    entities
        .iter()
        .filter(|entity| entity.classname == "trigger_hurt")
        .filter(|entity| entity.property("StartDisabled") != Some("1"))
        .filter_map(|entity| {
            let damage = entity.number("damage")?;
            if damage <= 0.0 {
                // Negative damage heals in Source; rustlife has no healing
                // zones.
                return None;
            }
            let (mins, maxs) = world.model_bounds(
                entity.brush_model()?,
                Vec3::from(entity.origin),
                entity.angles,
            )?;
            Some(Hazard { mins, maxs, damage })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn players_touch_hazards_they_overlap() {
        let pit = Hazard {
            mins: Vec3::new(0.0, 0.0, -100.0),
            maxs: Vec3::new(100.0, 100.0, 0.0),
            damage: 1000.0,
        };
        let player = |z: f32| (Vec3::new(40.0, 40.0, z), Vec3::new(72.0, 72.0, z + 72.0));
        let (mins, maxs) = player(-50.0);
        assert!(pit.touches(mins, maxs));
        let (mins, maxs) = player(10.0);
        assert!(!pit.touches(mins, maxs), "standing on the lid");
        assert_eq!(pit.dose(), 500.0);
    }

    #[test]
    fn entities_without_a_model_or_damage_are_no_hazard() {
        let world = crate::collision::WorldBuilder::new().build();
        let entities = [
            MapEntity::new("trigger_hurt", [0.0; 3]).with("damage", "20"),
            MapEntity::new("trigger_hurt", [0.0; 3]).with("model", "*1"),
            MapEntity::new("trigger_multiple", [0.0; 3])
                .with("model", "*1")
                .with("damage", "20"),
        ];
        assert!(from_entities(&entities, &world).is_empty());
    }
}
