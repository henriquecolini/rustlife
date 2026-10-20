//! Static geometry from Source models (`.mdl` + `.dx90.vtx` + `.vvd`).
//!
//! A Source model is split across three files: the `.mdl` holds the skeleton,
//! materials and skins, the `.vvd` the vertices, and the `.vtx` the
//! hardware-optimised index strips per level of detail. `vmdl` reads all three;
//! this module flattens the highest-detail LOD into triangle lists grouped by
//! mesh, with the material table needed to texture them.
//!
//! Positions and normals stay in Source space (Z up, inches), in the model's
//! own frame. For models that are not flagged as static props, the model's
//! root bone transform is applied so that they rest the way they appear in the
//! game; animation proper arrives with M7.

use thiserror::Error;
use vmdl::{Mdl, Model, Vtx, Vvd};

/// One mesh of a model: a triangle list using one material slot.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelMesh {
    /// Index into each skin's material list ([`ModelGeometry::skins`]).
    pub material: usize,
    /// Triangle-list positions, Source space, model frame.
    pub positions: Vec<[f32; 3]>,
    /// Per-vertex normals, Source space.
    pub normals: Vec<[f32; 3]>,
    /// Per-vertex texture coordinates.
    pub uvs: Vec<[f32; 2]>,
}

/// The renderable contents of a model.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelGeometry {
    /// Meshes of the highest level of detail.
    pub meshes: Vec<ModelMesh>,
    /// Material names per skin: `skins[skin][material_slot]`, e.g.
    /// `"oildrum001"`. Skin 0 is the default; a prop picks another by index.
    pub skins: Vec<Vec<String>>,
    /// Directories (relative to `materials/`) searched in order for the
    /// material names, from the model's `$cdmaterials`.
    pub material_dirs: Vec<String>,
    /// Axis-aligned bounds `(min, max)` from the model header.
    pub bounds: ([f32; 3], [f32; 3]),
    /// The model's `$surfaceprop`, used for physics and impact sounds.
    pub surface_prop: String,
}

impl ModelGeometry {
    /// Candidate VMT paths for `material` in `skin` (falling back to skin 0),
    /// in search order: `materials/<dir><name>.vmt` for each material
    /// directory. The first that exists is the material to use.
    #[must_use]
    pub fn material_candidates(&self, skin: usize, material: usize) -> Vec<String> {
        let Some(name) = self
            .skins
            .get(skin)
            .or_else(|| self.skins.first())
            .and_then(|skin| skin.get(material))
        else {
            return Vec::new();
        };
        let name = name.replace('\\', "/");
        self.material_dirs
            .iter()
            .map(|dir| {
                let dir = dir.replace('\\', "/");
                let dir = dir.trim_matches('/');
                if dir.is_empty() {
                    format!("materials/{name}.vmt")
                } else {
                    format!("materials/{dir}/{name}.vmt")
                }
            })
            .collect()
    }
}

/// Errors from [`load_model`].
#[derive(Debug, Error)]
pub enum ModelError {
    /// One of the three files is not valid, or they do not belong together.
    #[error("could not read model: {0}")]
    Parse(String),
}

/// Reads a model from its `.mdl`, `.dx90.vtx` and `.vvd` bytes.
///
/// # Errors
///
/// Returns [`ModelError::Parse`] on malformed or mismatched files. Never
/// panics: a panic inside the parser is contained and reported as an error.
pub fn load_model(mdl: &[u8], vtx: &[u8], vvd: &[u8]) -> Result<ModelGeometry, ModelError> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        load_model_inner(mdl, vtx, vvd)
    }))
    .unwrap_or_else(|_| {
        Err(ModelError::Parse(
            "model parser panicked on malformed data".to_owned(),
        ))
    })
}

fn load_model_inner(mdl: &[u8], vtx: &[u8], vvd: &[u8]) -> Result<ModelGeometry, ModelError> {
    let parse = |error: vmdl::ModelError| ModelError::Parse(error.to_string());
    let model = Model::from_parts(
        Mdl::read(mdl).map_err(parse)?,
        Vtx::read(vtx).map_err(parse)?,
        Vvd::read(vvd).map_err(parse)?,
    );

    let meshes: Vec<ModelMesh> = model
        .meshes()
        .map(|mesh| {
            let mut positions = Vec::new();
            let mut normals = Vec::new();
            let mut uvs = Vec::new();
            let vertices: Vec<_> = mesh.vertices().collect();
            // Source winds front faces clockwise; Bevy treats counter-clockwise
            // as front. Swapping two corners of every triangle lets the
            // renderer cull back faces correctly.
            for triangle in vertices.as_chunks::<3>().0 {
                for vertex in [triangle[0], triangle[2], triangle[1]] {
                    let position = model.apply_root_transform(vertex.position);
                    let normal = model.apply_root_transform(vertex.normal);
                    positions.push([position.x, position.y, position.z]);
                    normals.push([normal.x, normal.y, normal.z]);
                    uvs.push(vertex.texture_coordinates);
                }
            }
            ModelMesh {
                material: usize::try_from(mesh.material_index()).unwrap_or(0),
                positions,
                normals,
                uvs,
            }
        })
        .filter(|mesh| !mesh.positions.is_empty())
        .collect();

    // A skin table maps material slots to texture indices; models without
    // skin tables use the texture list directly.
    let slots = meshes
        .iter()
        .map(|mesh| mesh.material + 1)
        .max()
        .unwrap_or(0);
    let mut skins: Vec<Vec<String>> = model
        .skin_tables()
        .map(|table| {
            (0..slots)
                .map(|slot| {
                    i32::try_from(slot)
                        .ok()
                        .and_then(|slot| table.texture(slot))
                        .unwrap_or_default()
                        .to_owned()
                })
                .collect()
        })
        .collect();
    if skins.iter().all(|skin| skin.iter().all(String::is_empty)) {
        skins = vec![
            model
                .textures()
                .iter()
                .map(|texture| texture.name.clone())
                .collect(),
        ];
    }

    let (min, max) = model.bounding_box();
    Ok(ModelGeometry {
        meshes,
        skins,
        material_dirs: model.texture_directories().to_vec(),
        bounds: ([min.x, min.y, min.z], [max.x, max.y, max.z]),
        surface_prop: model.surface_prop().to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn garbage_errors_without_panicking() {
        assert!(load_model(b"not", b"a", b"model").is_err());
        assert!(load_model(&[], &[], &[]).is_err());
    }

    #[test]
    fn material_candidates_follow_the_search_directories() {
        let geometry = ModelGeometry {
            meshes: Vec::new(),
            skins: vec![
                vec!["oildrum001".into()],
                vec!["oildrum001_explosive".into()],
            ],
            material_dirs: vec!["models\\props_c17\\".into(), "models/shared/".into()],
            bounds: ([0.0; 3], [0.0; 3]),
            surface_prop: String::new(),
        };
        assert_eq!(
            geometry.material_candidates(0, 0),
            [
                "materials/models/props_c17/oildrum001.vmt",
                "materials/models/shared/oildrum001.vmt"
            ]
        );
        assert_eq!(
            geometry.material_candidates(1, 0)[0],
            "materials/models/props_c17/oildrum001_explosive.vmt"
        );
        // An out-of-range skin falls back to skin 0; a missing slot yields none.
        assert_eq!(geometry.material_candidates(9, 0).len(), 2);
        assert!(geometry.material_candidates(0, 5).is_empty());
    }

    /// Loads a real model. Set `RUSTLIFE_TEST_MDL` to a `.mdl` path whose
    /// `.dx90.vtx` and `.vvd` siblings sit next to it.
    #[test]
    #[ignore = "requires a model; set RUSTLIFE_TEST_MDL"]
    fn loads_a_real_model() {
        let Ok(path) = std::env::var("RUSTLIFE_TEST_MDL") else {
            return;
        };
        let read = |extension: &str| {
            std::fs::read(std::path::Path::new(&path).with_extension(extension))
                .expect("read model part")
        };
        let geometry =
            load_model(&read("mdl"), &read("dx90.vtx"), &read("vvd")).expect("load model");
        let triangles: usize = geometry.meshes.iter().map(|m| m.positions.len() / 3).sum();
        eprintln!(
            "{path}: {} meshes, {triangles} triangles, skins {:?}, dirs {:?}, bounds {:?}",
            geometry.meshes.len(),
            geometry.skins,
            geometry.material_dirs,
            geometry.bounds
        );
        assert!(triangles > 0);
        assert!(geometry.meshes.iter().all(|m| m.positions.len() % 3 == 0));
        assert!(!geometry.material_candidates(0, 0).is_empty());
    }
}
