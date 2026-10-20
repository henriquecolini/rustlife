//! Model placements in a map: static props and `prop_*` entities.
//!
//! A Source map places models in two ways:
//!
//! - **Static props** live in the `sprp` game lump. They never move, are
//!   compiled into the map by `vbsp`, and make up most of a level's detail
//!   (crates, pipes, railings, foliage).
//! - **Prop entities** (`prop_dynamic`, `prop_physics`, ...) live in the
//!   entity lump. They can animate, move or be knocked about, so the
//!   simulation owns them from M4 on; for now they are drawn where the mapper
//!   placed them.
//!
//! Both become a [`PropPlacement`] in Source space; turning the placement into
//! a transform happens at the render boundary.

use vbsp::{Bsp, StaticPropLumpFlags};

use crate::bsp::parse_vector;

/// How a placed model behaves in the game.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PropKind {
    /// A static prop from the `sprp` game lump: never moves.
    Static,
    /// A `prop_dynamic`-style entity: may animate or be moved by map logic,
    /// but is not simulated physically.
    Dynamic,
    /// A `prop_physics`-style entity: simulated as a rigid body (from M4).
    Physics,
}

/// One model placed in a map, in Source space.
#[derive(Debug, Clone, PartialEq)]
pub struct PropPlacement {
    /// Model path as stored in the map, normalised to forward slashes and
    /// lower case, e.g. `models/props_c17/oildrum001.mdl`.
    pub model: String,
    /// Model origin (Source units, Z-up).
    pub origin: [f32; 3],
    /// Orientation as Source Euler angles in degrees: `[pitch, yaw, roll]`.
    pub angles: [f32; 3],
    /// Skin (material set) index into the model's skin table.
    pub skin: usize,
    /// Uniform scale (`modelscale` on entities; static props are unscaled).
    pub scale: f32,
    /// Whether this is a static prop or which kind of entity.
    pub kind: PropKind,
    /// Index of the prop within the `sprp` lump for static props; `None` for
    /// entities.
    pub static_index: Option<usize>,
    /// Pakfile path of the prop's baked per-vertex lighting
    /// (`sp_<index>.vhv`, see [`crate::vhv`]); `None` for entities and for
    /// static props compiled without it.
    pub vertex_lighting: Option<String>,
    /// Point the prop should be lit from, when the mapper overrode it; when
    /// `None` the renderer lights the prop from its own centre.
    pub lighting_origin: Option<[f32; 3]>,
}

/// Entity `rendermode` meaning "don't render" (`kRenderNone`).
const RENDER_MODE_NONE: &str = "10";

/// Every drawable static prop and prop entity of `bsp`.
///
/// Static props flagged `NO_DRAW` and entities with `rendermode 10` are
/// skipped, as are entries whose model name is not an `.mdl` path.
pub(crate) fn props(bsp: &Bsp) -> Vec<PropPlacement> {
    let mut props = static_props(bsp);
    props.extend(entity_props(bsp));
    props
}

fn static_props(bsp: &Bsp) -> Vec<PropPlacement> {
    let names = &bsp.static_props.dict.name;
    bsp.static_props
        .props
        .props
        .iter()
        .enumerate()
        .filter(|(_, prop)| !prop.flags.contains(StaticPropLumpFlags::NO_DRAW))
        .filter_map(|(index, prop)| {
            // Index the dictionary ourselves: `vbsp`'s `model()` handle
            // accessor panics on an out-of-range `prop_type`.
            let name = names.get(usize::from(prop.prop_type))?.as_str();
            let model = normalize_model(name)?;
            Some(PropPlacement {
                model,
                origin: [prop.origin.x, prop.origin.y, prop.origin.z],
                angles: [prop.angles.pitch, prop.angles.yaw, prop.angles.roll],
                skin: usize::try_from(prop.skin).unwrap_or(0),
                scale: 1.0,
                kind: PropKind::Static,
                static_index: Some(index),
                vertex_lighting: (!prop
                    .flags
                    .contains(StaticPropLumpFlags::NO_PER_VERTEX_LIGHTING))
                .then(|| format!("sp_{index}.vhv")),
                lighting_origin: prop
                    .flags
                    .contains(StaticPropLumpFlags::USE_LIGHTING_ORIGIN)
                    .then_some([
                        prop.lighting_origin.x,
                        prop.lighting_origin.y,
                        prop.lighting_origin.z,
                    ]),
            })
        })
        .collect()
}

fn entity_props(bsp: &Bsp) -> Vec<PropPlacement> {
    bsp.entities
        .iter()
        .filter_map(|entity| {
            let kind = entity_kind(entity.prop("classname")?)?;
            if entity.prop("rendermode") == Some(RENDER_MODE_NONE) {
                return None;
            }
            let model = normalize_model(entity.prop("model")?)?;
            let origin = entity
                .prop("origin")
                .and_then(parse_vector)
                .unwrap_or_default();
            let angles = entity
                .prop("angles")
                .and_then(parse_vector)
                .unwrap_or_default();
            let skin = entity
                .prop("skin")
                .and_then(|skin| skin.trim().parse().ok())
                .unwrap_or(0);
            let scale = entity
                .prop("modelscale")
                .and_then(|scale| scale.trim().parse::<f32>().ok())
                .filter(|scale| scale.is_finite() && *scale > 0.0)
                .unwrap_or(1.0);
            Some(PropPlacement {
                model,
                origin,
                angles,
                skin,
                scale,
                kind,
                static_index: None,
                vertex_lighting: None,
                lighting_origin: None,
            })
        })
        .collect()
}

/// The [`PropKind`] of an entity class, or `None` if it does not place a
/// model the renderer should draw as a prop.
fn entity_kind(classname: &str) -> Option<PropKind> {
    if !classname.starts_with("prop_") {
        return None;
    }
    // `prop_physics`, `prop_physics_multiplayer`, `prop_physics_override`,
    // `prop_physics_respawnable`, and ragdolls (simulated as one body until
    // ragdoll physics exists).
    if classname.contains("physics") || classname == "prop_ragdoll" {
        Some(PropKind::Physics)
    } else {
        // `prop_dynamic`, `prop_dynamic_override`, `prop_door_rotating`, ...
        Some(PropKind::Dynamic)
    }
}

/// Normalises a model path to forward slashes and lower case, or `None` if it
/// is not an `.mdl` path (brush models such as `*3` and sprites are drawn by
/// other means).
fn normalize_model(name: &str) -> Option<String> {
    let model = name.trim().replace('\\', "/").to_ascii_lowercase();
    model
        .strip_suffix(".mdl")
        .is_some_and(|stem| !stem.is_empty())
        .then_some(model)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_prop_entities() {
        assert_eq!(entity_kind("prop_physics"), Some(PropKind::Physics));
        assert_eq!(
            entity_kind("prop_physics_multiplayer"),
            Some(PropKind::Physics)
        );
        assert_eq!(entity_kind("prop_ragdoll"), Some(PropKind::Physics));
        assert_eq!(entity_kind("prop_dynamic"), Some(PropKind::Dynamic));
        assert_eq!(entity_kind("prop_door_rotating"), Some(PropKind::Dynamic));
        assert_eq!(entity_kind("func_brush"), None);
        assert_eq!(entity_kind("info_player_deathmatch"), None);
    }

    #[test]
    fn normalizes_model_paths() {
        assert_eq!(
            normalize_model("Models\\Props_C17\\OilDrum001.MDL").as_deref(),
            Some("models/props_c17/oildrum001.mdl")
        );
        assert_eq!(normalize_model("*3"), None, "brush model");
        assert_eq!(normalize_model("sprites/glow01.spr"), None);
        assert_eq!(normalize_model(".mdl"), None);
    }

    /// Lists the props of a real map. Set `RUSTLIFE_TEST_BSP`.
    #[test]
    #[ignore = "requires a BSP file; set RUSTLIFE_TEST_BSP"]
    fn reads_real_map_props() {
        let Ok(path) = std::env::var("RUSTLIFE_TEST_BSP") else {
            return;
        };
        let bytes = std::fs::read(&path).expect("read bsp");
        let bsp = Bsp::read(&bytes).expect("parse bsp");
        let props = props(&bsp);
        let count = |kind| props.iter().filter(|p| p.kind == kind).count();
        eprintln!(
            "{path}: {} static, {} dynamic, {} physics",
            count(PropKind::Static),
            count(PropKind::Dynamic),
            count(PropKind::Physics)
        );
        assert!(count(PropKind::Static) > 0, "a stock map has static props");
        assert!(
            props
                .iter()
                .all(|p| normalize_model(&p.model).as_deref() == Some(p.model.as_str())),
            "model paths are normalised"
        );
        assert!(
            props
                .iter()
                .all(|p| p.origin.iter().chain(&p.angles).all(|v| v.is_finite()))
        );
    }
}
