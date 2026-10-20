//! BSP map loading: `source://maps/foo.bsp` becomes a [`MapAsset`].
//!
//! The loader turns each per-material surface from
//! [`source_formats::bsp`] into a Bevy [`Mesh`] (converting positions and
//! normals to Bevy space), stored as a labeled sub-asset. The resulting
//! [`MapAsset`] pairs each mesh handle with its material name; the client
//! resolves those names to materials and spawns the meshes.
//!
//! The baked lighting becomes one `Rgba16Float` atlas image (a labeled
//! sub-asset too), addressed by each mesh's second UV set.

use bevy_asset::io::Reader;
use bevy_asset::{Asset, AssetLoader, Handle, LoadContext, RenderAssetUsages};
use bevy_image::{Image, ImageSampler};
use bevy_math::Vec3;
use bevy_mesh::{Mesh, PrimitiveTopology};
use bevy_reflect::TypePath;
use futures_lite::AsyncReadExt;
use half::f16;
use source_formats::bsp::{self, BspError, MapGeometry, MapSurface};
use source_formats::lightmap::LightmapAtlas;
use thiserror::Error;
use wgpu_types::{Extent3d, TextureDimension, TextureFormat};

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

/// A player spawn point in Bevy space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spawn {
    /// Position at the player's feet, in metres.
    pub position: Vec3,
    /// Facing as a Bevy yaw in radians (see [`coords::yaw`]).
    pub yaw: f32,
}

/// A loaded BSP map: its world geometry as one mesh per material, its spawn
/// points, and a bounding sphere (Bevy space) used to frame the camera when a
/// map has no spawns.
#[derive(Asset, TypePath, Debug, Clone)]
pub struct MapAsset {
    /// One entry per distinct material in the map.
    pub meshes: Vec<MapMesh>,
    /// Player spawn points, in entity-lump order.
    pub spawns: Vec<Spawn>,
    /// The baked-lighting atlas, sampled with each mesh's `UV_1`; `None` for
    /// maps without lightmaps.
    pub lightmap: Option<Handle<Image>>,
    /// Centre of the geometry's axis-aligned bounding box, in Bevy space.
    pub center: Vec3,
    /// Half the bounding box's diagonal length, a rough "radius" for framing.
    pub radius: f32,
}

impl Default for MapAsset {
    fn default() -> Self {
        Self {
            meshes: Vec::new(),
            spawns: Vec::new(),
            lightmap: None,
            center: Vec3::ZERO,
            radius: 1.0,
        }
    }
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

/// Builds the [`MapAsset`], adding each surface's mesh as a labeled sub-asset
/// and accumulating a bounding box over all vertices (in Bevy space).
fn build_map(geometry: &MapGeometry, load_context: &mut LoadContext) -> MapAsset {
    let mut min = Vec3::splat(f32::INFINITY);
    let mut max = Vec3::splat(f32::NEG_INFINITY);

    let meshes = geometry
        .surfaces
        .iter()
        .enumerate()
        .map(|(index, surface)| {
            for &position in &surface.positions {
                let p = coords::position(position);
                min = min.min(p);
                max = max.max(p);
            }
            let handle = load_context.add_labeled_asset(format!("mesh{index}"), mesh_of(surface));
            MapMesh {
                material: surface.material.clone(),
                mesh: handle,
            }
        })
        .collect();

    // Guard against an empty or degenerate map so the camera still gets sane
    // framing values rather than infinities.
    let (center, radius) = if min.x <= max.x {
        ((min + max) * 0.5, (max - min).length() / 2.0)
    } else {
        (Vec3::ZERO, 1.0)
    };

    let spawns = geometry
        .spawns
        .iter()
        .map(|spawn| Spawn {
            position: coords::position(spawn.origin),
            yaw: coords::yaw(spawn.yaw),
        })
        .collect();

    let lightmap = geometry
        .lightmap
        .as_ref()
        .map(|atlas| load_context.add_labeled_asset("lightmap", lightmap_image(atlas)));

    MapAsset {
        meshes,
        spawns,
        lightmap,
        center,
        radius: radius.max(1.0),
    }
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

    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::RENDER_WORLD,
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
    .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, normals)
    .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, surface.uvs.clone());
    if !surface.lightmap_uvs.is_empty() {
        mesh.insert_attribute(Mesh::ATTRIBUTE_UV_1, surface.lightmap_uvs.clone());
    }
    mesh
}

/// Uploads a lightmap atlas as an `Rgba16Float` image.
///
/// Half floats keep the over-bright range of Source lighting (luxels well above
/// 1.0 are common near lights) while staying filterable on every GPU, which
/// `Rgba32Float` is not. The sampler is linear and clamps to the edge; the
/// atlas's per-block borders already stop bleeding between faces.
fn lightmap_image(atlas: &LightmapAtlas) -> Image {
    let one = f16::from_f32(1.0).to_le_bytes();
    let data: Vec<u8> = atlas
        .luxels
        .iter()
        .flat_map(|luxel| {
            let [r, g, b] = luxel.map(|c| f16::from_f32(c).to_le_bytes());
            [r, g, b, one].into_iter().flatten()
        })
        .collect();
    let mut image = Image::new(
        Extent3d {
            width: atlas.width as u32,
            height: atlas.height as u32,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        data,
        TextureFormat::Rgba16Float,
        RenderAssetUsages::RENDER_WORLD,
    );
    image.sampler = ImageSampler::linear();
    image
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
            lightmap_uvs: Vec::new(),
        };
        let mesh = mesh_of(&surface);
        assert_eq!(mesh.count_vertices(), 3);
        assert!(mesh.attribute(Mesh::ATTRIBUTE_POSITION).is_some());
        assert!(mesh.attribute(Mesh::ATTRIBUTE_NORMAL).is_some());
        assert!(mesh.attribute(Mesh::ATTRIBUTE_UV_0).is_some());
        assert!(
            mesh.attribute(Mesh::ATTRIBUTE_UV_1).is_none(),
            "no lightmap, no second UV set"
        );

        let lit = MapSurface {
            lightmap_uvs: vec![[0.5, 0.5]; 3],
            ..surface
        };
        assert!(mesh_of(&lit).attribute(Mesh::ATTRIBUTE_UV_1).is_some());
    }

    #[test]
    fn lightmap_atlas_becomes_a_half_float_image() {
        let atlas = LightmapAtlas {
            width: 2,
            height: 1,
            luxels: vec![[2.0, 0.5, 0.0], [1.0, 1.0, 1.0]],
        };
        let image = lightmap_image(&atlas);
        assert_eq!(image.texture_descriptor.format, TextureFormat::Rgba16Float);
        let data = image.data.as_ref().expect("CPU data present before upload");
        assert_eq!(data.len(), 2 * 4 * 2, "two luxels of four f16 channels");
        let red = f16::from_le_bytes([data[0], data[1]]).to_f32();
        assert_eq!(red, 2.0, "over-bright values survive");
    }
}
