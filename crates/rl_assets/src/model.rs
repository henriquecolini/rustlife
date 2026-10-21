//! Model loading: `source://models/foo.mdl` becomes a [`ModelAsset`].
//!
//! A Source model spans three files. The loader is invoked on the `.mdl` and
//! reads its siblings — the `.vvd` vertices and the `.vtx` index strips
//! (`.dx90.vtx`, falling back to the older `.dx80.vtx` and `.sw.vtx`) —
//! through the same asset source, then hands all three to
//! [`source_formats::mdl::load_model`].
//!
//! Each mesh becomes a Bevy [`Mesh`] sub-asset in Bevy space. Materials are
//! resolved to names (relative to `materials/`, without extension) by probing
//! the model's `$cdmaterials` directories for the first VMT that exists, so
//! the client can share its material cache between world and models.
//!
//! The loader also reads the model's `.phy` collision model when there is
//! one, which the simulation builds the prop's physics body from; it stays in
//! Source space, like everything the simulation consumes.
//!
//! A skeletal model (more than one bone) also gets its [`Skeleton`], the
//! inverse bind poses its meshes are skinned with, and a clip per blend of
//! each of its sequences — its own and those of the models it includes
//! (`$includemodel`), which may include more in turn.

use std::sync::Arc;

use bevy_animation::prelude::AnimationClip;
use bevy_asset::io::Reader;
use bevy_asset::{Asset, AssetLoader, Handle, LoadContext, RenderAssetUsages};
use bevy_math::Vec3;
use bevy_mesh::skinning::SkinnedMeshInverseBindposes;
use bevy_mesh::{Mesh, PrimitiveTopology, VertexAttributeValues};
use bevy_reflect::TypePath;
use futures_lite::AsyncReadExt;
use source_formats::anim::{self, ModelAnimation};
use source_formats::mdl::{self, ModelError, ModelGeometry, ModelMesh};
use source_formats::phy::{self, PhysicsModel};
use thiserror::Error;

use crate::coords;
use crate::skeleton::{self, SequenceClips, Skeleton};
use crate::source::SOURCE;

/// How deep `$includemodel`s nest before the loader stops following them.
const MAX_INCLUDE_DEPTH: usize = 4;

/// The `.vtx` variants tried, best first. Every stock model ships `dx90`;
/// older community models may only have the others.
const VTX_EXTENSIONS: [&str; 3] = ["dx90.vtx", "dx80.vtx", "sw.vtx"];

/// One mesh of a model and the material slot it is drawn with.
#[derive(Debug, Clone)]
pub struct ModelPart {
    /// The mesh, in Bevy space and the model's own frame.
    pub mesh: Handle<Mesh>,
    /// Index into each skin of [`ModelAsset::skins`].
    pub material: usize,
    /// Model vertex index of each of the mesh's vertices, which baked
    /// per-vertex lighting is indexed by.
    pub vertex_ids: Vec<u32>,
}

/// A loaded model: its meshes and, per skin, the material each slot uses.
#[derive(Asset, TypePath, Debug, Clone)]
pub struct ModelAsset {
    /// The meshes of the highest level of detail.
    pub parts: Vec<ModelPart>,
    /// `skins[skin][slot]`: the material name (relative to `materials/`,
    /// without extension) or `None` if no candidate VMT exists. Skin 0 is
    /// the default.
    pub skins: Vec<Vec<Option<String>>>,
    /// Axis-aligned bounds `(min, max)` in Bevy space, model frame.
    pub bounds: (Vec3, Vec3),
    /// The model's checksum, which baked lighting must match.
    pub checksum: u32,
    /// Model vertex index of each highest-detail strip-group vertex, the
    /// order baked lighting is stored in.
    pub strip_vertex_ids: Vec<u32>,
    /// The model's bounds `(min, max)` in Source space, model frame: the
    /// collision box of models without a physics model.
    pub source_bounds: ([f32; 3], [f32; 3]),
    /// The model's `$surfaceprop`, lower-cased.
    pub surface_prop: String,
    /// The model's physics collision model, if it ships a readable `.phy`.
    pub physics: Option<Arc<PhysicsModel>>,
    /// A skeletal model's skeleton and animation.
    pub animation: Option<Arc<ModelAnimationAsset>>,
}

/// What animating a skeletal model takes.
#[derive(Debug, Clone)]
pub struct ModelAnimationAsset {
    /// Its joints.
    pub skeleton: Skeleton,
    /// The inverse bind poses its meshes are skinned with.
    pub inverse_bindposes: Handle<SkinnedMeshInverseBindposes>,
    /// Its sequences and those of the models it includes, in the engine's
    /// order (its own first).
    pub sequences: Vec<SequenceClips>,
    /// Named points on its bones: name (lower case), joint and offset.
    pub attachments: Vec<(String, usize, bevy_transform::components::Transform)>,
}

impl ModelAnimationAsset {
    /// The first sequence labelled `label` (any case).
    #[must_use]
    pub fn sequence(&self, label: &str) -> Option<&SequenceClips> {
        self.sequences
            .iter()
            .find(|sequence| sequence.label.eq_ignore_ascii_case(label))
    }

    /// The first sequence of activity `activity` (any case).
    #[must_use]
    pub fn activity(&self, activity: &str) -> Option<&SequenceClips> {
        self.sequences
            .iter()
            .find(|sequence| sequence.activity.eq_ignore_ascii_case(activity))
    }
}

impl ModelAsset {
    /// The material of `slot` in `skin`, falling back to skin 0 for skins the
    /// model does not have (as the engine does).
    #[must_use]
    pub fn material(&self, skin: usize, slot: usize) -> Option<&str> {
        self.skins
            .get(skin)
            .or_else(|| self.skins.first())?
            .get(slot)?
            .as_deref()
    }
}

/// Errors from [`MdlLoader`].
#[derive(Debug, Error)]
pub enum ModelLoadError {
    /// The `.mdl` bytes could not be read.
    #[error("reading model bytes: {0}")]
    Read(#[from] std::io::Error),
    /// A sibling `.vvd` or `.vtx` file is missing.
    #[error("model is missing its {0} file")]
    MissingPart(&'static str),
    /// The files are malformed or do not belong together.
    #[error(transparent)]
    Parse(#[from] ModelError),
}

/// Loads a `.mdl` (with its `.vvd` and `.vtx`) into a [`ModelAsset`].
#[derive(Default, TypePath)]
pub struct MdlLoader;

impl AssetLoader for MdlLoader {
    type Asset = ModelAsset;
    type Settings = ();
    type Error = ModelLoadError;

    async fn load(
        &self,
        reader: &mut dyn Reader,
        _settings: &(),
        load_context: &mut LoadContext<'_>,
    ) -> Result<ModelAsset, ModelLoadError> {
        let mut mdl = Vec::new();
        AsyncReadExt::read_to_end(reader, &mut mdl).await?;
        let stem = model_stem(&load_context.path().path().to_string_lossy());

        let vvd = load_context
            .read_asset_bytes(format!("{SOURCE}://{stem}.vvd"))
            .await
            .map_err(|_| ModelLoadError::MissingPart("vvd"))?;
        let mut vtx = None;
        for extension in VTX_EXTENSIONS {
            let path = format!("{SOURCE}://{stem}.{extension}");
            if let Ok(bytes) = load_context.read_asset_bytes(path).await {
                vtx = Some(bytes);
                break;
            }
        }
        let vtx = vtx.ok_or(ModelLoadError::MissingPart("vtx"))?;

        let geometry = mdl::load_model(&mdl, &vtx, &vvd)?;
        let skins = resolve_skins(load_context, &geometry).await;
        let physics = load_physics(load_context, &stem).await;
        let animation = load_animation(load_context, &mdl).await;
        let skinned = animation.is_some();
        let parts = geometry
            .meshes
            .iter()
            .enumerate()
            .map(|(index, mesh)| ModelPart {
                mesh: load_context
                    .add_labeled_asset(format!("mesh{index}"), mesh_of(mesh, skinned)),
                material: mesh.material,
                vertex_ids: mesh.vertex_ids.clone(),
            })
            .collect();
        Ok(ModelAsset {
            parts,
            skins,
            bounds: bevy_bounds(geometry.bounds),
            checksum: geometry.checksum,
            strip_vertex_ids: geometry.strip_vertex_ids,
            source_bounds: geometry.bounds,
            surface_prop: geometry.surface_prop.to_ascii_lowercase(),
            physics,
            animation: animation.map(Arc::new),
        })
    }

    fn extensions(&self) -> &[&str] {
        &["mdl"]
    }
}

/// The asset path without its `.mdl` extension, with forward slashes:
/// `models/props_c17/oildrum001.mdl` → `models/props_c17/oildrum001`.
fn model_stem(path: &str) -> String {
    let path = path.replace('\\', "/");
    match path.len().checked_sub(4) {
        Some(cut) if path.is_char_boundary(cut) && path[cut..].eq_ignore_ascii_case(".mdl") => {
            path[..cut].to_owned()
        }
        _ => path,
    }
}

/// A model's animation file: its `.ani` blocks, if it streams them.
async fn read_ani(load_context: &mut LoadContext<'_>, model: &ModelAnimation) -> Option<Vec<u8>> {
    let file = model.anim_blocks_file.as_ref()?;
    load_context
        .read_asset_bytes(format!("{SOURCE}://{file}"))
        .await
        .ok()
}

/// A skeletal model's skeleton and sequences, its included models'
/// sequences after its own; `None` for a model of one bone (a prop) or one
/// whose skeleton cannot be read.
async fn load_animation(
    load_context: &mut LoadContext<'_>,
    mdl: &[u8],
) -> Option<ModelAnimationAsset> {
    let first = anim::read(mdl, None).ok()?;
    if first.bones.len() < 2 || first.static_prop {
        return None;
    }
    let main = match read_ani(load_context, &first).await {
        Some(ani) => anim::read(mdl, Some(&ani)).unwrap_or(first),
        None => first,
    };
    let skeleton = Skeleton::of(&main);
    let inverse_bindposes = load_context.add_labeled_asset(
        "bindposes".to_owned(),
        SkinnedMeshInverseBindposes::from(skeleton.inverse_bindposes()),
    );
    let mut clip_count = 0;
    let mut sequences = Vec::new();
    // The model, then its includes breadth first, each once.
    let mut queue: Vec<(ModelAnimation, usize)> = vec![(main.clone(), 0)];
    let mut seen: Vec<String> = Vec::new();
    while !queue.is_empty() {
        let (model, depth) = queue.remove(0);
        let mut add = |clip: AnimationClip| {
            clip_count += 1;
            load_context.add_labeled_asset(format!("clip{clip_count}"), clip)
        };
        sequences.extend(skeleton::sequence_clips(&skeleton, &model, &mut add));
        if depth >= MAX_INCLUDE_DEPTH {
            continue;
        }
        for include in &model.includes {
            let path = include.to_ascii_lowercase();
            if seen.contains(&path) {
                continue;
            }
            seen.push(path.clone());
            let Ok(bytes) = load_context
                .read_asset_bytes(format!("{SOURCE}://{path}"))
                .await
            else {
                tracing::debug!(include = path, "included model not found");
                continue;
            };
            let Ok(included) = anim::read(&bytes, None) else {
                tracing::debug!(include = path, "unreadable included model");
                continue;
            };
            let included = match read_ani(load_context, &included).await {
                Some(ani) => anim::read(&bytes, Some(&ani)).unwrap_or(included),
                None => included,
            };
            queue.push((included, depth + 1));
        }
    }
    let attachments = main
        .attachments
        .iter()
        .map(|attachment| {
            (
                attachment.name.to_ascii_lowercase(),
                attachment.bone,
                attachment_transform(&attachment.local),
            )
        })
        .collect();
    Some(ModelAnimationAsset {
        skeleton,
        inverse_bindposes,
        sequences,
        attachments,
    })
}

/// An attachment's 3×4 offset from its bone as a Bevy transform.
fn attachment_transform(local: &[[f32; 4]; 3]) -> bevy_transform::components::Transform {
    let columns = |column: usize| [local[0][column], local[1][column], local[2][column]];
    let rotation = bevy_math::Mat3::from_cols(
        coords::direction(columns(0)),
        coords::direction(columns(1)),
        coords::direction(columns(2)),
    );
    // The rotation's columns map the frame's axes; in Bevy space the frame
    // itself is remapped too.
    let frame = bevy_math::Mat3::from_cols(
        coords::direction([1.0, 0.0, 0.0]),
        coords::direction([0.0, 1.0, 0.0]),
        coords::direction([0.0, 0.0, 1.0]),
    );
    bevy_transform::components::Transform {
        translation: coords::position(columns(3)),
        rotation: bevy_math::Quat::from_mat3(&(rotation * frame.transpose())),
        scale: Vec3::ONE,
    }
}

/// Reads and decodes `<stem>.phy`. Most models without physics simply have
/// no such file; a present but unreadable one is logged, and the model then
/// collides as its bounding box.
async fn load_physics(load_context: &mut LoadContext<'_>, stem: &str) -> Option<Arc<PhysicsModel>> {
    let bytes = load_context
        .read_asset_bytes(format!("{SOURCE}://{stem}.phy"))
        .await
        .ok()?;
    match phy::decode(&bytes) {
        Ok(model) => Some(Arc::new(model)),
        Err(error) => {
            tracing::warn!(model = stem, "unreadable physics model: {error}");
            None
        }
    }
}

/// Resolves every skin's material slots to the first candidate VMT that
/// exists. VMTs are tiny, so probing by reading them is cheap; the reads also
/// make the model reload if a material it depends on changes.
async fn resolve_skins(
    load_context: &mut LoadContext<'_>,
    geometry: &ModelGeometry,
) -> Vec<Vec<Option<String>>> {
    let slots = geometry.skins.first().map_or(0, Vec::len);
    let mut skins = Vec::with_capacity(geometry.skins.len());
    for skin in 0..geometry.skins.len() {
        let mut materials = Vec::with_capacity(slots);
        for slot in 0..slots {
            let mut found = None;
            for candidate in geometry.material_candidates(skin, slot) {
                let path = format!("{SOURCE}://{candidate}");
                if load_context.read_asset_bytes(path).await.is_ok() {
                    found = Some(material_name(&candidate));
                    break;
                }
            }
            if found.is_none() {
                tracing::debug!(
                    model = %load_context.path(),
                    skin,
                    slot,
                    "no material found for model slot"
                );
            }
            materials.push(found);
        }
        skins.push(materials);
    }
    skins
}

/// `materials/models/Foo/Bar.vmt` → `models/foo/bar`: the form world
/// surfaces use, lower-cased (the VFS is case-insensitive, so this is the
/// canonical spelling) so equal materials share one cache entry.
fn material_name(vmt_path: &str) -> String {
    let name = vmt_path.to_ascii_lowercase();
    let name = name.strip_prefix("materials/").unwrap_or(&name);
    name.strip_suffix(".vmt").unwrap_or(name).to_owned()
}

/// The model's Source-space bounds as a Bevy-space AABB. The axis remap flips
/// Source Y into Bevy −Z, so min and max swap on that axis.
fn bevy_bounds((min, max): ([f32; 3], [f32; 3])) -> (Vec3, Vec3) {
    let a = coords::position(min);
    let b = coords::position(max);
    (a.min(b), a.max(b))
}

/// Converts one model mesh (Source space) into a Bevy [`Mesh`], with its
/// joints and weights when `skinned`.
///
/// The mesh stays readable on the CPU after upload: props with baked
/// lighting draw a vertex-coloured copy of it (see
/// [`crate::vertex_lighting::vertex_lit_mesh`]).
fn mesh_of(mesh: &ModelMesh, skinned: bool) -> Mesh {
    let positions: Vec<[f32; 3]> = mesh
        .positions
        .iter()
        .map(|&p| coords::position(p).to_array())
        .collect();
    let normals: Vec<[f32; 3]> = mesh
        .normals
        .iter()
        .map(|&n| coords::direction(n).normalize_or_zero().to_array())
        .collect();
    let built = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
    .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, normals)
    .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, mesh.uvs.clone());
    if !skinned || mesh.joints.len() != mesh.positions.len() {
        return built;
    }
    built
        .with_inserted_attribute(
            Mesh::ATTRIBUTE_JOINT_INDEX,
            VertexAttributeValues::Uint16x4(mesh.joints.clone()),
        )
        .with_inserted_attribute(Mesh::ATTRIBUTE_JOINT_WEIGHT, mesh.weights.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stems_drop_the_extension_case_insensitively() {
        assert_eq!(
            model_stem("models/props_c17/oildrum001.mdl"),
            "models/props_c17/oildrum001"
        );
        assert_eq!(model_stem("models\\Crate.MDL"), "models/Crate");
        assert_eq!(model_stem("mdl"), "mdl");
    }

    #[test]
    fn material_names_match_world_surface_names() {
        assert_eq!(
            material_name("materials/models/props_c17/Oil_Drum001a.vmt"),
            "models/props_c17/oil_drum001a"
        );
    }

    #[test]
    fn bounds_stay_ordered_after_the_axis_remap() {
        let (min, max) = bevy_bounds(([-10.0, -20.0, 0.0], [10.0, 20.0, 40.0]));
        assert!(min.cmple(max).all());
        assert!((max.z - 20.0 * coords::METERS_PER_UNIT).abs() < 1e-6);
    }

    #[test]
    fn missing_skins_fall_back_to_skin_zero() {
        let model = ModelAsset {
            parts: Vec::new(),
            skins: vec![vec![Some("a".into()), None], vec![Some("b".into())]],
            bounds: (Vec3::ZERO, Vec3::ZERO),
            checksum: 0,
            strip_vertex_ids: Vec::new(),
            source_bounds: ([0.0; 3], [0.0; 3]),
            surface_prop: String::new(),
            physics: None,
            animation: None,
        };
        assert_eq!(model.material(1, 0), Some("b"));
        assert_eq!(model.material(7, 0), Some("a"));
        assert_eq!(model.material(0, 1), None);
        assert_eq!(model.material(0, 9), None);
    }

    #[test]
    fn meshes_carry_positions_normals_and_uvs() {
        let source = ModelMesh {
            material: 0,
            positions: vec![[0.0; 3], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            normals: vec![[0.0, 0.0, 1.0]; 3],
            uvs: vec![[0.0; 2]; 3],
            vertex_ids: vec![0, 1, 2],
            joints: vec![[1, 0, 0, 0]; 3],
            weights: vec![[1.0, 0.0, 0.0, 0.0]; 3],
        };
        let mesh = mesh_of(&source, false);
        assert_eq!(mesh.count_vertices(), 3);
        assert!(mesh.attribute(Mesh::ATTRIBUTE_UV_0).is_some());
        assert!(mesh.attribute(Mesh::ATTRIBUTE_JOINT_INDEX).is_none());
        let skinned = mesh_of(&source, true);
        assert!(skinned.attribute(Mesh::ATTRIBUTE_JOINT_INDEX).is_some());
        assert!(skinned.attribute(Mesh::ATTRIBUTE_JOINT_WEIGHT).is_some());
    }
}
