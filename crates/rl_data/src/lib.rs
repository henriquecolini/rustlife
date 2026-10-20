//! rustlife's own gameplay data: schema, validation and loading.
//!
//! Everything tunable about how the game plays lives in RON files under
//! `data/` rather than in code. This crate defines the schema of each file
//! (with `deny_unknown_fields`, so a typo is an error rather than a silently
//! ignored key), validates the values, checks that files agree with each
//! other (a weapon cannot name a missing ammo type), and loads them.
//!
//! The repository's `data/` directory is compiled into the crate, so the game
//! always has a complete, valid set ([`GameData::builtin`]). A data directory
//! given at run time ([`GameData::load`]) overrides it file by file: any file
//! it contains replaces the built-in one, and missing files fall back to the
//! built-in copies. Files it adds to `weapons/` and `items/` add weapons and
//! items.
//!
//! A server sends its data to every client that joins ([`GameData::to_files`],
//! [`GameData::from_files`]), so both run the same rules; [`content_hash`]
//! lets a client that already has identical data skip the transfer.
//!
//! As of M7 the data set is:
//!
//! | File | Type | Contents |
//! |------|------|----------|
//! | `movement.ron` | [`MovementConfig`] | Player movement physics. |
//! | `simulation.ron` | [`SimulationConfig`] | Tick rate. |
//! | `physics.ron` | [`PhysicsConfig`] | Prop physics, materials, pushing, impact damage. |
//! | `tools/manipulator.ron` | [`ManipulatorConfig`] | The grab-and-launch tool. |
//! | `ammo.ron` | [`AmmoConfig`] | Ammo types and how much of each a player carries. |
//! | `weapons/<id>.ron` | [`WeaponConfig`] | One weapon each. |
//! | `items/<id>.ron` | [`ItemConfig`] | One health, armor or ammo pickup each. |
//! | `entity_map.ron` | [`EntityMapConfig`] | Map entity classes → rustlife pickups. |
//! | `player.ron` | [`PlayerConfig`] | Health, armor, player models. |
//! | `rules/deathmatch.ron` | [`RulesConfig`] | Limits, respawning, loadout, fall damage. |
//! | `hud.ron` | [`HudConfig`] | HUD layout and colours. |
//! | `net.ron` | [`NetConfig`] | Ports, snapshot rate, interpolation, lag compensation. |
//! | `map_logic.ron` | [`MapLogicConfig`] | Using doors and buttons, map explosions, the event budget. |

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;

pub mod ammo;
pub mod entity_map;
pub mod hud;
pub mod items;
pub mod manipulator;
pub mod map_logic;
pub mod movement;
pub mod net;
pub mod physics;
pub mod player;
pub mod rules;
pub mod simulation;
mod validate;
pub mod weapons;

pub use ammo::{AmmoConfig, AmmoType};
pub use entity_map::{EntityMapConfig, MappedEntity};
pub use hud::{Anchor, CrosshairConfig, HudColors, HudConfig, KillFeedConfig, Placement, Rgba};
pub use items::{ItemConfig, ItemEffect};
pub use manipulator::{GrabConfig, HoldConfig, LaunchConfig, ManipulatorConfig};
pub use map_logic::{MapExplosionConfig, MapLogicConfig};
pub use movement::{
    AirConfig, DuckConfig, GroundConfig, HullConfig, HullsConfig, JumpConfig, LadderConfig,
    MovementConfig, WaterConfig,
};
pub use net::NetConfig;
pub use physics::{ImpactDamageConfig, MaterialConfig, MaterialsConfig, PhysicsConfig, PushConfig};
pub use player::{ArmorConfig, HealthConfig, PlayerConfig};
pub use rules::{FallDamageConfig, LoadoutConfig, RespawnConfig, RulesConfig};
pub use simulation::SimulationConfig;
pub use validate::Problem;
pub use weapons::{
    ClipConfig, ExplosionConfig, Falloff, FireAmmo, FireConfig, HitscanConfig, MeleeConfig,
    ProjectileConfig, Reload, Shot, WeaponConfig, WeaponPickupConfig,
};

/// File name of the movement data.
pub const MOVEMENT_FILE: &str = "movement.ron";
/// File name of the simulation timing data.
pub const SIMULATION_FILE: &str = "simulation.ron";
/// File name of the prop physics data.
pub const PHYSICS_FILE: &str = "physics.ron";
/// Path of the Manipulator's data, relative to the data directory.
pub const MANIPULATOR_FILE: &str = "tools/manipulator.ron";
/// File name of the ammo types.
pub const AMMO_FILE: &str = "ammo.ron";
/// Directory of the weapons, one file each.
pub const WEAPONS_DIR: &str = "weapons";
/// Directory of the items, one file each.
pub const ITEMS_DIR: &str = "items";
/// File name of the map entity mapping.
pub const ENTITY_MAP_FILE: &str = "entity_map.ron";
/// File name of the player data.
pub const PLAYER_FILE: &str = "player.ron";
/// File name of the HUD layout.
pub const HUD_FILE: &str = "hud.ron";
/// File name of the networking data.
pub const NET_FILE: &str = "net.ron";
/// File name of the map logic data.
pub const MAP_LOGIC_FILE: &str = "map_logic.ron";
pub use rules::DEATHMATCH_FILE;

/// Pairs each data file's path (relative to `data/`) with its built-in text.
macro_rules! builtin_files {
    ($($name:literal),* $(,)?) => {
        [$(($name, include_str!(concat!("../../../data/", $name)))),*]
    };
}

/// The built-in copy of each data file, from the repository's `data/`.
const BUILTIN: &[(&str, &str)] = &builtin_files![
    "movement.ron",
    "simulation.ron",
    "physics.ron",
    "tools/manipulator.ron",
    "ammo.ron",
    "entity_map.ron",
    "player.ron",
    "rules/deathmatch.ron",
    "hud.ron",
    "net.ron",
    "map_logic.ron",
    "weapons/bar.ron",
    "weapons/boltcaster.ron",
    "weapons/frag.ron",
    "weapons/launcher.ron",
    "weapons/magnum.ron",
    "weapons/manipulator.ron",
    "weapons/rifle.ron",
    "weapons/scattergun.ron",
    "weapons/sidearm.ron",
    "weapons/smg.ron",
    "items/armor_station.ron",
    "items/battery.ron",
    "items/bolts.ron",
    "items/health_station.ron",
    "items/health_vial.ron",
    "items/magnum_rounds.ron",
    "items/medkit.ron",
    "items/pistol_rounds.ron",
    "items/rifle_cells.ron",
    "items/rifle_charges.ron",
    "items/rockets.ron",
    "items/shells.ron",
    "items/smg_grenades.ron",
    "items/smg_rounds.ron",
];

/// Errors from loading gameplay data. Each names the file it concerns.
#[derive(Debug, thiserror::Error)]
pub enum DataError {
    /// The file exists but could not be read.
    #[error("cannot read {path}: {source}")]
    Io {
        /// The data file.
        path: PathBuf,
        /// Underlying error.
        #[source]
        source: io::Error,
    },
    /// The file is not valid RON or does not match the schema.
    #[error("{file}: {source}")]
    Parse {
        /// Where the file came from (a path, or `builtin:<name>`).
        file: String,
        /// Parser error with line and column; boxed because it is large.
        #[source]
        source: Box<ron::error::SpannedError>,
    },
    /// A data set held in memory (one a server sent) lacks a file.
    #[error("{file}: missing from the data set")]
    Missing {
        /// The missing file's path, relative to the data directory.
        file: String,
    },
    /// A data file could not be written out as RON.
    #[error("{file}: cannot serialise: {source}")]
    Serialize {
        /// The file's path, relative to the data directory.
        file: String,
        /// Serialiser error.
        #[source]
        source: ron::Error,
    },
    /// The file parsed, but some values make no sense. For problems between
    /// files, `file` is `references` and each field names its file.
    #[error("{file}: {}", problems.iter().map(ToString::to_string).collect::<Vec<_>>().join("; "))]
    Invalid {
        /// Where the file came from (a path, or `builtin:<name>`).
        file: String,
        /// Every problem found.
        problems: Vec<Problem>,
    },
}

/// The complete, validated gameplay data set.
#[derive(Debug, Clone, PartialEq)]
pub struct GameData {
    /// Player movement.
    pub movement: MovementConfig,
    /// Simulation timing.
    pub simulation: SimulationConfig,
    /// Prop physics.
    pub physics: PhysicsConfig,
    /// The Manipulator tool.
    pub manipulator: ManipulatorConfig,
    /// Ammo types.
    pub ammo: AmmoConfig,
    /// Weapons by id (their file name).
    pub weapons: BTreeMap<String, WeaponConfig>,
    /// Items by id (their file name).
    pub items: BTreeMap<String, ItemConfig>,
    /// What map entities become.
    pub entity_map: EntityMapConfig,
    /// Health, armor and models.
    pub player: PlayerConfig,
    /// The rules of the game.
    pub rules: RulesConfig,
    /// The HUD.
    pub hud: HudConfig,
    /// Networking.
    pub net: NetConfig,
    /// The map's doors, buttons, triggers and traps.
    pub map_logic: MapLogicConfig,
}

/// Where data files come from.
#[derive(Debug, Clone, Copy)]
enum Files<'a> {
    /// The built-in set, each file overridden by its copy in this
    /// directory, if any.
    Overlay(Option<&'a Path>),
    /// A complete set in memory, text by path: what a server sends.
    Memory(&'a BTreeMap<String, String>),
}

impl GameData {
    /// The data compiled into the game.
    ///
    /// # Errors
    ///
    /// Only if the repository's own data is broken, which the crate's tests
    /// rule out.
    pub fn builtin() -> Result<Self, DataError> {
        Self::load(None)
    }

    /// Loads the data, taking each file from `dir` when it exists there and
    /// from the built-in set otherwise, then checks the files agree.
    ///
    /// # Errors
    ///
    /// Fails on the first file that cannot be read, parsed or validated, or
    /// with every disagreement between files.
    pub fn load(dir: Option<&Path>) -> Result<Self, DataError> {
        Self::assemble(Files::Overlay(dir))
    }

    /// Loads a complete data set from memory, text by path relative to the
    /// data directory (as [`to_files`](Self::to_files) makes it), with the
    /// same validation as files on disk. Nothing falls back to the
    /// built-in data: a set lacking a file is an error.
    ///
    /// # Errors
    ///
    /// Fails on the first file that is missing, cannot be parsed or is
    /// invalid, or with every disagreement between files.
    pub fn from_files(files: &BTreeMap<String, String>) -> Result<Self, DataError> {
        Self::assemble(Files::Memory(files))
    }

    /// Writes the data set out as RON text by path relative to the data
    /// directory: what a server sends its clients.
    ///
    /// # Errors
    ///
    /// Only if a value cannot be written as RON.
    pub fn to_files(&self) -> Result<BTreeMap<String, String>, DataError> {
        let mut files = BTreeMap::new();
        let mut put = |file: String, text: Result<String, ron::Error>| {
            let text = text.map_err(|source| DataError::Serialize {
                file: file.clone(),
                source,
            })?;
            files.insert(file, text);
            Ok::<(), DataError>(())
        };
        put(MOVEMENT_FILE.to_owned(), ron::to_string(&self.movement))?;
        put(SIMULATION_FILE.to_owned(), ron::to_string(&self.simulation))?;
        put(PHYSICS_FILE.to_owned(), ron::to_string(&self.physics))?;
        put(
            MANIPULATOR_FILE.to_owned(),
            ron::to_string(&self.manipulator),
        )?;
        put(AMMO_FILE.to_owned(), ron::to_string(&self.ammo))?;
        for (id, weapon) in &self.weapons {
            put(format!("{WEAPONS_DIR}/{id}.ron"), ron::to_string(weapon))?;
        }
        for (id, item) in &self.items {
            put(format!("{ITEMS_DIR}/{id}.ron"), ron::to_string(item))?;
        }
        put(ENTITY_MAP_FILE.to_owned(), ron::to_string(&self.entity_map))?;
        put(PLAYER_FILE.to_owned(), ron::to_string(&self.player))?;
        put(DEATHMATCH_FILE.to_owned(), ron::to_string(&self.rules))?;
        put(HUD_FILE.to_owned(), ron::to_string(&self.hud))?;
        put(NET_FILE.to_owned(), ron::to_string(&self.net))?;
        put(MAP_LOGIC_FILE.to_owned(), ron::to_string(&self.map_logic))?;
        Ok(files)
    }

    /// Loads every file from `files` and checks they agree.
    fn assemble(files: Files<'_>) -> Result<Self, DataError> {
        let data = Self {
            movement: load_file(files, MOVEMENT_FILE, MovementConfig::validate)?,
            simulation: load_file(files, SIMULATION_FILE, SimulationConfig::validate)?,
            physics: load_file(files, PHYSICS_FILE, PhysicsConfig::validate)?,
            manipulator: load_file(files, MANIPULATOR_FILE, ManipulatorConfig::validate)?,
            ammo: load_file(files, AMMO_FILE, AmmoConfig::validate)?,
            weapons: load_dir(files, WEAPONS_DIR, WeaponConfig::validate)?,
            items: load_dir(files, ITEMS_DIR, ItemConfig::validate)?,
            entity_map: load_file(files, ENTITY_MAP_FILE, EntityMapConfig::validate)?,
            player: load_file(files, PLAYER_FILE, PlayerConfig::validate)?,
            rules: load_file(files, DEATHMATCH_FILE, RulesConfig::validate)?,
            hud: load_file(files, HUD_FILE, HudConfig::validate)?,
            net: load_file(files, NET_FILE, NetConfig::validate)?,
            map_logic: load_file(files, MAP_LOGIC_FILE, MapLogicConfig::validate)?,
        };
        let problems = data.reference_problems();
        if problems.is_empty() {
            Ok(data)
        } else {
            Err(DataError::Invalid {
                file: "references".to_owned(),
                problems,
            })
        }
    }

    /// Every reference from one file to something another file should
    /// define but does not.
    fn reference_problems(&self) -> Vec<Problem> {
        let mut check = validate::Validator::default();
        let ammo = |check: &mut validate::Validator, field: String, id: &str| {
            if !self.ammo.contains(id) {
                check.fail(&field, &format!("no ammo type \"{id}\" in {AMMO_FILE}"));
            }
        };
        let weapon = |check: &mut validate::Validator, field: String, id: &str| {
            if !self.weapons.contains_key(id) {
                check.fail(&field, &format!("no weapon \"{id}\" in {WEAPONS_DIR}/"));
            }
        };
        for (id, config) in &self.weapons {
            for ammo_type in config.ammo_types() {
                ammo(&mut check, format!("{WEAPONS_DIR}/{id}.ron"), ammo_type);
            }
            if config.pickup.as_ref().is_some_and(|pickup| pickup.ammo > 0)
                && config.ammo_type().is_none()
            {
                check.fail(
                    &format!("{WEAPONS_DIR}/{id}.ron: pickup.ammo"),
                    "the weapon uses no ammo to give",
                );
            }
        }
        for (id, item) in &self.items {
            if let ItemEffect::Ammo(rounds) = &item.gives {
                for ammo_type in rounds.keys() {
                    ammo(
                        &mut check,
                        format!("{ITEMS_DIR}/{id}.ron: gives"),
                        ammo_type,
                    );
                }
            }
        }
        for (classname, mapped) in &self.entity_map.classes {
            let field = format!("{ENTITY_MAP_FILE}: classes.{classname}");
            match mapped {
                MappedEntity::Weapon(id) => {
                    weapon(&mut check, field, id);
                    if self.weapons.get(id).is_some_and(|w| w.pickup.is_none()) {
                        check.fail(
                            &format!("{ENTITY_MAP_FILE}: classes.{classname}"),
                            &format!("weapon \"{id}\" has no pickup"),
                        );
                    }
                }
                MappedEntity::Item(id) if !self.items.contains_key(id) => {
                    check.fail(&field, &format!("no item \"{id}\" in {ITEMS_DIR}/"));
                }
                MappedEntity::Item(_) | MappedEntity::Ignore => {}
            }
        }
        let loadout = &self.rules.loadout;
        for id in &loadout.weapons {
            weapon(
                &mut check,
                format!("{DEATHMATCH_FILE}: loadout.weapons"),
                id,
            );
        }
        for ammo_type in loadout.ammo.keys() {
            ammo(
                &mut check,
                format!("{DEATHMATCH_FILE}: loadout.ammo"),
                ammo_type,
            );
        }
        check.finish()
    }
}

/// A fingerprint of a data set written out by [`GameData::to_files`]: equal
/// sets hash equally, so a client holding the server's data need not be
/// sent it again.
///
/// The hash is 64-bit FNV-1a over every path and text in path order. It
/// guards against accidental mismatches, not deliberate ones: the server
/// validates nothing a client claims, it only skips sending data the
/// client's hash says it has.
#[must_use]
pub fn content_hash(files: &BTreeMap<String, String>) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0100_0000_01b3;
    let mut hash = OFFSET;
    for (path, text) in files {
        // A separator that cannot occur in either keeps ("ab", "c") and
        // ("a", "bc") apart.
        for byte in path.bytes().chain([0]).chain(text.bytes()).chain([0]) {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(PRIME);
        }
    }
    hash
}

/// Loads, parses and validates one data file.
fn load_file<T: DeserializeOwned>(
    files: Files<'_>,
    name: &str,
    validate: fn(&T, &mut validate::Validator),
) -> Result<T, DataError> {
    let dir = match files {
        Files::Overlay(dir) => dir,
        Files::Memory(set) => {
            let text = set.get(name).ok_or_else(|| DataError::Missing {
                file: name.to_owned(),
            })?;
            return parse(&format!("server:{name}"), text, validate);
        }
    };
    let (file, text) = match dir.map(|dir| dir.join(name)) {
        Some(path) if path.exists() => {
            let text = std::fs::read_to_string(&path).map_err(|source| DataError::Io {
                path: path.clone(),
                source,
            })?;
            (path.display().to_string(), text)
        }
        _ => (format!("builtin:{name}"), builtin_text(name).to_owned()),
    };
    parse(&file, &text, validate)
}

/// Loads every `.ron` file of directory `subdir`: the built-in ones plus
/// any the data directory adds, keyed by file name without extension.
fn load_dir<T: DeserializeOwned>(
    files: Files<'_>,
    subdir: &str,
    validate: fn(&T, &mut validate::Validator),
) -> Result<BTreeMap<String, T>, DataError> {
    let prefix = format!("{subdir}/");
    let names = match files {
        Files::Overlay(dir) => overlay_names(dir, subdir)?,
        Files::Memory(set) => set
            .keys()
            .filter_map(|path| path.strip_prefix(&prefix))
            .filter(|name| {
                !name.contains('/') && Path::new(name).extension().is_some_and(|ext| ext == "ron")
            })
            .map(str::to_owned)
            .collect(),
    };
    names
        .into_iter()
        .map(|name| {
            let id = name.trim_end_matches(".ron").to_owned();
            let file = format!("{prefix}{name}");
            if !is_valid_id(&id) {
                return Err(DataError::Invalid {
                    file,
                    problems: vec![Problem {
                        field: "file name".to_owned(),
                        message: "ids are lower-case letters, digits and underscores".to_owned(),
                    }],
                });
            }
            Ok((id, load_file(files, &file, validate)?))
        })
        .collect()
}

/// The `.ron` file names of directory `subdir`: the built-in ones plus any
/// the overriding directory adds.
fn overlay_names(dir: Option<&Path>, subdir: &str) -> Result<BTreeSet<String>, DataError> {
    let prefix = format!("{subdir}/");
    let mut names: BTreeSet<String> = BUILTIN
        .iter()
        .filter_map(|(name, _)| name.strip_prefix(&prefix))
        .map(str::to_owned)
        .collect();
    if let Some(path) = dir.map(|dir| dir.join(subdir))
        && path.is_dir()
    {
        let entries = std::fs::read_dir(&path).map_err(|source| DataError::Io {
            path: path.clone(),
            source,
        })?;
        for entry in entries {
            let entry = entry.map_err(|source| DataError::Io {
                path: path.clone(),
                source,
            })?;
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "ron") {
                names.insert(entry.file_name().to_string_lossy().into_owned());
            }
        }
    }
    Ok(names)
}

/// Whether `id` is a valid weapon or item id: lower-case ASCII letters,
/// digits and underscores.
fn is_valid_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// Parses and validates `text`, attributing errors to `file`.
fn parse<T: DeserializeOwned>(
    file: &str,
    text: &str,
    validate: fn(&T, &mut validate::Validator),
) -> Result<T, DataError> {
    let value: T = ron::from_str(text).map_err(|source| DataError::Parse {
        file: file.to_owned(),
        source: Box::new(source),
    })?;
    let mut check = validate::Validator::default();
    validate(&value, &mut check);
    let problems = check.finish();
    if problems.is_empty() {
        Ok(value)
    } else {
        Err(DataError::Invalid {
            file: file.to_owned(),
            problems,
        })
    }
}

/// The built-in text of data file `name` (empty for an unknown name, which
/// then fails to parse with a clear error).
fn builtin_text(name: &str) -> &'static str {
    BUILTIN
        .iter()
        .find(|(file, _)| *file == name)
        .map_or("", |(_, text)| text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_data_is_valid() {
        let data = GameData::builtin().expect("the repository's data/ must be valid");
        assert_eq!(data.simulation.tick_rate, 66);
        assert!(data.movement.jump.launch_speed(data.movement.gravity) > 0.0);
        assert!(!data.physics.materials.surfaces.is_empty());
        assert!(data.manipulator.grab.range > 0.0);
        assert!(data.weapons.contains_key("manipulator"));
        assert!(!data.items.is_empty());
    }

    #[test]
    fn every_file_in_the_data_directory_is_built_in() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data");
        let mut found = Vec::new();
        let mut pending = vec![root.clone()];
        while let Some(dir) = pending.pop() {
            for entry in std::fs::read_dir(&dir).expect("read data/") {
                let path = entry.expect("entry").path();
                if path.is_dir() {
                    pending.push(path);
                } else if path.extension().is_some_and(|ext| ext == "ron") {
                    let relative = path.strip_prefix(&root).expect("under data/");
                    found.push(relative.to_string_lossy().replace('\\', "/"));
                }
            }
        }
        for name in &found {
            assert!(
                BUILTIN.iter().any(|(file, _)| file == name),
                "data/{name} is missing from BUILTIN"
            );
        }
        assert_eq!(found.len(), BUILTIN.len(), "BUILTIN lists a missing file");
    }

    #[test]
    fn builtin_impact_damage_matches_its_documented_example() {
        let data = GameData::builtin().expect("builtin");
        // physics.ron: a 30 kg barrel at 1000 units/s deals about 110.
        let damage = data.physics.impact_damage.damage(30.0, 1000.0);
        assert!((100.0..120.0).contains(&damage), "{damage}");
        // manipulator.ron: that barrel leaves at about 1000 units/s.
        let speed = data.manipulator.launch.speed_for(30.0);
        assert!((950.0..1050.0).contains(&speed), "{speed}");
    }

    #[test]
    fn data_files_in_subdirectories_override_too() {
        let dir = tempfile::tempdir().expect("tempdir");
        let tools = dir.path().join("tools");
        std::fs::create_dir(&tools).expect("mkdir");
        let mut manipulator = GameData::builtin().expect("builtin").manipulator;
        manipulator.grab.max_mass = 1.0;
        std::fs::write(
            tools.join("manipulator.ron"),
            ron::to_string(&manipulator).expect("serialize"),
        )
        .expect("write");
        let data = GameData::load(Some(dir.path())).expect("load");
        assert_eq!(data.manipulator.grab.max_mass, 1.0);
    }

    #[test]
    fn a_data_directory_overrides_files_it_contains() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join(SIMULATION_FILE),
            "Simulation(tick_rate: 100)",
        )
        .expect("write");
        let data = GameData::load(Some(dir.path())).expect("load");
        assert_eq!(data.simulation.tick_rate, 100);
        assert_eq!(
            data.movement,
            GameData::builtin().expect("builtin").movement
        );
    }

    #[test]
    fn new_weapon_files_add_weapons() {
        let builtin = GameData::builtin().expect("builtin");
        let dir = tempfile::tempdir().expect("tempdir");
        let weapons = dir.path().join(WEAPONS_DIR);
        std::fs::create_dir(&weapons).expect("mkdir");
        let mut nailgun = builtin.weapons["smg"].clone();
        nailgun.name = "Nailgun".to_owned();
        std::fs::write(
            weapons.join("nailgun.ron"),
            ron::to_string(&nailgun).expect("serialize"),
        )
        .expect("write");
        let data = GameData::load(Some(dir.path())).expect("load");
        assert_eq!(data.weapons.len(), builtin.weapons.len() + 1);
        assert_eq!(data.weapons["nailgun"].name, "Nailgun");
    }

    #[test]
    fn badly_named_weapon_files_are_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let weapons = dir.path().join(WEAPONS_DIR);
        std::fs::create_dir(&weapons).expect("mkdir");
        std::fs::write(weapons.join("Nail Gun.ron"), "").expect("write");
        let error = GameData::load(Some(dir.path())).expect_err("bad id");
        assert!(matches!(error, DataError::Invalid { .. }), "{error}");
    }

    #[test]
    fn dangling_references_are_reported_with_their_files() {
        let mut data = GameData::builtin().expect("builtin");
        data.ammo.types.remove("buckshot");
        data.rules.loadout.weapons.push("bfg".to_owned());
        data.entity_map.classes.insert(
            "item_cake".to_owned(),
            MappedEntity::Item("cake".to_owned()),
        );
        let fields: Vec<String> = data
            .reference_problems()
            .into_iter()
            .map(|problem| problem.field)
            .collect();
        assert!(
            fields.contains(&"weapons/scattergun.ron".to_owned()),
            "{fields:?}"
        );
        assert!(
            fields.contains(&"items/shells.ron: gives".to_owned()),
            "{fields:?}"
        );
        assert!(
            fields.contains(&"rules/deathmatch.ron: loadout.weapons".to_owned()),
            "{fields:?}"
        );
        assert!(
            fields.contains(&"entity_map.ron: classes.item_cake".to_owned()),
            "{fields:?}"
        );
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let error = parse::<SimulationConfig>(
            "test",
            "Simulation(tick_rate: 66, tickrate: 1)",
            SimulationConfig::validate,
        )
        .expect_err("typo");
        assert!(matches!(error, DataError::Parse { .. }), "{error}");
    }

    #[test]
    fn invalid_values_report_every_problem() {
        let mut movement = GameData::builtin().expect("builtin").movement;
        movement.gravity = -1.0;
        movement.hulls.ducked.maxs[2] = 100.0;
        movement.hulls.standing.mins[2] = 1.0;
        let text = ron::to_string(&movement).expect("serialize");
        let Err(DataError::Invalid { problems, .. }) =
            parse::<MovementConfig>("test", &text, MovementConfig::validate)
        else {
            panic!("invalid movement data accepted");
        };
        let fields: Vec<&str> = problems.iter().map(|p| p.field.as_str()).collect();
        assert!(fields.contains(&"gravity"), "{fields:?}");
        assert!(fields.contains(&"hulls.standing.mins"), "{fields:?}");
        assert!(fields.contains(&"hulls.ducked.maxs"), "{fields:?}");
    }

    #[test]
    fn tick_interval_is_the_reciprocal_rate() {
        let simulation = SimulationConfig { tick_rate: 50 };
        assert!((simulation.tick_interval() - 0.02).abs() < 1e-7);
        let mut check = validate::Validator::default();
        SimulationConfig { tick_rate: 0 }.validate(&mut check);
        assert_eq!(check.finish().len(), 1);
    }

    #[test]
    fn data_round_trips_through_files() {
        let data = GameData::builtin().expect("builtin");
        let files = data.to_files().expect("to files");
        assert!(files.contains_key("weapons/smg.ron"), "{:?}", files.keys());
        assert!(files.contains_key(NET_FILE));
        assert_eq!(GameData::from_files(&files).expect("from files"), data);
    }

    #[test]
    fn a_data_set_in_memory_must_be_complete() {
        let mut files = GameData::builtin()
            .expect("builtin")
            .to_files()
            .expect("files");
        files.remove(PLAYER_FILE);
        let error = GameData::from_files(&files).expect_err("incomplete");
        assert!(matches!(error, DataError::Missing { ref file } if file == PLAYER_FILE));
    }

    #[test]
    fn a_data_set_in_memory_is_validated() {
        let mut files = GameData::builtin()
            .expect("builtin")
            .to_files()
            .expect("files");
        files.insert(
            SIMULATION_FILE.to_owned(),
            "Simulation(tick_rate: 0)".to_owned(),
        );
        let error = GameData::from_files(&files).expect_err("invalid");
        assert!(
            matches!(error, DataError::Invalid { ref file, .. } if file == "server:simulation.ron"),
            "{error}"
        );
    }

    #[test]
    fn the_content_hash_tells_data_sets_apart() {
        let data = GameData::builtin().expect("builtin");
        let files = data.to_files().expect("files");
        assert_eq!(content_hash(&files), content_hash(&files.clone()));
        let mut tweaked = data.clone();
        tweaked.movement.gravity += 1.0;
        let other = tweaked.to_files().expect("files");
        assert_ne!(content_hash(&files), content_hash(&other));
        // Moving a byte between path and text changes the hash.
        let split = |path: &str, text: &str| {
            content_hash(&BTreeMap::from([(path.to_owned(), text.to_owned())]))
        };
        assert_ne!(split("ab", "c"), split("a", "bc"));
    }

    #[test]
    fn ids_are_lower_case_words() {
        assert!(is_valid_id("smg_2"));
        assert!(!is_valid_id("SMG"));
        assert!(!is_valid_id("nail gun"));
        assert!(!is_valid_id(""));
    }
}
