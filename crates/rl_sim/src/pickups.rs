//! Pickups: the weapons and items lying about a map.
//!
//! A map places them as entities of its game's classes; `entity_map.ron`
//! says which of rustlife's weapons or items each becomes
//! ([`from_entities`]). A pickup is taken by the first living player to come
//! within its radius who can use it — a full player leaves a medkit where it
//! is — and comes back after its respawn time.
//!
//! *Stations* (wall chargers) are used rather than touched: a player
//! holding use at one draws its health or armor at its rate
//! ([`Pickup::dispense`]) until its charge is spent; it is then away until
//! it has recharged.

use std::collections::BTreeSet;

use glam::Vec3;
use rl_data::{GameData, ItemEffect, MappedEntity, PlayerConfig};
use source_formats::entities::MapEntity;

use crate::combat::Vitals;
use crate::physics::BodyId;
use crate::weapons::{AmmoId, Arsenal, Inventory, WeaponId};

/// What a pickup gives.
#[derive(Debug, Clone, PartialEq)]
pub enum Gives {
    /// A weapon (with a full clip if new) and rounds of its main ammo.
    Weapon {
        /// The weapon.
        weapon: WeaponId,
        /// Rounds of its main ammo type, if it has one.
        ammo: Option<(AmmoId, u32)>,
    },
    /// Health, up to the player's maximum.
    Health(f32),
    /// Armor, up to the player's maximum.
    Armor(f32),
    /// Rounds of ammo types.
    Ammo(Vec<(AmmoId, u32)>),
}

/// A weapon or item lying in the map.
#[derive(Debug, Clone, PartialEq)]
pub struct Pickup {
    /// The weapon's or item's id (its data file name).
    pub id: String,
    /// Its name, for the HUD.
    pub name: String,
    /// What it gives.
    pub gives: Gives,
    /// Model drawn for it.
    pub model: String,
    /// Where it lies, Source space.
    pub origin: Vec3,
    /// Its orientation, `[pitch, yaw, roll]` degrees, as the map placed it.
    pub angles: [f32; 3],
    /// Degrees added to the yaw of `angles` to turn the model the way the map
    /// means it to face.
    pub model_yaw: f32,
    /// A player touches it within this distance of their box, units.
    pub radius: f32,
    /// Seconds it takes to come back once taken.
    pub respawn: f32,
    /// Seconds until it is back; zero while it is there to take.
    pub away: f32,
    /// For a station: points given per second of use.
    pub station: Option<f32>,
    /// For a station: what is left of its charge (its `gives`).
    pub charge: f32,
    /// The body it lies as in the physics world, if it is a thing that can
    /// fall and be grabbed. Stations have none, and so do pickups in a
    /// simulation that did not give them bodies.
    pub body: Option<BodyId>,
}

impl Pickup {
    /// The orientation to draw its model at: `angles` with `model_yaw`
    /// added to the yaw, normalized to `[0, 360)`.
    #[must_use]
    pub fn model_angles(&self) -> [f32; 3] {
        let [pitch, yaw, roll] = self.angles;
        [pitch, (yaw + self.model_yaw).rem_euclid(360.0), roll]
    }

    /// Whether it is there to be taken.
    #[must_use]
    pub fn available(&self) -> bool {
        self.away <= 0.0
    }

    /// Whether a player's box `mins..maxs` touches it.
    #[must_use]
    pub fn touches(&self, mins: Vec3, maxs: Vec3) -> bool {
        self.origin.clamp(mins, maxs).distance_squared(self.origin) <= self.radius * self.radius
    }

    /// Counts down its absence by `dt` seconds; `true` on the tick it comes
    /// back.
    pub fn tick(&mut self, dt: f32) -> bool {
        if self.away <= 0.0 {
            return false;
        }
        self.away -= dt;
        let back = self.away <= 0.0;
        if back {
            self.charge = self.capacity();
        }
        back
    }

    /// A station's full charge: what it gives (zero for other pickups).
    #[must_use]
    pub fn capacity(&self) -> f32 {
        match self.gives {
            Gives::Health(amount) | Gives::Armor(amount) if self.station.is_some() => amount,
            _ => 0.0,
        }
    }

    /// A station being used for `dt` seconds: gives a player what of its
    /// rate they can take, from its charge; once that is spent it is away
    /// until recharged. Returns how much it gave.
    pub fn dispense(&mut self, vitals: &mut Vitals, player: &PlayerConfig, dt: f32) -> f32 {
        let Some(rate) = self.station else {
            return 0.0;
        };
        if !self.available() {
            return 0.0;
        }
        let dose = (rate * dt).min(self.charge);
        let (value, max) = match self.gives {
            Gives::Health(_) => (&mut vitals.health, player.health.max),
            Gives::Armor(_) => (&mut vitals.armor, player.armor.max),
            Gives::Weapon { .. } | Gives::Ammo(_) => return 0.0,
        };
        let given = dose.min(max - *value).max(0.0);
        *value += given;
        self.charge -= given;
        if self.charge <= 0.0 {
            self.charge = 0.0;
            self.away = self.respawn.max(f32::MIN_POSITIVE);
        }
        given
    }

    /// Gives it to a player if they can use any of it; then it is away for
    /// its respawn time. Returns whether it was taken.
    pub fn take(
        &mut self,
        vitals: &mut Vitals,
        inventory: &mut Inventory,
        arsenal: &Arsenal,
        player: &PlayerConfig,
    ) -> bool {
        if !self.available() || self.station.is_some() {
            return false;
        }
        let used = match &self.gives {
            Gives::Weapon { weapon, ammo } => {
                let new = inventory.give_weapon(arsenal, *weapon);
                let rounds =
                    ammo.is_some_and(|(ammo, count)| inventory.give_ammo(arsenal, ammo, count) > 0);
                new || rounds
            }
            Gives::Health(amount) => vitals.heal(*amount, player.health.max),
            Gives::Armor(amount) => vitals.add_armor(*amount, player.armor.max),
            // Every type is offered, even after one is taken.
            Gives::Ammo(rounds) => rounds.iter().fold(false, |taken, &(ammo, count)| {
                inventory.give_ammo(arsenal, ammo, count) > 0 || taken
            }),
        };
        if used {
            self.away = self.respawn.max(f32::MIN_POSITIVE);
        }
        used
    }
}

/// The pickups `entities` make under `data`'s entity map. Classnames the
/// map does not list, and listed ones naming missing data, are logged once
/// each and skipped.
#[must_use]
pub fn from_entities(entities: &[MapEntity], data: &GameData, arsenal: &Arsenal) -> Vec<Pickup> {
    let mut unmapped = BTreeSet::new();
    let pickups = entities
        .iter()
        .filter_map(|entity| {
            let mapped = data.entity_map.get(&entity.classname);
            if mapped.is_none()
                && (entity.classname.starts_with("weapon_")
                    || entity.classname.starts_with("item_"))
            {
                unmapped.insert(entity.classname.as_str());
            }
            let pickup = match mapped? {
                MappedEntity::Weapon(id) => weapon_pickup(id, data, arsenal)?,
                MappedEntity::Item(id) => item_pickup(id, data, arsenal)?,
                MappedEntity::Ignore => return None,
            };
            Some(Pickup {
                origin: Vec3::from(entity.origin),
                angles: entity.angles,
                ..pickup
            })
        })
        .collect();
    for classname in unmapped {
        tracing::info!(classname, "entity_map.ron does not map this pickup class");
    }
    pickups
}

/// A pickup of weapon `id`, placed at the origin.
fn weapon_pickup(id: &str, data: &GameData, arsenal: &Arsenal) -> Option<Pickup> {
    let config = data.weapons.get(id)?;
    let pickup = config.pickup.as_ref()?;
    let ammo = config
        .ammo_type()
        .and_then(|ammo| arsenal.find_ammo(ammo))
        .map(|ammo| (ammo, pickup.ammo))
        .filter(|&(_, count)| count > 0);
    Some(Pickup {
        id: id.to_owned(),
        name: config.name.clone(),
        gives: Gives::Weapon {
            weapon: arsenal.find_weapon(id)?,
            ammo,
        },
        model: config.world_model.clone(),
        origin: Vec3::ZERO,
        angles: [0.0; 3],
        model_yaw: 0.0,
        radius: pickup.radius,
        respawn: pickup.respawn,
        away: 0.0,
        station: None,
        charge: 0.0,
        body: None,
    })
}

/// A pickup of item `id`, placed at the origin.
fn item_pickup(id: &str, data: &GameData, arsenal: &Arsenal) -> Option<Pickup> {
    let item = data.items.get(id)?;
    let gives = match &item.gives {
        ItemEffect::Health(amount) => Gives::Health(*amount),
        ItemEffect::Armor(amount) => Gives::Armor(*amount),
        ItemEffect::Ammo(rounds) => Gives::Ammo(
            rounds
                .iter()
                .filter_map(|(ammo, &count)| Some((arsenal.find_ammo(ammo)?, count)))
                .collect(),
        ),
    };
    let mut pickup = Pickup {
        id: id.to_owned(),
        name: item.name.clone(),
        gives,
        model: item.model.clone(),
        origin: Vec3::ZERO,
        angles: [0.0; 3],
        model_yaw: item.model_yaw,
        radius: item.radius,
        respawn: item.respawn,
        away: 0.0,
        station: item.station.as_ref().map(|station| station.rate),
        charge: 0.0,
        body: None,
    };
    pickup.charge = pickup.capacity();
    Some(pickup)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data() -> GameData {
        GameData::builtin().expect("builtin data")
    }

    fn pickups(entities: &[MapEntity]) -> (GameData, Arsenal, Vec<Pickup>) {
        let data = data();
        let arsenal = Arsenal::new(&data);
        let pickups = from_entities(entities, &data, &arsenal);
        (data, arsenal, pickups)
    }

    #[test]
    fn wall_chargers_are_drawn_turned_off_their_entity_angles() {
        let mut charger = MapEntity::new("item_healthcharger", [0.0; 3]);
        charger.angles = [0.0, 270.0, 0.0];
        let (_, _, pickups) = pickups(&[charger]);
        assert_eq!(pickups[0].angles, [0.0, 270.0, 0.0]);
        assert_eq!(pickups[0].model_angles(), [0.0, 0.0, 0.0]);
    }

    #[test]
    fn map_entities_become_rustlife_pickups() {
        let (_, arsenal, pickups) = pickups(&[
            MapEntity::new("item_healthkit", [10.0, 20.0, 30.0]),
            MapEntity::new("weapon_shotgun", [0.0; 3]),
            MapEntity::new("weapon_physcannon", [0.0; 3]),
            MapEntity::new("item_unheard_of", [0.0; 3]),
            MapEntity::new("info_player_deathmatch", [0.0; 3]),
        ]);
        assert_eq!(pickups.len(), 2);
        assert_eq!(pickups[0].id, "medkit");
        assert_eq!(pickups[0].origin, Vec3::new(10.0, 20.0, 30.0));
        assert_eq!(pickups[0].gives, Gives::Health(25.0));
        assert_eq!(
            pickups[1].gives,
            Gives::Weapon {
                weapon: arsenal.find_weapon("scattergun").expect("weapon"),
                ammo: Some((arsenal.find_ammo("buckshot").expect("ammo"), 12)),
            }
        );
    }

    #[test]
    fn only_players_who_can_use_a_pickup_take_it() {
        let (data, arsenal, mut pickups) = pickups(&[MapEntity::new("item_healthkit", [0.0; 3])]);
        let medkit = &mut pickups[0];
        let mut inventory = Inventory::empty(&arsenal);
        let mut healthy = Vitals::spawn(&data.player);
        assert!(!medkit.take(&mut healthy, &mut inventory, &arsenal, &data.player));
        let mut hurt = Vitals {
            health: 50.0,
            armor: 0.0,
        };
        assert!(medkit.take(&mut hurt, &mut inventory, &arsenal, &data.player));
        assert_eq!(hurt.health, 75.0);
        assert!(!medkit.available());
        assert!(
            !medkit.take(&mut hurt, &mut inventory, &arsenal, &data.player),
            "gone until it respawns"
        );
        let ticks = (20.0 * 66.0) as usize;
        let back = (0..=ticks).filter(|_| medkit.tick(1.0 / 66.0)).count();
        assert_eq!(back, 1);
        assert!(medkit.available());
    }

    #[test]
    fn weapon_pickups_give_ammo_to_those_who_have_the_weapon() {
        let (data, arsenal, mut pickups) = pickups(&[MapEntity::new("weapon_shotgun", [0.0; 3])]);
        let mut vitals = Vitals::spawn(&data.player);
        let mut inventory = Inventory::empty(&arsenal);
        let shells = arsenal.find_ammo("buckshot").expect("ammo");
        assert!(pickups[0].take(&mut vitals, &mut inventory, &arsenal, &data.player));
        assert_eq!(inventory.ammo(shells), 12);
        pickups[0].away = 0.0;
        assert!(pickups[0].take(&mut vitals, &mut inventory, &arsenal, &data.player));
        assert_eq!(inventory.ammo(shells), 24);
        inventory.give_ammo(&arsenal, shells, 100);
        pickups[0].away = 0.0;
        assert!(
            !pickups[0].take(&mut vitals, &mut inventory, &arsenal, &data.player),
            "nothing to gain"
        );
    }

    #[test]
    fn stations_dispense_their_charge_at_their_rate_then_recharge() {
        let (data, arsenal, mut pickups) =
            pickups(&[MapEntity::new("item_healthcharger", [0.0; 3])]);
        let station = &mut pickups[0];
        assert_eq!((station.station, station.charge), (Some(25.0), 50.0));
        let mut vitals = Vitals {
            health: 10.0,
            armor: 0.0,
        };
        let mut inventory = Inventory::empty(&arsenal);
        assert!(
            !station.take(&mut vitals, &mut inventory, &arsenal, &data.player),
            "used, not picked up"
        );
        let given = station.dispense(&mut vitals, &data.player, 1.0);
        assert_eq!((given, vitals.health, station.charge), (25.0, 35.0, 25.0));
        vitals.health = 90.0;
        assert_eq!(
            station.dispense(&mut vitals, &data.player, 1.0),
            10.0,
            "up to the maximum"
        );
        vitals.health = 0.0;
        assert_eq!(
            station.dispense(&mut vitals, &data.player, 1.0),
            15.0,
            "what was left"
        );
        assert!(!station.available(), "spent");
        assert_eq!(station.dispense(&mut vitals, &data.player, 1.0), 0.0);
        let back = (0..=(61 * 66)).filter(|_| station.tick(1.0 / 66.0)).count();
        assert_eq!((back, station.charge), (1, 50.0), "recharged");
    }

    #[test]
    fn touching_is_measured_to_the_players_box() {
        let (_, _, pickups) = pickups(&[MapEntity::new("item_battery", [0.0, 0.0, 0.0])]);
        let at = |x: f32| {
            (
                Vec3::new(x - 16.0, -16.0, 0.0),
                Vec3::new(x + 16.0, 16.0, 72.0),
            )
        };
        let (mins, maxs) = at(30.0);
        assert!(pickups[0].touches(mins, maxs));
        let (mins, maxs) = at(60.0);
        assert!(!pickups[0].touches(mins, maxs));
    }
}
