//! BSP map loading: `source://maps/foo.bsp` becomes a [`MapAsset`].
//!
//! The loader turns each per-material surface from
//! [`source_formats::bsp`] into a Bevy [`Mesh`] (converting positions and
//! normals to Bevy space), stored as a labeled sub-asset. The resulting
//! [`MapAsset`] pairs each mesh handle with its material name; the client
//! resolves those names to materials and spawns the meshes.
//!
//! The baked lighting becomes one `Rgba16Float` atlas image (a labeled
//! sub-asset too), addressed by each mesh's second UV set. The ambient cubes
//! models are lit by become an irradiance-volume image ([`crate::irradiance`]),
//! and every prop placement a [`PlacedProp`] naming the model to load. The
//! map's collision model ([`source_formats::collision`]) is read alongside,
//! in Source space, for the simulation.

use bevy_asset::io::Reader;
use bevy_asset::{Asset, AssetLoader, Handle, LoadContext, RenderAssetUsages};
use bevy_image::{Image, ImageSampler};
use bevy_math::{Quat, Vec3};
use bevy_mesh::{Mesh, PrimitiveTopology};
use bevy_reflect::TypePath;
use futures_lite::AsyncReadExt;
use half::f16;
use std::sync::Arc;

use source_formats::ambient::AmbientLighting;
use source_formats::bsp::{self, BrushEntity, BspError, MapGeometry, MapSurface, SpawnPoint};
use source_formats::collision::{self, MapCollision};
use source_formats::entities::MapEntity;
use source_formats::lightmap::LightmapAtlas;
use source_formats::props::{PropKind, PropPlacement};
use source_formats::skybox::{self, Cubemap};
use source_formats::visibility::Visibility;
use source_formats::vmt;
use source_formats::vtf::{self, DecodedTexture};
use thiserror::Error;
use wgpu_types::{
    Extent3d, TextureDimension, TextureFormat, TextureViewDescriptor, TextureViewDimension,
};

use crate::coords;
use crate::irradiance::{self, VolumePlacement};
use crate::source::SOURCE;

/// One renderable chunk of a map: a mesh and the material it should be drawn
/// with (a path such as `concrete/concretefloor001a`).
#[derive(Debug, Clone)]
pub struct MapMesh {
    /// Material path, relative to `materials/`, without extension.
    pub material: String,
    /// Handle to the sub-mesh built for this material.
    pub mesh: Handle<Mesh>,
    /// The visibility clusters its faces lie in; empty: always drawn.
    pub clusters: Vec<u16>,
    /// Whether it is part of the 3D skybox.
    pub skybox: bool,
}

/// The 3D skybox's camera, in Bevy space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SkyCameraData {
    /// Where the skybox's miniature of the map's centre is, metres.
    pub origin: Vec3,
    /// How many times smaller than the map the skybox is.
    pub scale: f32,
}

/// A player spawn point, in Bevy space for rendering and in Source space for
/// the simulation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spawn {
    /// Position at the player's feet, in metres.
    pub position: Vec3,
    /// Facing as a Bevy yaw in radians (see [`coords::yaw`]).
    pub yaw: f32,
    /// The same spawn point in Source space (units, degrees).
    pub source: SpawnPoint,
}

/// A brush entity (door, breakable, `func_brush`, ...) in Bevy space: its
/// meshes are in the entity's own frame, placed by `translation` and
/// `rotation`.
#[derive(Debug, Clone)]
pub struct MapBrushEntity {
    /// The entity class, e.g. `func_door`.
    pub classname: String,
    /// Its BSP model: what map logic knows it by.
    pub model: usize,
    /// Whether it starts hidden (disabled until map logic shows it).
    pub start_disabled: bool,
    /// Position of the entity's origin, in metres.
    pub translation: Vec3,
    /// Orientation (see [`coords::rotation`]).
    pub rotation: Quat,
    /// One mesh per material, lightmapped like the world.
    pub meshes: Vec<MapMesh>,
}

/// A model placed in the map, in Bevy space.
#[derive(Debug, Clone, PartialEq)]
pub struct PlacedProp {
    /// Asset path of the model, e.g. `source://models/props_c17/oildrum001.mdl`.
    pub model: String,
    /// Position of the model's origin, in metres.
    pub translation: Vec3,
    /// Orientation (see [`coords::rotation`]).
    pub rotation: Quat,
    /// Uniform scale.
    pub scale: f32,
    /// Skin (material set) index.
    pub skin: usize,
    /// Static prop or entity kind.
    pub kind: PropKind,
    /// Asset path of the prop's baked per-vertex lighting
    /// (`source://sp_<index>.vhv`), if it was compiled with one.
    pub vertex_lighting: Option<String>,
    /// The placement in Source space, for the simulation.
    pub source: PropPlacement,
    /// Whether it stands in the 3D skybox.
    pub skybox: bool,
}

/// The map's ambient light as an irradiance volume, placed over the world.
#[derive(Debug, Clone)]
pub struct IrradianceVolumeData {
    /// The voxel texture in Bevy's irradiance-volume layout.
    pub voxels: Handle<Image>,
    /// Where the volume's unit cube is stretched to.
    pub placement: VolumePlacement,
}

/// A loaded BSP map: its world geometry as one mesh per material, its spawn
/// points, and a bounding sphere (Bevy space) used to frame the camera when a
/// map has no spawns.
#[derive(Asset, TypePath, Debug, Clone)]
pub struct MapAsset {
    /// The world: one entry per distinct material.
    pub meshes: Vec<MapMesh>,
    /// Brush entities, each with its own meshes and placement.
    pub brush_entities: Vec<MapBrushEntity>,
    /// Player spawn points, in entity-lump order.
    pub spawns: Vec<Spawn>,
    /// The baked-lighting atlas, sampled with each mesh's `UV_1`; `None` for
    /// maps without lightmaps.
    pub lightmap: Option<Handle<Image>>,
    /// The 2D skybox as a cube-view image; `None` if the map names no sky or
    /// its textures could not be loaded.
    pub skybox: Option<Handle<Image>>,
    /// Every model placed in the map (static props and prop entities).
    pub props: Vec<PlacedProp>,
    /// Ambient light for models; `None` for maps without ambient samples.
    pub irradiance: Option<IrradianceVolumeData>,
    /// The engine's model lighting (ambient cubes plus visible world
    /// lights), queryable at any point; `None` for maps without ambient
    /// samples.
    pub lighting: Option<Arc<AmbientLighting>>,
    /// What players collide with, in Source space; `None` if it could not be
    /// read (the map is then only viewable, not playable).
    pub collision: Option<Arc<MapCollision>>,
    /// What can be seen from where, if the map has visibility data.
    pub visibility: Option<Arc<Visibility>>,
    /// The 3D skybox's camera, if the map has one.
    pub sky_camera: Option<SkyCameraData>,
    /// Every entity of the map, Source space, for the simulation to
    /// interpret (pickups, hazards).
    pub entities: Arc<[MapEntity]>,
    /// Centre of the geometry's axis-aligned bounding box, in Bevy space.
    pub center: Vec3,
    /// Half the bounding box's diagonal length, a rough "radius" for framing.
    pub radius: f32,
}

impl Default for MapAsset {
    fn default() -> Self {
        Self {
            meshes: Vec::new(),
            brush_entities: Vec::new(),
            spawns: Vec::new(),
            lightmap: None,
            skybox: None,
            props: Vec::new(),
            irradiance: None,
            lighting: None,
            collision: None,
            visibility: None,
            sky_camera: None,
            entities: Arc::new([]),
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
        let mut map = build_map(&geometry, load_context);
        map.collision = match collision::load(&bytes) {
            Ok(collision) => Some(Arc::new(collision)),
            Err(error) => {
                tracing::warn!(%error, "no collision model; the map is view-only");
                None
            }
        };
        if let Some(sky) = &geometry.sky_name {
            if let Some(cube) = load_sky(load_context, sky).await {
                map.skybox = Some(load_context.add_labeled_asset("skybox", cubemap_image(&cube)));
            } else {
                tracing::warn!(sky, "skybox textures not found; drawing no sky");
            }
        }
        Ok(map)
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

    for surface in &geometry.surfaces {
        for &position in &surface.positions {
            let p = coords::position(position);
            min = min.min(p);
            max = max.max(p);
        }
    }
    let meshes = add_meshes(load_context, "mesh", &geometry.surfaces);
    let brush_entities = geometry
        .brush_entities
        .iter()
        .enumerate()
        .map(|(index, entity)| brush_entity(load_context, index, entity))
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
            source: *spawn,
        })
        .collect();

    let lightmap = geometry
        .lightmap
        .as_ref()
        .map(|atlas| load_context.add_labeled_asset("lightmap", lightmap_image(atlas)));

    let irradiance =
        geometry
            .ambient
            .as_ref()
            .and_then(irradiance::bake)
            .map(|(image, placement)| IrradianceVolumeData {
                voxels: load_context.add_labeled_asset("irradiance", image),
                placement,
            });

    MapAsset {
        meshes,
        brush_entities,
        spawns,
        lightmap,
        skybox: None,
        props: geometry
            .props
            .iter()
            .map(|prop| placed_prop(prop, in_skybox(geometry, prop.origin)))
            .collect(),
        irradiance,
        lighting: geometry.ambient.clone().map(Arc::new),
        collision: None,
        visibility: geometry.visibility.clone().map(Arc::new),
        sky_camera: geometry.sky_camera.map(|camera| SkyCameraData {
            origin: coords::position(camera.origin),
            scale: camera.scale,
        }),
        entities: geometry.entities.clone().into(),
        center,
        radius: radius.max(1.0),
    }
}

/// Adds one labeled mesh per surface (`<prefix><index>`), paired with its
/// material.
fn add_meshes(
    load_context: &mut LoadContext,
    prefix: &str,
    surfaces: &[MapSurface],
) -> Vec<MapMesh> {
    surfaces
        .iter()
        .enumerate()
        .map(|(index, surface)| MapMesh {
            material: surface.material.clone(),
            mesh: load_context.add_labeled_asset(format!("{prefix}{index}"), mesh_of(surface)),
            clusters: surface.clusters.clone(),
            skybox: surface.skybox,
        })
        .collect()
}

/// Converts brush entity number `index` to Bevy space, adding its meshes as
/// `brush<index>_<n>` sub-assets.
fn brush_entity(
    load_context: &mut LoadContext,
    index: usize,
    entity: &BrushEntity,
) -> MapBrushEntity {
    MapBrushEntity {
        classname: entity.classname.clone(),
        model: entity.model,
        start_disabled: entity.start_disabled,
        translation: coords::position(entity.origin),
        rotation: coords::rotation(entity.angles),
        meshes: add_meshes(load_context, &format!("brush{index}_"), &entity.surfaces),
    }
}

/// Whether `point` (Source space) lies in the map's 3D skybox.
fn in_skybox(geometry: &MapGeometry, point: [f32; 3]) -> bool {
    let sky = geometry.sky_camera.and_then(|camera| camera.area);
    let area = geometry
        .visibility
        .as_ref()
        .and_then(|visibility| visibility.area_at(point));
    sky.is_some() && area == sky
}

/// Converts a Source-space placement into a [`PlacedProp`].
fn placed_prop(prop: &PropPlacement, skybox: bool) -> PlacedProp {
    PlacedProp {
        model: format!("{SOURCE}://{}", prop.model),
        translation: coords::position(prop.origin),
        rotation: coords::rotation(prop.angles),
        scale: prop.scale,
        skin: prop.skin,
        kind: prop.kind,
        vertex_lighting: prop
            .vertex_lighting
            .as_ref()
            .map(|path| format!("{SOURCE}://{path}")),
        source: prop.clone(),
        skybox,
    }
}

/// Loads and assembles the six faces of the 2D skybox named `sky`, or `None` if
/// any face is missing or undecodable.
async fn load_sky(load_context: &mut LoadContext<'_>, sky: &str) -> Option<Cubemap> {
    let mut faces = Vec::with_capacity(6);
    for suffix in skybox::SUFFIXES {
        faces.push(load_sky_face(load_context, &format!("skybox/{sky}{suffix}")).await?);
    }
    skybox::assemble([
        &faces[0], &faces[1], &faces[2], &faces[3], &faces[4], &faces[5],
    ])
}

/// Loads one sky face. The face's VMT is consulted first because its
/// `$basetexture` may name a different texture than the material itself; if
/// the VMT is missing, the texture is assumed to share the material's name.
async fn load_sky_face(
    load_context: &mut LoadContext<'_>,
    material: &str,
) -> Option<DecodedTexture> {
    let vmt_path = format!("{SOURCE}://materials/{material}.vmt");
    let texture = match load_context.read_asset_bytes(vmt_path).await {
        Ok(bytes) => vmt::parse(&String::from_utf8_lossy(&bytes))
            .ok()
            .and_then(|desc| desc.base_texture)
            .unwrap_or_else(|| material.to_owned()),
        Err(_) => material.to_owned(),
    };
    let vtf_path = format!("{SOURCE}://materials/{}.vtf", texture.replace('\\', "/"));
    let bytes = load_context.read_asset_bytes(vtf_path).await.ok()?;
    vtf::decode(&bytes).ok()
}

/// Uploads an assembled sky as a six-layer image viewed as a cubemap.
fn cubemap_image(cube: &Cubemap) -> Image {
    let size = cube.size as u32;
    let mut image = Image::new(
        Extent3d {
            width: size,
            height: size,
            depth_or_array_layers: 6,
        },
        TextureDimension::D2,
        cube.rgba8.clone(),
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    );
    image.texture_view_descriptor = Some(TextureViewDescriptor {
        dimension: Some(TextureViewDimension::Cube),
        ..Default::default()
    });
    image
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
    // Painted displacement blend travels as vertex-colour alpha (white RGB so
    // it never tints). The client draws `$basetexture2` as a second,
    // alpha-blended layer over the same mesh, which then fades in exactly
    // where the mapper painted it.
    if surface.blend.iter().any(|&alpha| alpha > 0.0) {
        let colors: Vec<[f32; 4]> = surface
            .blend
            .iter()
            .map(|&alpha| [1.0, 1.0, 1.0, alpha])
            .collect();
        mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, colors);
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
    use source_formats::props::PropSolid;

    use super::*;

    #[test]
    fn one_triangle_surface_yields_a_three_vertex_mesh() {
        let surface = MapSurface {
            material: "concrete/floor".to_owned(),
            positions: vec![[0.0, 0.0, 0.0], [16.0, 0.0, 0.0], [0.0, 0.0, 16.0]],
            normals: vec![[0.0, 1.0, 0.0]; 3],
            uvs: vec![[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]],
            lightmap_uvs: Vec::new(),
            blend: vec![0.0; 3],
            clusters: Vec::new(),
            skybox: false,
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

        assert!(
            mesh.attribute(Mesh::ATTRIBUTE_COLOR).is_none(),
            "no painted blend, no vertex colours"
        );

        let lit = MapSurface {
            lightmap_uvs: vec![[0.5, 0.5]; 3],
            blend: vec![0.0, 0.5, 1.0],
            ..surface
        };
        let lit = mesh_of(&lit);
        assert!(lit.attribute(Mesh::ATTRIBUTE_UV_1).is_some());
        assert!(lit.attribute(Mesh::ATTRIBUTE_COLOR).is_some());
    }

    #[test]
    fn placements_become_bevy_space_props() {
        let prop = placed_prop(
            &PropPlacement {
                model: "models/props_c17/oildrum001.mdl".to_owned(),
                origin: [64.0, 0.0, 0.0],
                angles: [0.0, 90.0, 0.0],
                skin: 1,
                scale: 2.0,
                kind: PropKind::Physics,
                static_index: None,
                vertex_lighting: None,
                lighting_origin: None,
                solid: PropSolid::Hull,
                spawnflags: 0,
                mass_scale: 1.0,
                name: None,
                health: 0.0,
                explode_damage: 0.0,
                explode_radius: 0.0,
                respawn: 0.0,
            },
            true,
        );
        assert!(prop.skybox);
        assert_eq!(prop.model, "source://models/props_c17/oildrum001.mdl");
        assert_eq!(prop.translation, coords::position([64.0, 0.0, 0.0]));
        // Yaw 90° turns the model's forward (+X) to Source +Y, Bevy -Z.
        assert!((prop.rotation * Vec3::X - Vec3::NEG_Z).length() < 1e-6);
        assert_eq!(
            (prop.skin, prop.scale, prop.kind),
            (1, 2.0, PropKind::Physics)
        );
    }

    #[test]
    fn sky_becomes_a_six_layer_cube_image() {
        let cube = Cubemap {
            size: 2,
            rgba8: vec![7; 6 * 2 * 2 * 4],
        };
        let image = cubemap_image(&cube);
        assert_eq!(image.texture_descriptor.size.depth_or_array_layers, 6);
        assert_eq!(
            image
                .texture_view_descriptor
                .as_ref()
                .and_then(|view| view.dimension),
            Some(TextureViewDimension::Cube)
        );
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
