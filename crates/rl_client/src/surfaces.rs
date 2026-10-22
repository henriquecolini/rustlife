//! What the map's surfaces are made of: their `$surfaceprop`, and the
//! coarse class footsteps, decals and bullet effects share.
//!
//! The collision model names the material (texture) of every brush side and
//! displacement. The map's VMTs, read through the same mounted content the
//! renderer uses, say what each material is made of (`concrete`, `metal`,
//! `wood`, ...). Props carry their `$surfaceprop` in their physics data
//! already; this module only fills in the map's own surfaces.

use std::collections::HashMap;

use bevy::prelude::*;
use rl_assets::MapAsset;
use rl_sim::combat::hitscan::{ShotTrace, Struck};
use rl_sim::simulation::Simulation;
use source_formats::vmt;
use source_fs::Vfs;

use crate::AppState;
use crate::{GameVfs, world::MapHandle};

/// A surface's broad material class: what it sounds and looks like when
/// struck.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SurfaceClass {
    /// Concrete, cement, brick, plaster.
    Concrete,
    /// Ceramic floor and wall tiles.
    Tile,
    /// Metal, grates, vents, cans.
    Metal,
    /// Wood, cardboard, paper.
    Wood,
    /// Glass and bottles.
    Glass,
    /// Players and gore.
    Flesh,
    /// Dirt, sand, gravel, grass, rock.
    Dirt,
    /// Water and slime.
    Water,
}

impl SurfaceClass {
    /// The class's key in `effects.ron`, and the fallback `$surfaceprop`.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Concrete => "concrete",
            Self::Tile => "tile",
            Self::Metal => "metal",
            Self::Wood => "wood",
            Self::Glass => "glass",
            Self::Flesh => "flesh",
            Self::Dirt => "dirt",
            Self::Water => "water",
        }
    }
}

/// The class of a surface property or material name (`metal_barrel`,
/// `wood/woodfloor`, `Concrete`).
#[must_use]
pub fn classify(name: &str) -> SurfaceClass {
    let name = name.to_ascii_lowercase();
    let has = |part: &str| name.contains(part);
    if has("flesh") || has("blood") {
        SurfaceClass::Flesh
    } else if has("glass") {
        SurfaceClass::Glass
    } else if has("water") || has("wade") || has("slime") {
        SurfaceClass::Water
    } else if has("metal") || has("canister") || has("grate") || has("vent") || has("chainlink") {
        SurfaceClass::Metal
    } else if has("wood") || has("cardboard") || has("paper") {
        SurfaceClass::Wood
    } else if has("dirt")
        || has("sand")
        || has("gravel")
        || has("mud")
        || has("grass")
        || has("rock")
        || has("boulder")
        || has("carpet")
    {
        SurfaceClass::Dirt
    } else if has("tile") {
        SurfaceClass::Tile
    } else {
        SurfaceClass::Concrete
    }
}

/// What one of the map's materials is made of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SurfaceInfo {
    /// Its `$surfaceprop`, or its class's name when the VMT names none.
    pub prop: String,
    /// Its broad class.
    pub class: SurfaceClass,
}

impl SurfaceInfo {
    fn of(vfs: &Vfs, material: &str) -> Self {
        let prop = surface_prop(vfs, material)
            .filter(|prop| !prop.trim().is_empty())
            .unwrap_or_else(|| classify(material).name().to_owned());
        let class = classify(&prop);
        Self { prop, class }
    }
}

/// The map's collision materials, by the index a trace reports.
#[derive(Resource, Debug, Default)]
pub struct MapSurfaces {
    loaded: Option<AssetId<MapAsset>>,
    by_material: Vec<SurfaceInfo>,
}

impl MapSurfaces {
    /// What material `index` (a trace's `material`) is made of.
    #[must_use]
    pub fn get(&self, index: Option<u16>) -> Option<&SurfaceInfo> {
        self.by_material.get(usize::from(index?))
    }

    /// What surface a shot struck: the prop's own `$surfaceprop`, else the
    /// map material's, else `default`.
    #[must_use]
    pub fn of_shot<'a>(&'a self, sim: &'a Simulation, trace: &ShotTrace) -> &'a str {
        if let Struck::Prop(body) = trace.struck {
            return sim.physics().surface(body);
        }
        self.get(trace.material)
            .map_or("default", |info| info.prop.as_str())
    }
}

/// The class a prop's `$surfaceprop` names.
#[must_use]
pub fn class_of_prop(prop: &str) -> SurfaceClass {
    classify(prop)
}

/// Loads the map's material classes once its collision and VMTs are
/// available; leaves the previous map's classes in place until then.
pub struct SurfacesPlugin;

impl Plugin for SurfacesPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<MapSurfaces>()
            .add_systems(
                OnExit(AppState::InGame),
                |mut surfaces: ResMut<MapSurfaces>| {
                    *surfaces = MapSurfaces::default();
                },
            )
            .add_systems(Update, load.run_if(in_state(AppState::InGame)));
    }
}

fn load(
    handle: Option<Res<MapHandle>>,
    maps: Res<Assets<MapAsset>>,
    vfs: Res<GameVfs>,
    mut surfaces: ResMut<MapSurfaces>,
) {
    let Some((id, map)) = handle
        .as_ref()
        .and_then(|handle| Some((handle.0.id(), maps.get(&handle.0)?)))
    else {
        return;
    };
    if surfaces.loaded == Some(id) {
        return;
    }
    let Some(collision) = &map.collision else {
        return;
    };
    // Several texture-info entries share a material name; resolve each once.
    let mut cache: HashMap<&str, SurfaceInfo> = HashMap::new();
    let by_material: Vec<SurfaceInfo> = collision
        .materials
        .iter()
        .map(|material| {
            cache
                .entry(material.as_str())
                .or_insert_with(|| SurfaceInfo::of(&vfs.0, material))
                .clone()
        })
        .collect();
    tracing::debug!(
        materials = by_material.len(),
        "classified the map's surface materials"
    );
    surfaces.loaded = Some(id);
    surfaces.by_material = by_material;
}

/// A material's `$surfaceprop` from its VMT, following a patch to its base.
fn surface_prop(vfs: &Vfs, material: &str) -> Option<String> {
    let path = format!("materials/{}.vmt", material.replace('\\', "/"));
    let text = String::from_utf8_lossy(&vfs.read(&path).ok()??).into_owned();
    let desc = match vmt::patch_include(&text).ok()? {
        Some(include) => {
            let path = include.replace('\\', "/");
            let base = String::from_utf8_lossy(&vfs.read(&path).ok()??).into_owned();
            vmt::parse_resolved(&text, |_| Ok(base.clone())).ok()?
        }
        None => vmt::parse(&text).ok()?,
    };
    desc.surface_prop
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn surfaces_classify_by_their_names() {
        assert_eq!(classify("concrete"), SurfaceClass::Concrete);
        assert_eq!(
            classify("concrete/concretefloor001a"),
            SurfaceClass::Concrete
        );
        assert_eq!(classify("tile/tilefloor"), SurfaceClass::Tile);
        assert_eq!(classify("metal_barrel"), SurfaceClass::Metal);
        assert_eq!(classify("MetalGrate"), SurfaceClass::Metal);
        assert_eq!(classify("wood_crate"), SurfaceClass::Wood);
        assert_eq!(classify("cardboard"), SurfaceClass::Wood);
        assert_eq!(classify("glassbottle"), SurfaceClass::Glass);
        assert_eq!(classify("bloodyflesh"), SurfaceClass::Flesh);
        assert_eq!(classify("dirt"), SurfaceClass::Dirt);
        assert_eq!(classify("water"), SurfaceClass::Water);
        assert_eq!(classify("something unknown"), SurfaceClass::Concrete);
    }

    #[test]
    fn unknown_material_indices_fall_back_to_default() {
        let surfaces = MapSurfaces::default();
        assert!(surfaces.get(None).is_none());
        assert!(surfaces.get(Some(7)).is_none());
    }
}
