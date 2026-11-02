//! Extracting renderable world geometry from a Source BSP map.
//!
//! A BSP stores the level as planes, brushes and a visibility tree. For
//! rendering we only need the *faces*: each is a convex polygon with a material
//! and texture coordinates. This module walks the visible faces, triangulates
//! them (via `vbsp`, which also handles displacement surfaces), and groups the
//! triangles by material so the renderer can build one mesh per material.
//!
//! Positions and normals are left in **Source space** (Z-up, units of inches).
//! The conversion to Bevy's coordinate system happens once, at the render
//! boundary (`rl_assets`), so the geometry here matches what the simulation and
//! collision code will later use.
//!
//! The baked lighting is extracted alongside: every lit face's luxels are
//! decoded and packed into one [`LightmapAtlas`], and each vertex gets a second
//! UV set addressing its face's block (see [`crate::lightmap`]).

use std::collections::HashMap;

use thiserror::Error;
use vbsp::Bsp;

use crate::ambient::{self, AmbientLighting};
use crate::entities::{self, MapEntity};
use crate::lightmap::{AtlasBuilder, LightStyles, LightmapAtlas, Luxel, Placement, decode_luxel};
use crate::overlays::{self, Overlay, OverlayVertex};
use crate::props::{self, PropPlacement};
use crate::visibility::{FaceRegions, Visibility};

/// BSP lump index of the low-dynamic-range lighting data the face lump's
/// `light_offset`s point into.
const LUMP_LIGHTING: usize = 8;
/// BSP lump index of the embedded pakfile (a zip archive of map-specific
/// content: patched cubemap materials, custom textures and models).
const LUMP_PAKFILE: usize = 40;
/// Light style value meaning "no lightmap in this slot".
const NO_LIGHT_STYLE: u8 = 255;
/// Sample sets per style of a face flagged `SURF_BUMPLIGHT`.
const BUMP_SAMPLE_SETS: usize = 4;
/// Largest lightmap block dimension accepted, in luxels. vbsp caps face
/// lightmaps far below this; anything larger means corrupt data.
const MAX_LIGHTMAP_SIZE: usize = 1024;
/// Size of one entry in the BSP lump directory: `fileofs`, `filelen` and
/// `version` as `i32`, then a 4-byte `fourCC`.
const LUMP_ENTRY_SIZE: usize = 16;
/// Offset of the lump directory: after the `"VBSP"` magic and the version.
const LUMP_DIRECTORY_OFFSET: usize = 8;
/// Side of the cubes the world's surfaces are split into, units: small
/// enough for visibility and frustum culling to leave most of a map out,
/// large enough to keep the number of meshes in the low thousands.
const CHUNK_SIZE: f32 = 1024.0;
/// The 3D skybox's scale when its `sky_camera` gives none.
const DEFAULT_SKY_SCALE: f32 = 16.0;
/// How far an overlay is lifted off its face, units, on top of its material's
/// depth bias.
const OVERLAY_LIFT: f32 = 0.06;

/// One mesh's worth of triangles: every three consecutive vertices form a
/// triangle (a triangle list), all sharing one `material` and lying in one
/// cube of the map (1024 units across, by each face's first
/// vertex).
#[derive(Debug, Clone, PartialEq)]
pub struct MapSurface {
    /// Material path as stored in the BSP, e.g. `concrete/concretefloor001a`
    /// (relative to `materials/`, without extension).
    pub material: String,
    /// Triangle-list vertex positions, Source space.
    pub positions: Vec<[f32; 3]>,
    /// Per-vertex normals (the face plane normal), Source space.
    pub normals: Vec<[f32; 3]>,
    /// Per-vertex texture coordinates, already divided by texture size.
    pub uvs: Vec<[f32; 2]>,
    /// Per-vertex coordinates into [`MapGeometry::lightmap`]; empty when the
    /// map has no lightmap.
    pub lightmap_uvs: Vec<[f32; 2]>,
    /// Per-vertex blend between a `WorldVertexTransition` material's two
    /// textures (0 = `$basetexture`, 1 = `$basetexture2`), painted by the mapper
    /// on displacements; 0 for every other face.
    pub blend: Vec<f32>,
    /// The visibility clusters its faces lie in, sorted; empty when not
    /// known (a face in no leaf, a map without visibility): always drawn.
    pub clusters: Vec<u16>,
    /// Whether it belongs to the 3D skybox (the area of the `sky_camera`).
    pub skybox: bool,
}

/// The 3D skybox's camera: the scenery around `origin` is drawn behind the
/// sky, as seen from `origin + eye / scale`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SkyCamera {
    /// Where the skybox's miniature of the map's centre is, Source space.
    pub origin: [f32; 3],
    /// How many times smaller than the map the skybox is built.
    pub scale: f32,
    /// The skybox's area.
    pub area: Option<u16>,
}

/// A player spawn point, in Source space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpawnPoint {
    /// Spawn origin (Source units, Z-up), at the player's feet.
    pub origin: [f32; 3],
    /// Facing yaw in degrees (Source convention: around +Z, 0 = +X).
    pub yaw: f32,
}

/// A brush entity (door, breakable, `func_brush`, ...): its own BSP model,
/// placed by the entity's origin and angles.
///
/// `vbsp` stores a brush entity's faces relative to the entity's origin, so
/// they must be moved by it (and turned by its angles) to appear in place.
#[derive(Debug, Clone, PartialEq)]
pub struct BrushEntity {
    /// The entity class, e.g. `func_door`.
    pub classname: String,
    /// Its BSP model (never 0, the world): what map logic knows it by.
    pub model: usize,
    /// Whether it starts disabled (hidden until map logic shows it).
    pub start_disabled: bool,
    /// Entity origin, Source space.
    pub origin: [f32; 3],
    /// Entity orientation, `[pitch, yaw, roll]` in degrees.
    pub angles: [f32; 3],
    /// The model's visible geometry, one entry per material, in the
    /// entity's own frame.
    pub surfaces: Vec<MapSurface>,
}

/// All visible geometry of a map, grouped by material, plus its spawn
/// points, props and lighting.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MapGeometry {
    /// The world (BSP model 0): one entry per distinct material referenced
    /// by a visible face.
    pub surfaces: Vec<MapSurface>,
    /// Brush entities with visible geometry.
    pub brush_entities: Vec<BrushEntity>,
    /// Player spawn points parsed from the map's entities.
    pub spawns: Vec<SpawnPoint>,
    /// The map's baked lighting, or `None` if it has none (unlit or
    /// HDR-only maps).
    pub lightmap: Option<LightmapAtlas>,
    /// The worldspawn `skyname` (e.g. `sky_day02_09`), naming the 2D skybox
    /// textures under `materials/skybox/`.
    pub sky_name: Option<String>,
    /// Static props and prop entities placed in the map.
    pub props: Vec<PropPlacement>,
    /// The baked ambient light models are lit by, or `None` if the map has
    /// none.
    pub ambient: Option<AmbientLighting>,
    /// Every entity of the map, for the simulation to interpret.
    pub entities: Vec<MapEntity>,
    /// What can be seen from where, if the map has visibility data.
    pub visibility: Option<Visibility>,
    /// The 3D skybox's camera, if the map has one.
    pub sky_camera: Option<SkyCamera>,
    /// The map's overlays (`info_overlay`, lump 45), clipped to the faces they
    /// lie on: one entry per material and cube of the map. `material` names a
    /// decal's material; `uvs` address its texture and `lightmap_uvs` the
    /// lightmap under it.
    pub overlays: Vec<MapSurface>,
}

/// Entity classnames that mark a place a player can spawn.
const SPAWN_CLASSNAMES: [&str; 4] = [
    "info_player_deathmatch",
    "info_player_combine",
    "info_player_rebel",
    "info_player_start",
];

/// Errors from [`load_geometry`].
#[derive(Debug, Error)]
pub enum BspError {
    /// The bytes are not a readable BSP, or geometry extraction failed.
    #[error("could not read BSP: {0}")]
    Parse(String),
}

/// Reads a BSP and returns its visible world geometry grouped by material.
///
/// Faces flagged sky, nodraw, trigger, hint or skip are skipped (via
/// [`vbsp`]'s `is_visible`). Displacement faces are tessellated.
///
/// # Errors
///
/// Returns [`BspError::Parse`] if the data is not a valid BSP. Like the other
/// decoders in this crate, it never panics on malformed input: a panic inside
/// the parser is contained and reported as an error.
pub fn load_geometry(bytes: &[u8]) -> Result<MapGeometry, BspError> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| load_geometry_inner(bytes)))
        .unwrap_or_else(|_| {
            Err(BspError::Parse(
                "BSP geometry extraction panicked on malformed data".to_owned(),
            ))
        })
}

/// What a map places for the simulation — spawn points, props and every
/// entity — without its visible geometry or lighting: what a dedicated
/// server, which draws nothing, reads.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MapContents {
    /// Player spawn points.
    pub spawns: Vec<SpawnPoint>,
    /// Static props and prop entities, in the same order as
    /// [`MapGeometry::props`].
    pub props: Vec<PropPlacement>,
    /// Every entity of the map.
    pub entities: Vec<MapEntity>,
}

/// Reads a BSP's spawn points, props and entities, skipping the costly
/// geometry and lighting [`load_geometry`] extracts.
///
/// # Errors
///
/// Returns [`BspError::Parse`] if the data is not a valid BSP; never panics.
pub fn load_contents(bytes: &[u8]) -> Result<MapContents, BspError> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let bsp = Bsp::read(bytes).map_err(|error| BspError::Parse(error.to_string()))?;
        Ok(MapContents {
            spawns: spawn_points(&bsp),
            props: props::props(&bsp),
            entities: entities::entities(&bsp),
        })
    }))
    .unwrap_or_else(|_| {
        Err(BspError::Parse(
            "BSP contents extraction panicked on malformed data".to_owned(),
        ))
    })
}

fn load_geometry_inner(bytes: &[u8]) -> Result<MapGeometry, BspError> {
    let bsp = Bsp::read(bytes).map_err(|error| BspError::Parse(error.to_string()))?;
    let visible: Vec<usize> = (0..bsp.faces.len())
        .filter(|&index| bsp.face(index).is_some_and(|face| face.is_visible()))
        .collect();

    // Pass 1: pack every lit face's lightmap into one atlas.
    let lightmaps = pack_lightmaps(&bsp, bytes, &visible);

    // Pass 2: emit triangles grouped by material, for the world (model 0)
    // and for each brush entity's model.
    let mut is_visible = vec![false; bsp.faces.len()];
    for &index in &visible {
        is_visible[index] = true;
    }
    let model_faces = |model: usize| -> Vec<usize> {
        bsp.models.get(model).map_or_else(Vec::new, |model| {
            let first = usize::try_from(model.first_face).unwrap_or(0);
            let count = usize::try_from(model.face_count).unwrap_or(0);
            (first..first.saturating_add(count))
                .filter(|&face| is_visible.get(face).copied().unwrap_or(false))
                .collect()
        })
    };
    let visibility = Visibility::load(bytes);
    let sky_camera = sky_camera(&bsp, visibility.as_ref());
    let regions = visibility
        .as_ref()
        .map(|visibility| visibility.face_regions(bsp.faces.len()))
        .unwrap_or_default();
    let world = Regions {
        faces: &regions,
        sky_area: sky_camera.and_then(|camera| camera.area),
    };
    let surfaces = build_surfaces(&bsp, &model_faces(0), lightmaps.as_ref(), &world);
    let overlays = build_overlays(&bsp, &overlays::load(bytes), lightmaps.as_ref(), &world);
    let none = Regions {
        faces: &[],
        sky_area: None,
    };
    let brush_entities = brush_entities(&bsp)
        .into_iter()
        .map(|(model, mut entity)| {
            entity.surfaces = build_surfaces(&bsp, &model_faces(model), lightmaps.as_ref(), &none);
            entity
        })
        .filter(|entity| !entity.surfaces.is_empty())
        .collect();

    Ok(MapGeometry {
        surfaces,
        brush_entities,
        spawns: spawn_points(&bsp),
        lightmap: lightmaps.map(|packed| packed.atlas),
        sky_name: sky_name(&bsp),
        props: props::props(&bsp),
        ambient: ambient::load(&bsp, bytes),
        entities: entities::entities(&bsp),
        visibility,
        sky_camera,
        overlays,
    })
}

/// The map's `sky_camera`, with the area it stands in.
fn sky_camera(bsp: &Bsp, visibility: Option<&Visibility>) -> Option<SkyCamera> {
    let entity = bsp
        .entities
        .iter()
        .find(|entity| entity.prop("classname") == Some("sky_camera"))?;
    let origin = entity.prop("origin").and_then(parse_vector)?;
    let scale = entity
        .prop("scale")
        .and_then(|scale| scale.trim().parse::<f32>().ok())
        .filter(|&scale| scale > 0.0 && scale.is_finite())
        .unwrap_or(DEFAULT_SKY_SCALE);
    Some(SkyCamera {
        origin,
        scale,
        area: visibility.and_then(|visibility| visibility.area_at(origin)),
    })
}

/// Where each face lies, for grouping surfaces: its clusters and areas
/// (none known for faces past the end), and the 3D skybox's area.
struct Regions<'a> {
    faces: &'a [FaceRegions],
    sky_area: Option<u16>,
}

impl Regions<'_> {
    /// Face `face`'s clusters, and whether it is part of the 3D skybox:
    /// every area it lies in is the skybox's (area 0, solid space, aside).
    fn of(&self, face: usize) -> (&[u16], bool) {
        let Some(region) = self.faces.get(face) else {
            return (&[], false);
        };
        let skybox = self.sky_area.is_some_and(|sky| {
            let mut areas = region.areas.iter().filter(|&&area| area != 0).peekable();
            areas.peek().is_some() && areas.all(|&area| area == sky)
        });
        (&region.clusters, skybox)
    }
}

/// Surfaces being filled: one per material, cube of the map and skybox flag.
#[derive(Default)]
struct SurfaceGroups {
    by_key: HashMap<(String, [i32; 3], bool), usize>,
    surfaces: Vec<MapSurface>,
    /// Surfaces with a face in no known cluster, which must always be drawn.
    unplaced: Vec<bool>,
}

impl SurfaceGroups {
    /// The surface for a face of `material` whose first vertex is `first`, in
    /// `clusters`.
    fn surface(
        &mut self,
        material: &str,
        first: Option<[f32; 3]>,
        clusters: &[u16],
        skybox: bool,
    ) -> &mut MapSurface {
        let chunk = first.map_or([0; 3], |first| {
            first.map(|coordinate| (coordinate / CHUNK_SIZE).floor() as i32)
        });
        let index = *self
            .by_key
            .entry((material.to_owned(), chunk, skybox))
            .or_insert_with(|| {
                self.surfaces.push(MapSurface {
                    material: material.to_owned(),
                    positions: Vec::new(),
                    normals: Vec::new(),
                    uvs: Vec::new(),
                    lightmap_uvs: Vec::new(),
                    blend: Vec::new(),
                    clusters: Vec::new(),
                    skybox,
                });
                self.unplaced.push(false);
                self.surfaces.len() - 1
            });
        self.unplaced[index] |= clusters.is_empty();
        let surface = &mut self.surfaces[index];
        for &cluster in clusters {
            if let Err(at) = surface.clusters.binary_search(&cluster) {
                surface.clusters.insert(at, cluster);
            }
        }
        surface
    }

    /// The finished surfaces.
    fn finish(mut self) -> Vec<MapSurface> {
        for (surface, unplaced) in self.surfaces.iter_mut().zip(self.unplaced) {
            if unplaced {
                surface.clusters.clear();
            }
        }
        self.surfaces
    }
}

/// One vertex of a face's triangle list.
#[derive(Debug, Clone, Copy, PartialEq)]
struct FaceVertex {
    position: [f32; 3],
    uv: [f32; 2],
    /// Coordinate into the lightmap atlas, if the map is lit.
    lightmap_uv: Option<[f32; 2]>,
    blend: f32,
}

/// Face `index`'s triangle list, tessellating a displacement, with its UVs.
fn face_vertices(
    face: &vbsp::Handle<'_, vbsp::Face>,
    index: usize,
    lightmaps: Option<&PackedLightmaps>,
) -> Vec<FaceVertex> {
    let texture = face.texture();
    let displaced = face.displacement().map(|displacement| {
        let steps = 1usize << displacement.power.clamp(0, 8);
        let grid: Vec<DisplacedVertex> = displacement
            .displacement_vertices()
            .zip(displacement.displaced_vertices())
            .enumerate()
            .map(|(grid_index, (vertex, position))| {
                let offset = vertex.displacement();
                DisplacedVertex::new(
                    [position.x, position.y, position.z],
                    [offset.x, offset.y, offset.z],
                    vertex.alpha,
                    grid_fraction(grid_index, steps),
                )
            })
            .collect();
        triangulated_grid(steps)
            .map(|grid_index| grid.get(grid_index).copied())
            .collect::<Vec<Option<DisplacedVertex>>>()
    });
    // `vertex_positions` yields an already-triangulated vertex stream
    // (triangle list), tessellating displacement faces along the way; the
    // per-vertex data above follows the same order.
    face.vertex_positions()
        .enumerate()
        .map(|(vertex, position)| {
            let position = [position.x, position.y, position.z];
            let extra = displaced
                .as_ref()
                .and_then(|grid| grid.get(vertex).copied().flatten());
            // A displacement stretches its base face's texture over its surface
            // rather than projecting it onto the displaced shape.
            let base = extra.map_or(position, |extra| extra.base);
            FaceVertex {
                position,
                uv: texture.uv(vbsp::Vector {
                    x: base[0],
                    y: base[1],
                    z: base[2],
                }),
                lightmap_uv: lightmaps.map(|lightmaps| match extra {
                    Some(extra) => lightmaps.displaced_uv(index, extra.fraction),
                    None => lightmaps.uv(index, face, &texture, position),
                }),
                blend: extra.map_or(0.0, |extra| extra.blend),
            }
        })
        .collect()
}

/// Triangulates `faces` into one [`MapSurface`] per material, with texture
/// UVs, lightmap UVs (when the map is lit) and displacement blend alphas.
fn build_surfaces(
    bsp: &Bsp,
    faces: &[usize],
    lightmaps: Option<&PackedLightmaps>,
    regions: &Regions,
) -> Vec<MapSurface> {
    let mut groups = SurfaceGroups::default();
    for &index in faces {
        let Some(face) = bsp.face(index) else {
            continue;
        };
        let normal = face.normal();
        let normal = [normal.x, normal.y, normal.z];
        let (clusters, skybox) = regions.of(index);
        let vertices = face_vertices(&face, index, lightmaps);
        let first = vertices.first().map(|vertex| vertex.position);
        let surface = groups.surface(face.texture().name(), first, clusters, skybox);
        for vertex in vertices {
            surface.blend.push(vertex.blend);
            surface.positions.push(vertex.position);
            surface.normals.push(normal);
            surface.uvs.push(vertex.uv);
            if let Some(lightmap_uv) = vertex.lightmap_uv {
                surface.lightmap_uvs.push(lightmap_uv);
            }
        }
    }
    groups.finish()
}

/// Clips each overlay to the faces it lists, into one [`MapSurface`] per
/// material and cube of the map. Its vertices keep the lightmap coordinates of
/// the face under them, and sit a little above it.
fn build_overlays(
    bsp: &Bsp,
    overlays: &[Overlay],
    lightmaps: Option<&PackedLightmaps>,
    regions: &Regions,
) -> Vec<MapSurface> {
    let mut groups = SurfaceGroups::default();
    for overlay in overlays {
        let Some(info) = bsp.texture_info(overlay.texinfo) else {
            continue;
        };
        let material = info.name().to_ascii_lowercase();
        for &index in &overlay.faces {
            let Some(face) = bsp.face(index) else {
                continue;
            };
            let normal = face.normal();
            let normal = [normal.x, normal.y, normal.z];
            let (clusters, skybox) = regions.of(index);
            let vertices = face_vertices(&face, index, lightmaps);
            for triangle in vertices.as_chunks::<3>().0 {
                let corners = triangle.map(|vertex| OverlayVertex {
                    position: vertex.position,
                    lightmap_uv: vertex.lightmap_uv.unwrap_or_default(),
                    uv: [0.0; 2],
                });
                let clipped = overlay.clip(corners);
                let Some(first) = clipped.first() else {
                    continue;
                };
                let surface = groups.surface(&material, Some(first[0].position), clusters, skybox);
                for vertex in clipped.iter().flatten() {
                    surface.positions.push(std::array::from_fn(|axis| {
                        vertex.position[axis] + normal[axis] * OVERLAY_LIFT
                    }));
                    surface.normals.push(normal);
                    surface.uvs.push(vertex.uv);
                    surface.blend.push(0.0);
                    if lightmaps.is_some() {
                        surface.lightmap_uvs.push(vertex.lightmap_uv);
                    }
                }
            }
        }
    }
    groups.finish()
}

/// What a vertex of a displacement's grid carries besides its position.
#[derive(Debug, Clone, Copy, PartialEq)]
struct DisplacedVertex {
    /// Where the vertex is on the flat base face, before its displacement.
    base: [f32; 3],
    /// Its blend towards a `WorldVertexTransition`'s second texture, 0 to 1.
    blend: f32,
    /// How far along the grid it is, from 0 to 1, as `[column, row]`: which
    /// places it in the face's block of luxels.
    fraction: [f32; 2],
}

impl DisplacedVertex {
    /// The vertex displaced to `position` by `offset`, painted with `alpha`
    /// (0 to 255), at `fraction` of its grid.
    fn new(position: [f32; 3], offset: [f32; 3], alpha: f32, fraction: [f32; 2]) -> Self {
        Self {
            base: std::array::from_fn(|axis| position[axis] - offset[axis]),
            blend: alpha / 255.0,
            fraction,
        }
    }
}

/// How far along a displacement's grid of `steps + 1` vertices a side vertex
/// `grid_index` is, from 0 to 1, as `[column, row]`. The file stores the grid
/// row by row, the rows running from the start corner along the base face's
/// next edge.
fn grid_fraction(grid_index: usize, steps: usize) -> [f32; 2] {
    let row_length = steps + 1;
    [grid_index % row_length, grid_index / row_length].map(|at| at as f32 / steps as f32)
}

/// Every drawable brush entity (an entity whose `model` is `*<n>`, n ≥ 1)
/// with the index of its BSP model; surfaces are filled in by the caller.
/// Entities that render nothing (`rendermode 10`) are skipped, as are
/// references to missing models.
fn brush_entities(bsp: &Bsp) -> Vec<(usize, BrushEntity)> {
    bsp.entities
        .iter()
        .filter_map(|entity| {
            let model: usize = entity.prop("model")?.strip_prefix('*')?.parse().ok()?;
            if model == 0 || model >= bsp.models.len() {
                return None;
            }
            if entity.prop("rendermode") == Some("10") {
                return None;
            }
            Some((
                model,
                BrushEntity {
                    classname: entity.prop("classname").unwrap_or_default().to_owned(),
                    model,
                    start_disabled: entity.prop("StartDisabled") == Some("1"),
                    origin: entity
                        .prop("origin")
                        .and_then(parse_vector)
                        .unwrap_or_default(),
                    angles: entity
                        .prop("angles")
                        .and_then(parse_vector)
                        .unwrap_or_default(),
                    surfaces: Vec::new(),
                },
            ))
        })
        .collect()
}

/// Every lit face's lightmap, packed into one atlas.
struct PackedLightmaps {
    atlas: LightmapAtlas,
    placements: Vec<Placement>,
    /// Atlas block of each lit face and the block's size in luxels, by face
    /// index.
    face_blocks: HashMap<usize, (usize, [usize; 2])>,
    /// A 1×1 full-bright block for faces without baked light (e.g. unlit
    /// materials), which Source draws at full brightness.
    full_bright: usize,
}

impl PackedLightmaps {
    /// Atlas UV of the point `position` of flat face `index`.
    fn uv(
        &self,
        index: usize,
        face: &vbsp::Face,
        texture: &vbsp::TextureInfo,
        position: [f32; 3],
    ) -> [f32; 2] {
        let Some(&(block, size)) = self.face_blocks.get(&index) else {
            return self.placements[self.full_bright].uv(&self.atlas, 0.0, 0.0);
        };
        let axes = [texture.light_map_scale, texture.light_map_transform];
        let [s, t] = luxel_coordinates(axes, face.light_map_texture_min, size, position);
        self.placements[block].uv(&self.atlas, s, t)
    }

    /// Atlas UV of the vertex of displacement face `index` at `fraction` of its
    /// grid. A displacement's block is not a projection of its surface: `vrad`
    /// spreads the block's luxels evenly over the grid, columns along *s* and
    /// rows along *t*, whichever way the lightmap axes point. So the grid's
    /// edges meet the block's outer edges, half a luxel past the outer centres.
    fn displaced_uv(&self, index: usize, fraction: [f32; 2]) -> [f32; 2] {
        let Some(&(block, size)) = self.face_blocks.get(&index) else {
            return self.placements[self.full_bright].uv(&self.atlas, 0.0, 0.0);
        };
        let [s, t] = [0, 1].map(|axis| fraction[axis] * size[axis] as f32 - 0.5);
        self.placements[block].uv(&self.atlas, s, t)
    }
}

/// A point's coordinate in its face's block of luxels: its projection onto the
/// lightmap axes (`[x, y, z, offset]` for *s* and *t*), relative to the face's
/// first luxel, whose centre is at 0. Clamped to the block and its border, so
/// that a point just off the face never samples a neighbouring one.
fn luxel_coordinates(
    axes: [[f32; 4]; 2],
    mins: [i32; 2],
    size: [usize; 2],
    point: [f32; 3],
) -> [f32; 2] {
    std::array::from_fn(|i| {
        let [x, y, z, offset] = axes[i];
        let coordinate = x * point[0] + y * point[1] + z * point[2] + offset;
        (coordinate - mins[i] as f32).clamp(-0.5, size[i] as f32 - 0.5)
    })
}

/// Decodes and packs the lightmaps of the `visible` faces, or `None` if the
/// map has no lighting lump or no lit faces.
fn pack_lightmaps(bsp: &Bsp, bytes: &[u8], visible: &[usize]) -> Option<PackedLightmaps> {
    let lighting = lump(bytes, LUMP_LIGHTING).filter(|data| !data.is_empty())?;
    let mut builder = AtlasBuilder::new();
    let full_bright = builder.add(1, 1, vec![[1.0; 3]]);
    let styles = LightStyles::new(&entities::entities(bsp));
    let mut face_blocks = HashMap::new();
    for &index in visible {
        if let Some((width, height, luxels)) = bsp
            .face(index)
            .and_then(|face| face_luxels(&face, lighting, &styles))
        {
            face_blocks.insert(index, (builder.add(width, height, luxels), [width, height]));
        }
    }
    if face_blocks.is_empty() {
        return None;
    }
    let (atlas, placements) = builder.pack();
    Some(PackedLightmaps {
        atlas,
        placements,
        face_blocks,
        full_bright,
    })
}

/// The worldspawn `skyname`, if set.
fn sky_name(bsp: &Bsp) -> Option<String> {
    bsp.entities
        .iter()
        .find(|entity| entity.prop("classname") == Some("worldspawn"))
        .and_then(|world| world.prop("skyname"))
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
}

/// Every player spawn entity with a parsable origin.
fn spawn_points(bsp: &Bsp) -> Vec<SpawnPoint> {
    bsp.entities
        .iter()
        .filter(|entity| {
            entity
                .prop("classname")
                .is_some_and(|class| SPAWN_CLASSNAMES.contains(&class))
        })
        .filter_map(|entity| {
            let origin = parse_vector(entity.prop("origin")?)?;
            // `angles` is "pitch yaw roll"; only yaw matters for a spawn.
            let yaw = entity
                .prop("angles")
                .and_then(|angles| angles.split_whitespace().nth(1)?.parse().ok())
                .unwrap_or(0.0);
            Some(SpawnPoint { origin, yaw })
        })
        .collect()
}

/// Indices into a displacement's `(steps + 1)²` vertex grid, two triangles per
/// cell, in exactly the order `vbsp`'s `triangulated_displaced_vertices` emits
/// positions, so per-vertex data (blend alpha) lines up with them.
fn triangulated_grid(steps: usize) -> impl Iterator<Item = usize> {
    let index = move |x: usize, y: usize| y * (steps + 1) + x;
    (0..steps)
        .flat_map(move |x| (0..steps).map(move |y| (x, y)))
        .flat_map(move |(x, y)| {
            [
                index(x, y),
                index(x + 1, y),
                index(x, y + 1),
                index(x + 1, y),
                index(x + 1, y + 1),
                index(x, y + 1),
            ]
        })
}

/// Decodes a face's lightmaps into one: its `width × height` luxels, or `None`
/// if the face has no baked light or its data is out of range.
fn face_luxels(
    face: &vbsp::Handle<'_, vbsp::Face>,
    lighting: &[u8],
    styles: &LightStyles,
) -> Option<(usize, usize, Vec<Luxel>)> {
    let offset = usize::try_from(face.light_offset).ok()?;
    let width = usize::try_from(face.light_map_texture_size[0]).ok()? + 1;
    let height = usize::try_from(face.light_map_texture_size[1]).ok()? + 1;
    if width > MAX_LIGHTMAP_SIZE || height > MAX_LIGHTMAP_SIZE {
        return None;
    }
    let bumped = face.texture().flags.contains(vbsp::TextureFlags::BUMPLIGHT);
    let block = LightBlock {
        offset,
        luxels: width * height,
        bumped,
    };
    let luxels = block.bake(lighting, face.styles, styles)?;
    Some((width, height, luxels))
}

/// Where a face's samples lie in the lighting lump.
#[derive(Debug, Clone, Copy)]
struct LightBlock {
    /// Byte offset of the first sample.
    offset: usize,
    /// Luxels in one sample set.
    luxels: usize,
    /// Whether each style holds four sample sets (flat, then the three bump
    /// basis directions) rather than one.
    bumped: bool,
}

impl LightBlock {
    /// The sum of the block's style slots, each scaled by its style's
    /// brightness: what the engine computes each frame, as of the map's start.
    ///
    /// Slots are filled in order until the first [`NO_LIGHT_STYLE`]. Only the
    /// flat set of a bumped face is read, as rustlife does not bump its
    /// lightmaps. Returns `None` if the face has no slots or a slot's samples
    /// lie outside `lighting`.
    fn bake(self, lighting: &[u8], slots: [u8; 4], styles: &LightStyles) -> Option<Vec<Luxel>> {
        if slots[0] == NO_LIGHT_STYLE {
            return None;
        }
        let set = self.luxels.checked_mul(4)?;
        let stride = set.checked_mul(if self.bumped { BUMP_SAMPLE_SETS } else { 1 })?;
        let mut baked = vec![[0.0; 3]; self.luxels];
        for (slot, &style) in slots
            .iter()
            .take_while(|&&style| style != NO_LIGHT_STYLE)
            .enumerate()
        {
            let start = self.offset.checked_add(slot.checked_mul(stride)?)?;
            let data = lighting.get(start..start.checked_add(set)?)?;
            let weight = styles.weight(style);
            for (total, &bytes) in baked.iter_mut().zip(data.as_chunks::<4>().0) {
                let luxel = decode_luxel(bytes);
                for (channel, value) in total.iter_mut().zip(luxel) {
                    *channel += value * weight;
                }
            }
        }
        Some(baked)
    }
}

/// Returns the bytes of lump `index`, or `None` if the header is invalid or the
/// lump lies outside the file. Reads only the lump directory.
pub(crate) fn lump(bytes: &[u8], index: usize) -> Option<&[u8]> {
    let start = usize::try_from(lump_field(bytes, index, 0)?).ok()?;
    let length = usize::try_from(lump_field(bytes, index, 1)?).ok()?;
    bytes.get(start..start.checked_add(length)?)
}

/// Returns the format version of lump `index` from the lump directory, or
/// `None` if the header is invalid.
pub(crate) fn lump_version(bytes: &[u8], index: usize) -> Option<i32> {
    lump_field(bytes, index, 2)
}

/// Reads field `field` (0 = offset, 1 = length, 2 = version) of lump `index`'s
/// directory entry.
fn lump_field(bytes: &[u8], index: usize, field: usize) -> Option<i32> {
    if bytes.get(0..4)? != b"VBSP" {
        return None;
    }
    let at = LUMP_DIRECTORY_OFFSET + index * LUMP_ENTRY_SIZE + field * 4;
    Some(i32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

/// Parses a KeyValues vector such as `"-1024 512 64"`.
pub(crate) fn parse_vector(text: &str) -> Option<[f32; 3]> {
    let mut parts = text.split_whitespace().map(str::parse::<f32>);
    let vector = [
        parts.next()?.ok()?,
        parts.next()?.ok()?,
        parts.next()?.ok()?,
    ];
    Some(vector)
}

/// Returns the raw bytes of a BSP's embedded pakfile (lump 40) — a zip archive
/// — or `None` if the map has no pakfile or the header is too short or invalid.
///
/// The caller mounts these bytes as a zip to make the map's own materials and
/// textures available while that map is loaded. This reads only the lump
/// directory, so it is cheap and never allocates the whole file.
///
/// Compressed lumps (LZMA) are not unpacked here; an LZMA-compressed pakfile
/// would be returned as-is and fail to mount as a zip, which the caller handles
/// by simply not mounting it. HL2DM maps store the pakfile uncompressed.
#[must_use]
pub fn pakfile_bytes(bytes: &[u8]) -> Option<Vec<u8>> {
    lump(bytes, LUMP_PAKFILE)
        .filter(|data| !data.is_empty())
        .map(<[u8]>::to_vec)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_bsp_bytes_error_without_panicking() {
        assert!(load_geometry(b"not a bsp").is_err());
        assert!(load_geometry(&[]).is_err());
        assert!(load_contents(b"not a bsp").is_err());
        assert!(load_contents(&[]).is_err());
    }

    #[test]
    fn displacement_grid_triangulation_covers_each_cell_twice() {
        // A power-1 displacement: 2x2 cells over a 3x3 vertex grid.
        let indices: Vec<usize> = triangulated_grid(2).collect();
        assert_eq!(indices.len(), 2 * 2 * 6);
        assert!(indices.iter().all(|&i| i < 9));
        // First cell's two triangles share the cell's diagonal.
        assert_eq!(&indices[..6], &[0, 1, 3, 1, 4, 3]);
    }

    /// A packed atlas holding one `size` block, for face 7.
    fn one_face_atlas(size: [usize; 2]) -> PackedLightmaps {
        let mut builder = AtlasBuilder::new();
        let full_bright = builder.add(1, 1, vec![[1.0; 3]]);
        let block = builder.add(size[0], size[1], vec![[0.5; 3]; size[0] * size[1]]);
        let (atlas, placements) = builder.pack();
        PackedLightmaps {
            atlas,
            placements,
            face_blocks: HashMap::from([(7, (block, size))]),
            full_bright,
        }
    }

    #[test]
    fn the_grid_is_stored_row_by_row() {
        assert_eq!(grid_fraction(0, 4), [0.0, 0.0]);
        assert_eq!(grid_fraction(4, 4), [1.0, 0.0], "the first row's end");
        assert_eq!(grid_fraction(5, 4), [0.0, 0.25], "the next row's start");
        assert_eq!(grid_fraction(24, 4), [1.0, 1.0]);
    }

    #[test]
    fn a_displaced_vertex_remembers_its_place_on_the_base_face() {
        let vertex = DisplacedVertex::new([24.0, 32.0, 48.0], [24.0, 0.0, 48.0], 255.0, [0.5, 1.0]);
        assert_eq!(vertex.base, [0.0, 32.0, 0.0]);
        assert_eq!(vertex.blend, 1.0);
        assert_eq!(vertex.fraction, [0.5, 1.0]);
    }

    #[test]
    fn a_displacements_grid_spans_its_whole_block_of_luxels() {
        let packed = one_face_atlas([36, 10]);
        let place = packed.placements[packed.face_blocks[&7].0];
        let (width, height) = (packed.atlas.width as f32, packed.atlas.height as f32);
        let (left, top) = (place.x as f32, place.y as f32);
        let expect = |[column, row]: [f32; 2]| [(left + column) / width, (top + row) / height];
        // Columns run along s and rows along t, to the outer edges of the
        // block: dm_underpass's lit floors land where the native game has them
        // only this way, and its faces point their axes all ways.
        assert_eq!(packed.displaced_uv(7, [0.0, 0.0]), expect([0.0, 0.0]));
        assert_eq!(packed.displaced_uv(7, [1.0, 0.0]), expect([36.0, 0.0]));
        assert_eq!(packed.displaced_uv(7, [0.0, 1.0]), expect([0.0, 10.0]));
        assert_eq!(packed.displaced_uv(7, [0.5, 0.5]), expect([18.0, 5.0]));
    }

    #[test]
    fn a_face_without_a_block_is_drawn_full_bright() {
        let packed = one_face_atlas([2, 2]);
        let bright = packed.placements[packed.full_bright].uv(&packed.atlas, 0.0, 0.0);
        assert_eq!(packed.displaced_uv(8, [0.5, 0.5]), bright);
    }

    #[test]
    fn a_flat_faces_luxels_are_its_projection_onto_the_lightmap_axes() {
        let axes = [[1.0 / 16.0, 0.0, 0.0, 2.0], [0.0, 1.0 / 16.0, 0.0, -1.0]];
        // Luxel (0, 0) is centred where the axes give the face's minimum.
        assert_eq!(
            luxel_coordinates(axes, [2, -1], [5, 5], [0.0, 0.0, 9.0]),
            [0.0, 0.0]
        );
        assert_eq!(
            luxel_coordinates(axes, [2, -1], [5, 5], [32.0, 48.0, 0.0]),
            [2.0, 3.0]
        );
    }

    #[test]
    fn luxel_coordinates_are_clamped_to_the_block_and_its_border() {
        let axes = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0]];
        assert_eq!(
            luxel_coordinates(axes, [0, 0], [4, 2], [-3.0, 9.0, 0.0]),
            [-0.5, 1.5]
        );
    }

    /// Samples of one luxel per style slot: `value` at exponent 0.
    fn samples(values: &[u8]) -> Vec<u8> {
        values.iter().flat_map(|&v| [v, v, v, 0]).collect()
    }

    #[test]
    fn a_faces_style_slots_add_up_by_brightness() {
        let lighting = samples(&[9, 51, 102, 204]);
        let block = LightBlock {
            offset: 4,
            luxels: 1,
            bumped: false,
        };
        let styles = LightStyles::new(&[
            MapEntity::new("light", [0.0; 3]).with("style", "32"),
            MapEntity::new("light", [0.0; 3])
                .with("style", "33")
                .with("spawnflags", "1"),
        ]);
        let baked = block
            .bake(&lighting, [0, 32, 33, 255], &styles)
            .expect("in range");
        // 51 and 102 lit, 204 belongs to a dark light; 9 is before the face.
        assert!((baked[0][1] - 153.0 / 255.0).abs() < 1e-5, "{baked:?}");
    }

    #[test]
    fn a_bumped_face_keeps_four_sample_sets_per_style() {
        let lighting = samples(&[10, 90, 90, 90, 20, 90, 90, 90]);
        let block = LightBlock {
            offset: 0,
            luxels: 1,
            bumped: true,
        };
        let baked = block
            .bake(&lighting, [0, 32, 255, 255], &LightStyles::default())
            .expect("in range");
        assert!((baked[0][0] - 30.0 / 255.0).abs() < 1e-5, "{baked:?}");
    }

    #[test]
    fn a_face_without_slots_or_with_slots_out_of_range_has_no_light() {
        let lighting = samples(&[10]);
        let block = LightBlock {
            offset: 0,
            luxels: 1,
            bumped: false,
        };
        let styles = LightStyles::default();
        assert_eq!(block.bake(&lighting, [255; 4], &styles), None);
        assert_eq!(block.bake(&lighting, [0, 32, 255, 255], &styles), None);
    }

    #[test]
    fn parses_keyvalues_vectors() {
        assert_eq!(parse_vector("-1024 512 64.5"), Some([-1024.0, 512.0, 64.5]));
        assert_eq!(parse_vector("1 2"), None);
        assert_eq!(parse_vector("1 x 3"), None);
    }

    #[test]
    fn pakfile_bytes_rejects_non_bsp_and_short_input() {
        assert_eq!(pakfile_bytes(b""), None);
        assert_eq!(pakfile_bytes(b"not a bsp"), None);
        // Valid magic but truncated before the lump directory.
        assert_eq!(pakfile_bytes(b"VBSP\x14\0\0\0"), None);
    }

    /// Smoke test against a real map. Set `RUSTLIFE_TEST_BSP` to a `.bsp` path
    /// and run with `--ignored`; skipped silently when the variable is unset so
    /// CI without an install still passes.
    #[test]
    #[ignore = "requires a BSP file; set RUSTLIFE_TEST_BSP"]
    fn loads_real_map_geometry() {
        let Ok(path) = std::env::var("RUSTLIFE_TEST_BSP") else {
            return;
        };
        let bytes = std::fs::read(&path).expect("read bsp");
        let geometry = load_geometry(&bytes).expect("extract geometry");
        let triangles: usize = geometry
            .surfaces
            .iter()
            .map(|s| s.positions.len())
            .sum::<usize>()
            / 3;
        eprintln!(
            "{path}: {} materials, {triangles} triangles",
            geometry.surfaces.len()
        );
        assert!(!geometry.surfaces.is_empty(), "a real map has surfaces");
        assert!(triangles > 0, "a real map has triangles");
        eprintln!("spawns: {}", geometry.spawns.len());
        eprintln!("sky: {:?}", geometry.sky_name);
        assert!(geometry.sky_name.is_some(), "worldspawn names a sky");
        assert!(!geometry.spawns.is_empty(), "a DM map has spawn points");
        let contents = load_contents(&bytes).expect("extract contents");
        assert_eq!(contents.spawns, geometry.spawns);
        assert_eq!(contents.props, geometry.props, "props in the same order");
        assert_eq!(contents.entities, geometry.entities);
        assert!(
            geometry
                .entities
                .iter()
                .any(|entity| entity.classname == "worldspawn"),
            "every map has a worldspawn entity"
        );

        let lightmap = geometry.lightmap.as_ref().expect("map has baked lighting");
        let lit = lightmap
            .luxels
            .iter()
            .filter(|l| l.iter().any(|&c| c > 0.0))
            .count();
        eprintln!(
            "lightmap: {}x{} luxels, {lit} non-black",
            lightmap.width, lightmap.height
        );
        let blended = geometry
            .surfaces
            .iter()
            .filter(|s| s.blend.iter().any(|&b| b > 0.0))
            .count();
        eprintln!("surfaces with painted blend: {blended}");
        eprintln!("brush entities: {}", geometry.brush_entities.len());
        let entity_surfaces = geometry.brush_entities.iter().flat_map(|e| &e.surfaces);
        for surface in geometry.surfaces.iter().chain(entity_surfaces) {
            assert_eq!(surface.blend.len(), surface.positions.len());
            assert_eq!(surface.lightmap_uvs.len(), surface.positions.len());
            assert!(
                surface
                    .lightmap_uvs
                    .iter()
                    .all(|uv| uv.iter().all(|c| (0.0..=1.0).contains(c))),
                "lightmap UVs stay inside the atlas ({})",
                surface.material
            );
        }

        eprintln!("overlay surfaces: {}", geometry.overlays.len());
        for overlay in &geometry.overlays {
            assert_eq!(overlay.uvs.len(), overlay.positions.len());
            assert_eq!(overlay.lightmap_uvs.len(), overlay.positions.len());
            assert_eq!(overlay.positions.len() % 3, 0);
            assert!(
                overlay
                    .uvs
                    .iter()
                    .flatten()
                    .all(|&c| (-1e-3..=1.0 + 1e-3).contains(&c)),
                "overlay coordinates stay on its texture ({})",
                overlay.material
            );
        }

        // The embedded pakfile should be a mountable zip (starts with "PK").
        let pak = pakfile_bytes(&bytes).expect("map has a pakfile");
        assert_eq!(&pak[0..2], b"PK", "pakfile is a zip archive");
        eprintln!("pakfile: {} bytes", pak.len());

        eprintln!("props: {}", geometry.props.len());
        assert!(!geometry.props.is_empty(), "a stock map places props");
        assert!(
            geometry.ambient.is_some(),
            "a stock map bakes ambient light"
        );
    }
}
