//! Maps as the simulation sees them, loaded from the mounted content without
//! rendering anything.
//!
//! A dedicated server draws nothing, yet needs everything of a map that
//! gameplay touches: its collision, where players spawn, its entities (for
//! pickups and hazards) and a physics body for every solid prop. [`load`]
//! reads exactly that from the map's BSP and the props' models — just each
//! `.mdl` header and its `.phy`, never meshes or textures — and
//! [`Level::simulation`] builds the [`Simulation`].
//!
//! The server and its clients both build their simulations this way, so
//! the props get the same [`BodyId`]s on every machine (one per solid prop,
//! in the map's placement order): a snapshot can name a prop by its body
//! and every client knows which one is meant. The client draws each prop
//! from the same placement list ([`Level::props`] follows the order of
//! `source_formats::bsp::MapGeometry::props`).
//!
//! Models a map packs into its own pakfile are found there first, as the
//! engine finds them, without mounting the pakfile on the shared [`Vfs`].

#![forbid(unsafe_code)]

pub mod maps;

use std::collections::HashMap;
use std::sync::Arc;

use glam::Vec3;
use rl_data::{GameData, PhysicsConfig};
use rl_sim::collision::CollisionWorld;
use rl_sim::physics::{BodyId, Breakable, GibPiece, PropDesc, map_joints, prop_desc};
use rl_sim::simulation::{Simulation, Spawn};
use source_formats::bsp::{self, BspError};
use source_formats::entities::MapEntity;
use source_formats::mdl;
use source_formats::phy::{self, PhysicsModel};
use source_formats::propdata::PropDataTable;
use source_formats::props::{PropKind, PropPlacement};
use source_fs::{Mount, MountOrigin, Vfs, ZipMount};
use thiserror::Error;

/// Errors from [`load`]. Each names the map.
#[derive(Debug, Error)]
pub enum LevelError {
    /// No `maps/<name>.bsp` in the mounted content.
    #[error("map {map} not found")]
    NotFound {
        /// The map's name.
        map: String,
    },
    /// The BSP exists but could not be read.
    #[error("cannot read map {map}: {source}")]
    Io {
        /// The map's name.
        map: String,
        /// Underlying error.
        #[source]
        source: source_fs::Error,
    },
    /// The BSP is not a readable map, or has no collision model.
    #[error("map {map}: {source}")]
    Bsp {
        /// The map's name.
        map: String,
        /// Parser error.
        #[source]
        source: BspError,
    },
}

/// A solid prop of a level and the body it gets.
#[derive(Debug, Clone)]
pub struct LevelProp {
    /// Where the map placed it, and how it collides.
    pub placement: PropPlacement,
    /// Its physics body, or `None` for props nothing collides with (or
    /// whose model could not be read).
    pub desc: Option<PropDesc>,
}

/// Everything of a map that gameplay touches.
#[derive(Debug, Clone)]
pub struct Level {
    /// The map's name, without path or extension.
    pub name: String,
    /// CRC-32 of the BSP file: a client must have the very same map.
    pub checksum: u32,
    /// Where the BSP was found (a workshop item tells players where to get
    /// it); `None` for levels built in memory.
    pub origin: Option<MountOrigin>,
    /// The map's static collision.
    pub world: Arc<CollisionWorld>,
    /// Where players spawn.
    pub spawns: Vec<Spawn>,
    /// Every entity, for pickups and hazards.
    pub entities: Vec<MapEntity>,
    /// Every prop placement, in the map's order.
    pub props: Vec<LevelProp>,
}

/// A level's simulation and which body each prop got.
#[derive(Debug)]
pub struct LevelSimulation {
    /// The simulation, with spawns, pickups, hazards and props in place and
    /// no players yet.
    pub simulation: Simulation,
    /// Each of [`Level::props`]' body, by placement index.
    pub bodies: Vec<Option<BodyId>>,
}

impl Level {
    /// Builds the level's simulation under `data`: spawn points, the
    /// entities' pickups and hazards, every solid prop's body, added in
    /// placement order so the bodies are numbered alike on every machine,
    /// and the joints the map's constraints hold them by.
    #[must_use]
    pub fn simulation(&self, data: &GameData) -> LevelSimulation {
        let mut simulation = Simulation::new(Arc::clone(&self.world), data);
        simulation.set_spawns(self.spawns.clone());
        simulation.add_entities(&self.entities);
        let bodies: Vec<Option<BodyId>> = self
            .props
            .iter()
            .map(|prop| prop.desc.as_ref().map(|desc| simulation.add_prop(desc)))
            .collect();
        let joints = map_joints(&self.entities, |name| {
            self.props
                .iter()
                .zip(&bodies)
                .find(|(prop, _)| prop.placement.name.as_deref() == Some(name))
                .and_then(|(_, body)| *body)
        });
        for skipped in &joints.skipped {
            tracing::debug!("constraint left out: {skipped}");
        }
        let joined = joints
            .joints
            .iter()
            .filter(|joint| simulation.add_joint(joint))
            .count();
        if joined > 0 {
            tracing::debug!(joined, "joined the map's props");
        }
        simulation.refresh_props();
        LevelSimulation { simulation, bodies }
    }
}

/// The CRC-32 a [`Level`] records of its BSP.
#[must_use]
pub fn checksum(bsp: &[u8]) -> u32 {
    crc32fast::hash(bsp)
}

/// Loads map `name` (without path or extension) from `vfs`: its collision,
/// spawns, entities and prop bodies built under `physics`' materials.
///
/// # Errors
///
/// Fails if the map is missing or unreadable, or has no collision model.
/// A prop whose model cannot be read is logged and left without a body.
pub fn load(vfs: &Vfs, name: &str, physics: &PhysicsConfig) -> Result<Level, LevelError> {
    let path = format!("maps/{name}.bsp");
    let bytes = vfs
        .read(&path)
        .map_err(|source| LevelError::Io {
            map: name.to_owned(),
            source,
        })?
        .ok_or_else(|| LevelError::NotFound {
            map: name.to_owned(),
        })?;
    let bsp_error = |source| LevelError::Bsp {
        map: name.to_owned(),
        source,
    };
    let collision = source_formats::collision::load(&bytes).map_err(bsp_error)?;
    let contents = bsp::load_contents(&bytes).map_err(bsp_error)?;
    let pakfile = bsp::pakfile_bytes(&bytes)
        .and_then(|pak| ZipMount::from_bytes(format!("pakfile of {name}"), pak).ok());
    let files = Files { vfs, pakfile };
    let propdata = files
        .read("scripts/propdata.txt")
        .map(|bytes| PropDataTable::parse(&String::from_utf8_lossy(&bytes)))
        .unwrap_or_default();
    let mut models = HashMap::new();
    let props = contents
        .props
        .into_iter()
        .map(|placement| {
            let model = models
                .entry(placement.model.clone())
                .or_insert_with(|| files.model(&placement.model));
            let desc = model
                .as_ref()
                .and_then(|model| model.desc(&placement, physics, &propdata));
            LevelProp { placement, desc }
        })
        .collect::<Vec<_>>();
    let level = Level {
        name: name.to_owned(),
        checksum: checksum(&bytes),
        origin: vfs.locate(&path),
        world: Arc::new(CollisionWorld::new(collision)),
        spawns: contents.spawns.iter().map(spawn).collect(),
        entities: contents.entities,
        props,
    };
    tracing::info!(
        map = name,
        props = level.props.len(),
        bodies = level
            .props
            .iter()
            .filter(|prop| prop.desc.is_some())
            .count(),
        breakable = level
            .props
            .iter()
            .filter(|prop| prop
                .desc
                .as_ref()
                .is_some_and(|desc| desc.breakable.is_some()))
            .count(),
        spawns = level.spawns.len(),
        "loaded level"
    );
    Ok(level)
}

/// A spawn point in the simulation's terms.
fn spawn(point: &bsp::SpawnPoint) -> Spawn {
    Spawn {
        origin: Vec3::from(point.origin),
        yaw: point.yaw,
    }
}

/// What a prop's body is built from: its model's header and collision
/// model.
#[derive(Debug, Clone)]
pub struct PropModel {
    /// The model's bounds, Source space: the collision box of models
    /// without a physics model.
    pub bounds: ([f32; 3], [f32; 3]),
    /// The model's `$surfaceprop`, lower-cased.
    pub surface_prop: String,
    /// The model's `.phy`, if it ships a readable one.
    pub physics: Option<PhysicsModel>,
    /// The model's `$keyvalues` text, which may say how it breaks.
    pub key_values: Option<String>,
}

impl PropModel {
    /// The body of a prop of this model placed by `placement`, if it is
    /// solid, with what `propdata` says about how it breaks.
    #[must_use]
    pub fn desc(
        &self,
        placement: &PropPlacement,
        physics: &PhysicsConfig,
        propdata: &PropDataTable,
    ) -> Option<PropDesc> {
        let mut desc = prop_desc(
            placement,
            self.physics.as_ref(),
            self.bounds,
            &self.surface_prop,
            physics,
        )?;
        desc.breakable = self.breakable(placement, propdata);
        Some(desc)
    }

    /// How a prop of this model placed by `placement` breaks: its `health`
    /// from the entity, else from the model's `prop_data`; no health, no
    /// breaking. Only physics props break.
    fn breakable(&self, placement: &PropPlacement, propdata: &PropDataTable) -> Option<Breakable> {
        if placement.kind != PropKind::Physics {
            return None;
        }
        let data = self
            .key_values
            .as_deref()
            .and_then(|text| propdata.for_model(text))
            .unwrap_or_default();
        let health = if placement.health > 0.0 {
            placement.health
        } else {
            data.health
        };
        if health <= 0.0 {
            return None;
        }
        let damage = if placement.explode_damage > 0.0 {
            placement.explode_damage
        } else {
            data.explosive_damage
        };
        let radius = if placement.explode_radius > 0.0 {
            placement.explode_radius
        } else {
            data.explosive_radius
        };
        let gibs = data
            .breakable_model
            .as_deref()
            .map(|list| propdata.gibs(list).to_vec())
            .unwrap_or_default();
        let pieces = self
            .physics
            .as_ref()
            .map(|physics| {
                physics
                    .breaks
                    .iter()
                    .map(|piece| GibPiece {
                        model: piece.model.clone(),
                        fade: piece.fade_time,
                        burst: piece.burst,
                    })
                    .collect()
            })
            .unwrap_or_default();
        Some(Breakable {
            health,
            pieces,
            bullets: data.bullets,
            club: data.club,
            explosive: data.explosive,
            explosion: (damage > 0.0 && radius > 0.0).then_some((damage, radius)),
            gibs,
            gib_count: data.breakable_count,
            respawn: (placement.respawn > 0.0).then_some(placement.respawn),
        })
    }
}

/// The content a level's files come from: the map's pakfile, then the
/// mounted game.
struct Files<'a> {
    vfs: &'a Vfs,
    pakfile: Option<ZipMount>,
}

impl Files<'_> {
    /// Reads `path`, from the pakfile first.
    fn read(&self, path: &str) -> Option<Vec<u8>> {
        let packed = self
            .pakfile
            .as_ref()
            .and_then(|pak| pak.read(path).ok().flatten());
        packed.or_else(|| self.vfs.read(path).ok().flatten())
    }

    /// The model at `path` (`models/….mdl`), or `None` (logged) if it is
    /// missing or unreadable.
    fn model(&self, path: &str) -> Option<PropModel> {
        let Some(bytes) = self.read(path) else {
            tracing::warn!(model = path, "prop model not found; the prop has no body");
            return None;
        };
        let header = match mdl::read_header(&bytes) {
            Ok(header) => header,
            Err(error) => {
                tracing::warn!(model = path, %error, "unreadable prop model; the prop has no body");
                return None;
            }
        };
        let phy_path = format!("{}.phy", path.trim_end_matches(".mdl"));
        let physics = self.read(&phy_path).and_then(|bytes| {
            phy::decode(&bytes)
                .inspect_err(|error| tracing::debug!(model = path, %error, "unreadable .phy"))
                .ok()
        });
        Some(PropModel {
            bounds: header.bounds,
            surface_prop: header.surface_prop.to_ascii_lowercase(),
            physics,
            key_values: header.key_values,
        })
    }
}
