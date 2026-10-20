//! Per-instance model lighting baked into vertex colours.
//!
//! Static props: `source://sp_<index>.vhv` becomes a [`VertexLightingAsset`],
//! and [`vertex_lit_mesh`] bakes it into a copy of a model mesh. Other props:
//! [`cube_lit_mesh`] evaluates the light the engine would give the model (an
//! ambient cube including the world lights that see it, see
//! [`source_formats::ambient::AmbientLighting::lighting_at`]) at every vertex.
//!
//! The lighting belongs to one prop instance, so each lit prop gets its own
//! meshes; the model's shared meshes stay untouched.

use bevy_asset::io::Reader;
use bevy_asset::{Asset, AssetLoader, LoadContext, RenderAssetUsages};
use bevy_math::Quat;
use bevy_mesh::{Mesh, VertexAttributeValues};
use bevy_reflect::TypePath;
use futures_lite::AsyncReadExt;
use source_formats::ambient::AmbientCube;
use source_formats::vhv::{self, VertexLighting, VhvError};
use thiserror::Error;

use crate::coords;
use crate::model::{ModelAsset, ModelPart};

/// A static prop's decoded baked lighting.
#[derive(Asset, TypePath, Debug, Clone)]
pub struct VertexLightingAsset(pub VertexLighting);

/// Errors from [`VhvLoader`].
#[derive(Debug, Error)]
pub enum VhvLoadError {
    /// The asset bytes could not be read.
    #[error("reading vertex lighting bytes: {0}")]
    Read(#[from] std::io::Error),
    /// The file is malformed.
    #[error(transparent)]
    Decode(#[from] VhvError),
}

/// Loads a `.vhv` file into a [`VertexLightingAsset`].
#[derive(Default, TypePath)]
pub struct VhvLoader;

impl AssetLoader for VhvLoader {
    type Asset = VertexLightingAsset;
    type Settings = ();
    type Error = VhvLoadError;

    async fn load(
        &self,
        reader: &mut dyn Reader,
        _settings: &(),
        _load_context: &mut LoadContext<'_>,
    ) -> Result<VertexLightingAsset, VhvLoadError> {
        let mut bytes = Vec::new();
        AsyncReadExt::read_to_end(reader, &mut bytes).await?;
        Ok(VertexLightingAsset(vhv::decode(&bytes)?))
    }

    fn extensions(&self) -> &[&str] {
        &["vhv"]
    }
}

/// The lighting's colour for every vertex of `model`, or `None` if it was
/// baked for a different model or vertex layout.
#[must_use]
pub fn colors_by_vertex(lighting: &VertexLighting, model: &ModelAsset) -> Option<Vec<[f32; 3]>> {
    lighting.by_vertex_with(model.checksum, &model.strip_vertex_ids)
}

/// A copy of `base` (the mesh of `part`) carrying `colors` — from
/// [`colors_by_vertex`] — as linear vertex colours, or `None` if the mesh
/// and part disagree on their vertex count.
#[must_use]
pub fn vertex_lit_mesh(base: &Mesh, part: &ModelPart, colors: &[[f32; 3]]) -> Option<Mesh> {
    if base.count_vertices() != part.vertex_ids.len() {
        return None;
    }
    let vertex_colors: Vec<[f32; 4]> = part
        .vertex_ids
        .iter()
        .map(|&id| {
            let [r, g, b] = colors.get(id as usize).copied().unwrap_or([0.0; 3]);
            [r, g, b, 1.0]
        })
        .collect();
    Some(with_colors(base, vertex_colors))
}

/// A copy of `base` lit by `cube` (Source space) as vertex colours: each
/// vertex receives `Σ nᵢ² · cube[side of nᵢ]` for its normal `n` turned by
/// the prop's `rotation` (Valve's ambient-cube evaluation). `None` if the
/// mesh has no normals.
#[must_use]
pub fn cube_lit_mesh(base: &Mesh, rotation: Quat, cube: &AmbientCube) -> Option<Mesh> {
    let Some(VertexAttributeValues::Float32x3(normals)) = base.attribute(Mesh::ATTRIBUTE_NORMAL)
    else {
        return None;
    };
    let vertex_colors = normals
        .iter()
        .map(|&normal| {
            let n = coords::to_source_direction(rotation * bevy_math::Vec3::from(normal));
            let mut color = [0.0, 0.0, 0.0, 1.0];
            for (axis, component) in n.into_iter().enumerate() {
                let side = &cube[axis * 2 + usize::from(component < 0.0)];
                for (channel, light) in color.iter_mut().zip(side) {
                    *channel += component * component * light;
                }
            }
            color
        })
        .collect();
    Some(with_colors(base, vertex_colors))
}

/// A render-only copy of `base` with `colors` as its vertex colours.
fn with_colors(base: &Mesh, colors: Vec<[f32; 4]>) -> Mesh {
    let mut mesh = base.clone();
    mesh.asset_usage = RenderAssetUsages::RENDER_WORLD;
    mesh.insert_attribute(
        Mesh::ATTRIBUTE_COLOR,
        VertexAttributeValues::Float32x4(colors),
    );
    mesh
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_asset::Handle;
    use bevy_mesh::PrimitiveTopology;

    fn triangle() -> Mesh {
        Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::default(),
        )
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, vec![[0.0f32; 3]; 3])
    }

    #[test]
    fn colours_follow_the_part_vertex_ids() {
        let part = ModelPart {
            mesh: Handle::default(),
            material: 0,
            vertex_ids: vec![2, 0, 2],
        };
        let colors = [[0.1; 3], [0.2; 3], [0.3; 3]];
        let mesh = vertex_lit_mesh(&triangle(), &part, &colors).expect("matching counts");
        let Some(VertexAttributeValues::Float32x4(values)) = mesh.attribute(Mesh::ATTRIBUTE_COLOR)
        else {
            panic!("vertex colours present");
        };
        assert_eq!(
            values,
            &[
                [0.3, 0.3, 0.3, 1.0],
                [0.1, 0.1, 0.1, 1.0],
                [0.3, 0.3, 0.3, 1.0]
            ]
        );
        assert_eq!(mesh.asset_usage, RenderAssetUsages::RENDER_WORLD);
    }

    #[test]
    fn cube_lighting_follows_the_turned_normal() {
        // One vertex facing Bevy +Y (Source +Z, up) in the model frame.
        let base =
            triangle().with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0f32, 1.0, 0.0]; 3]);
        let mut cube = AmbientCube::default();
        cube[4] = [1.0, 0.0, 0.0]; // +Z: red from above
        cube[0] = [0.0, 1.0, 0.0]; // +X: green from Source +X

        let colors = |mesh: &Mesh| match mesh.attribute(Mesh::ATTRIBUTE_COLOR) {
            Some(VertexAttributeValues::Float32x4(values)) => values[0],
            _ => panic!("vertex colours present"),
        };
        let upright = cube_lit_mesh(&base, Quat::IDENTITY, &cube).expect("normals");
        assert_eq!(colors(&upright), [1.0, 0.0, 0.0, 1.0]);
        // Tipped 90° about Bevy -Z (Source +Y), the normal faces Bevy +X:
        // Source +X, which is lit green.
        let tipped = Quat::from_axis_angle(bevy_math::Vec3::NEG_Z, std::f32::consts::FRAC_PI_2);
        let tipped = cube_lit_mesh(&base, tipped, &cube).expect("normals");
        let expected = [0.0, 1.0, 0.0, 1.0];
        assert!(
            colors(&tipped)
                .iter()
                .zip(expected)
                .all(|(a, b)| (a - b).abs() < 1e-6),
            "{:?}",
            colors(&tipped)
        );
        assert!(
            cube_lit_mesh(&triangle(), Quat::IDENTITY, &cube).is_none(),
            "no normals"
        );
    }

    #[test]
    fn mismatched_parts_are_rejected() {
        let part = ModelPart {
            mesh: Handle::default(),
            material: 0,
            vertex_ids: vec![0; 6],
        };
        assert!(vertex_lit_mesh(&triangle(), &part, &[[1.0; 3]]).is_none());
    }
}
