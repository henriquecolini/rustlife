//! Static geometry from Source models (`.mdl` + `.dx90.vtx` + `.vvd`).
//!
//! A Source model is split across three files: the `.mdl` holds the skeleton,
//! materials and skins, the `.vvd` the vertices, and the `.vtx` the
//! hardware-optimised index strips per level of detail. `vmdl` reads all three;
//! this module flattens the highest-detail LOD into triangle lists grouped by
//! mesh, with the material table needed to texture them.
//!
//! Positions and normals stay in Source space (Z up, inches), in the model's
//! own frame. A single-bone model (a prop) has its bone's idle placement
//! applied, so it rests the way it appears in the game; a skeletal model
//! stays in its reference pose, each vertex with the bones that move it and
//! their weights, to be skinned by its skeleton ([`crate::anim`]).

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
    /// Per-vertex index into the model's vertex list (the `.vvd` vertices),
    /// which baked per-vertex data such as [`crate::vhv`] lighting refers to.
    pub vertex_ids: Vec<u32>,
    /// Per-vertex bones (up to three, padded with bone 0) that move it.
    pub joints: Vec<[u16; 4]>,
    /// Per-vertex weights of [`ModelMesh::joints`], summing to 1.
    pub weights: Vec<[f32; 4]>,
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
    /// The model's checksum, shared by its `.vtx`, `.vvd` and any baked
    /// lighting computed for it.
    pub checksum: u32,
    /// Model vertex index of every highest-detail strip-group vertex, in file
    /// order (body part, model, mesh, strip group). Baked static-prop
    /// lighting stores one colour per entry, in this order.
    pub strip_vertex_ids: Vec<u32>,
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

/// What the simulation needs of a model, all in its `.mdl`: no meshes.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelHeader {
    /// Axis-aligned bounds `(min, max)`, Source space, model frame.
    pub bounds: ([f32; 3], [f32; 3]),
    /// The model's `$surfaceprop`, as written.
    pub surface_prop: String,
}

/// Reads a model's bounds and surface property from its `.mdl` alone,
/// without the `.vvd` and `.vtx` that [`load_model`] needs: what a
/// dedicated server builds prop collision from.
///
/// # Errors
///
/// Returns [`ModelError::Parse`] on a malformed `.mdl`; never panics.
pub fn read_header(mdl: &[u8]) -> Result<ModelHeader, ModelError> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mdl = Mdl::read(mdl).map_err(|error| ModelError::Parse(error.to_string()))?;
        let [min, max] = mdl.header.bounding_box;
        Ok(ModelHeader {
            bounds: ([min.x, min.y, min.z], [max.x, max.y, max.z]),
            surface_prop: mdl.surface_prop,
        })
    }))
    .unwrap_or_else(|_| {
        Err(ModelError::Parse(
            "model header parser panicked on malformed data".to_owned(),
        ))
    })
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
    let mdl = Mdl::read(mdl).map_err(parse)?;
    let vtx = Vtx::read(vtx).map_err(parse)?;
    let checksum = u32::from_le_bytes(vtx.header.checksum);
    let strip_vertex_ids = strip_vertex_ids(&mdl, &vtx);
    let model = Model::from_parts(mdl, vtx, Vvd::read(vvd).map_err(parse)?);
    // Vertices are stored in model space, in the bind pose. A single-bone
    // model (a prop) is drawn as its idle sequence places that bone, which
    // may turn it. A skeletal model's root bone is its pelvis, whose own
    // rotation describes the skeleton's axes rather than a turn of the whole
    // model: applying it would lay a character on its back, so skeletal
    // models stay in their bind pose (until they are animated).
    let skeletal = model.bones().nth(1).is_some();
    let place = |vector| {
        if skeletal {
            vector
        } else {
            model.apply_root_transform(vector)
        }
    };

    let meshes: Vec<ModelMesh> = model
        .meshes()
        .map(|mesh| {
            let mut positions = Vec::new();
            let mut normals = Vec::new();
            let mut uvs = Vec::new();
            let mut vertex_ids = Vec::new();
            let mut joints = Vec::new();
            let mut weights = Vec::new();
            let vertices: Vec<_> = mesh
                .vertex_strip_indices()
                .flatten()
                .zip(mesh.vertices())
                .collect();
            // Source winds front faces clockwise; Bevy treats counter-clockwise
            // as front. Swapping two corners of every triangle lets the
            // renderer cull back faces correctly.
            for triangle in vertices.as_chunks::<3>().0 {
                for (id, vertex) in [triangle[0], triangle[2], triangle[1]] {
                    vertex_ids.push(u32::try_from(id).unwrap_or(u32::MAX));
                    let position = place(vertex.position);
                    let normal = place(vertex.normal);
                    positions.push([position.x, position.y, position.z]);
                    normals.push([normal.x, normal.y, normal.z]);
                    uvs.push(vertex.texture_coordinates);
                    let (joint, weight) = skin_weights(vertex);
                    joints.push(joint);
                    weights.push(weight);
                }
            }
            ModelMesh {
                material: usize::try_from(mesh.material_index()).unwrap_or(0),
                positions,
                normals,
                uvs,
                vertex_ids,
                joints,
                weights,
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
        checksum,
        strip_vertex_ids,
    })
}

/// A vertex's bones and their weights, normalised to sum to 1; a vertex
/// with no bones follows bone 0. (`vmdl` hands out each weight divided by
/// the vertex's bone count, which is undone here.)
fn skin_weights(vertex: &vmdl::vvd::Vertex) -> ([u16; 4], [f32; 4]) {
    let found: Vec<(u16, f32)> = vertex
        .bone_weights
        .weights()
        .map(|weight| (u16::from(weight.bone_id), weight.weight))
        .collect();
    let mut joints = [0u16; 4];
    let mut weights = [0.0f32; 4];
    let total: f32 = found.iter().map(|&(_, weight)| weight.max(0.0)).sum();
    if total <= 0.0 {
        weights[0] = 1.0;
        return (joints, weights);
    }
    for (slot, &(bone, weight)) in found.iter().take(4).enumerate() {
        joints[slot] = bone;
        weights[slot] = weight.max(0.0) / total;
    }
    (joints, weights)
}

/// The model vertex index of each highest-detail strip-group vertex, in the
/// order `vrad` writes baked lighting: body parts, their models, the models'
/// meshes, then each mesh's strip groups. A strip-group vertex addresses the
/// model's vertex list through its model's and mesh's vertex offsets.
fn strip_vertex_ids(mdl: &Mdl, vtx: &Vtx) -> Vec<u32> {
    let mut ids = Vec::new();
    for (mdl_part, vtx_part) in mdl.body_parts.iter().zip(&vtx.body_parts) {
        for (mdl_model, vtx_model) in mdl_part.models.iter().zip(&vtx_part.models) {
            let Some(lod) = vtx_model.lods.first() else {
                continue;
            };
            for (mdl_mesh, vtx_mesh) in mdl_model.meshes.iter().zip(&lod.meshes) {
                let offset = i64::from(mdl_model.vertex_offset) + i64::from(mdl_mesh.vertex_offset);
                for group in &vtx_mesh.strip_groups {
                    ids.extend(group.vertices.iter().map(|vertex| {
                        u32::try_from(offset + i64::from(vertex.original_mesh_vertex_id))
                            .unwrap_or(u32::MAX)
                    }));
                }
            }
        }
    }
    ids
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn garbage_errors_without_panicking() {
        assert!(load_model(b"not", b"a", b"model").is_err());
        assert!(load_model(&[], &[], &[]).is_err());
        assert!(read_header(b"not a model").is_err());
        assert!(read_header(&[]).is_err());
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
            checksum: 0,
            strip_vertex_ids: Vec::new(),
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
        let vertex_count = geometry
            .strip_vertex_ids
            .iter()
            .max()
            .map_or(0, |&id| id + 1);
        assert!(
            geometry
                .meshes
                .iter()
                .all(|m| m.vertex_ids.len() == m.positions.len()
                    && m.vertex_ids.iter().all(|&id| id < vertex_count)),
            "every triangle vertex is a strip-group vertex"
        );
        assert!(!geometry.material_candidates(0, 0).is_empty());
        let header = read_header(&read("mdl")).expect("read header");
        assert_eq!(header.bounds, geometry.bounds);
        assert_eq!(header.surface_prop, geometry.surface_prop);
    }
}
