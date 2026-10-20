//! BSP map loading: `source://maps/foo.bsp` becomes a [`MapAsset`].
//!
//! The loader turns each per-material surface from
//! [`source_formats::bsp`] into a Bevy [`Mesh`] (converting positions and
//! normals to Bevy space), stored as a labeled sub-asset. The resulting
//! [`MapAsset`] pairs each mesh handle with its material name; the client
//! resolves those names to materials and spawns the meshes.

use bevy_asset::io::Reader;
use bevy_asset::{Asset, AssetLoader, Handle, LoadContext, RenderAssetUsages};
use bevy_mesh::{Mesh, PrimitiveTopology};
use bevy_reflect::TypePath;
use futures_lite::AsyncReadExt;
use source_formats::bsp::{self, BspError, MapGeometry, MapSurface};
use thiserror::Error;

use crate::coords;

/// One renderable chunk of a map: a mesh and the material it should be drawn
/// with (a path such as `concrete/concretefloor001a`).
#[derive(Debug, Clone)]
pub struct MapMesh {
    /// Material path, relative to `materials/`, without extension.
    pub material: String,
    /// Handle to the sub-mesh built for this material.
    pub mesh: Handle<Mesh>,
}

/// A loaded BSP map: its world geometry as one mesh per material.
#[derive(Asset, TypePath, Debug, Clone, Default)]
pub struct MapAsset {
    /// One entry per distinct material in the map.
    pub meshes: Vec<MapMesh>,
}

/// Errors from [`BspLoader`].
#[derive(Debug, Error)]
pub enum MapLoadError {
    /// The asset bytes could not be read from the reader.
    #[error("reading BSP bytes: {0}")]
    Read(#[from] std::io::Error),
    /// The bytes are not a readable BSP.
    #[error(transparent)]
    Parse(#[from] BspError),
}

/// Loads a `.bsp` file into a [`MapAsset`] of per-material meshes.
#[derive(Default, TypePath)]
pub struct BspLoader;

impl AssetLoader for BspLoader {
    type Asset = MapAsset;
    type Settings = ();
    type Error = MapLoadError;

    async fn load(
        &self,
        reader: &mut dyn Reader,
        _settings: &(),
        load_context: &mut LoadContext<'_>,
    ) -> Result<MapAsset, MapLoadError> {
        let mut bytes = Vec::new();
        AsyncReadExt::read_to_end(reader, &mut bytes).await?;
        let geometry = bsp::load_geometry(&bytes)?;
        Ok(build_map(&geometry, load_context))
    }

    fn extensions(&self) -> &[&str] {
        &["bsp"]
    }
}

/// Builds the [`MapAsset`], adding each surface's mesh as a labeled sub-asset.
fn build_map(geometry: &MapGeometry, load_context: &mut LoadContext) -> MapAsset {
    let meshes = geometry
        .surfaces
        .iter()
        .enumerate()
        .map(|(index, surface)| {
            let handle = load_context.add_labeled_asset(format!("mesh{index}"), mesh_of(surface));
            MapMesh {
                material: surface.material.clone(),
                mesh: handle,
            }
        })
        .collect();
    MapAsset { meshes }
}

/// Converts one [`MapSurface`] (Source space) into a Bevy [`Mesh`].
fn mesh_of(surface: &MapSurface) -> Mesh {
    let positions: Vec<[f32; 3]> = surface
        .positions
        .iter()
        .map(|&p| coords::position(p).to_array())
        .collect();
    let normals: Vec<[f32; 3]> = surface
        .normals
        .iter()
        .map(|&n| coords::direction(n).normalize_or_zero().to_array())
        .collect();

    Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::RENDER_WORLD,
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
    .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, normals)
    .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, surface.uvs.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_triangle_surface_yields_a_three_vertex_mesh() {
        let surface = MapSurface {
            material: "concrete/floor".to_owned(),
            positions: vec![[0.0, 0.0, 0.0], [16.0, 0.0, 0.0], [0.0, 0.0, 16.0]],
            normals: vec![[0.0, 1.0, 0.0]; 3],
            uvs: vec![[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]],
        };
        let mesh = mesh_of(&surface);
        assert_eq!(mesh.count_vertices(), 3);
        assert!(mesh.attribute(Mesh::ATTRIBUTE_POSITION).is_some());
        assert!(mesh.attribute(Mesh::ATTRIBUTE_NORMAL).is_some());
        assert!(mesh.attribute(Mesh::ATTRIBUTE_UV_0).is_some());
    }
}
