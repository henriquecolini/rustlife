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

use crate::lightmap::{AtlasBuilder, LightmapAtlas, Luxel, Placement, decode_luxel};

/// BSP lump index of the low-dynamic-range lighting data the face lump's
/// `light_offset`s point into.
const LUMP_LIGHTING: usize = 8;
/// BSP lump index of the embedded pakfile (a zip archive of map-specific
/// content: patched cubemap materials, custom textures and models).
const LUMP_PAKFILE: usize = 40;
/// Light style value meaning "no lightmap in this slot".
const NO_LIGHT_STYLE: u8 = 255;
/// Largest lightmap block dimension accepted, in luxels. vbsp caps face
/// lightmaps far below this; anything larger means corrupt data.
const MAX_LIGHTMAP_SIZE: usize = 1024;
/// Size of one entry in the BSP lump directory: `fileofs`, `filelen` and
/// `version` as `i32`, then a 4-byte `fourCC`.
const LUMP_ENTRY_SIZE: usize = 16;
/// Offset of the lump directory: after the `"VBSP"` magic and the version.
const LUMP_DIRECTORY_OFFSET: usize = 8;

/// One mesh's worth of triangles: every three consecutive vertices form a
/// triangle (a triangle list), all sharing one `material`.
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
}

/// A player spawn point, in Source space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpawnPoint {
    /// Spawn origin (Source units, Z-up), at the player's feet.
    pub origin: [f32; 3],
    /// Facing yaw in degrees (Source convention: around +Z, 0 = +X).
    pub yaw: f32,
}

/// All visible world geometry of a map, grouped by material, plus its spawn
/// points.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MapGeometry {
    /// One entry per distinct material referenced by a visible face.
    pub surfaces: Vec<MapSurface>,
    /// Player spawn points parsed from the map's entities.
    pub spawns: Vec<SpawnPoint>,
    /// The map's baked lighting, or `None` if it has none (unlit or
    /// HDR-only maps).
    pub lightmap: Option<LightmapAtlas>,
    /// The worldspawn `skyname` (e.g. `sky_day02_09`), naming the 2D skybox
    /// textures under `materials/skybox/`.
    pub sky_name: Option<String>,
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

fn load_geometry_inner(bytes: &[u8]) -> Result<MapGeometry, BspError> {
    let bsp = Bsp::read(bytes).map_err(|error| BspError::Parse(error.to_string()))?;
    let visible: Vec<usize> = (0..bsp.faces.len())
        .filter(|&index| bsp.face(index).is_some_and(|face| face.is_visible()))
        .collect();

    // Pass 1: pack every lit face's lightmap into one atlas.
    let lightmaps = pack_lightmaps(&bsp, bytes, &visible);

    // Pass 2: emit triangles grouped by material.
    let mut by_material: HashMap<String, usize> = HashMap::new();
    let mut surfaces: Vec<MapSurface> = Vec::new();
    for &index in &visible {
        let Some(face) = bsp.face(index) else {
            continue;
        };

        let texture = face.texture();
        let material = texture.name().to_owned();
        let normal = face.normal();
        let normal = [normal.x, normal.y, normal.z];

        let surface_index = *by_material.entry(material.clone()).or_insert_with(|| {
            surfaces.push(MapSurface {
                material: material.clone(),
                positions: Vec::new(),
                normals: Vec::new(),
                uvs: Vec::new(),
                lightmap_uvs: Vec::new(),
                blend: Vec::new(),
            });
            surfaces.len() - 1
        });
        let surface = &mut surfaces[surface_index];

        let blend = face.displacement().map(|displacement| {
            let alphas: Vec<f32> = displacement
                .displacement_vertices()
                .map(|vertex| vertex.alpha / 255.0)
                .collect();
            triangulated_grid(1usize << displacement.power.clamp(0, 8))
                .map(|grid_index| alphas.get(grid_index).copied().unwrap_or(0.0))
                .collect::<Vec<f32>>()
        });

        // `vertex_positions` yields an already-triangulated vertex stream
        // (triangle list), tessellating displacement faces along the way; the
        // blend alphas above follow the same order.
        for (vertex, position) in face.vertex_positions().enumerate() {
            let position = [position.x, position.y, position.z];
            let alpha = blend.as_ref().and_then(|alphas| alphas.get(vertex));
            surface.blend.push(alpha.copied().unwrap_or(0.0));
            surface.positions.push(position);
            surface.normals.push(normal);
            surface.uvs.push(texture.uv(vbsp::Vector {
                x: position[0],
                y: position[1],
                z: position[2],
            }));
            if let Some(lightmaps) = &lightmaps {
                surface
                    .lightmap_uvs
                    .push(lightmaps.uv(index, &face, &texture, position));
            }
        }
    }

    Ok(MapGeometry {
        surfaces,
        spawns: spawn_points(&bsp),
        lightmap: lightmaps.map(|packed| packed.atlas),
        sky_name: sky_name(&bsp),
    })
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
    /// Atlas UV of a vertex of face `index`: the vertex projected onto the
    /// face's lightmap axes, relative to the face's luxel origin. Displaced
    /// vertices project too; their offsets lie mostly along the normal, which
    /// the projection discards, but can stray slightly past the face's extent,
    /// so the coordinate is clamped to the block (and its border) to never
    /// sample a neighbouring face.
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
        let [s, t] = [texture.light_map_scale, texture.light_map_transform].map(|axis| {
            axis[0] * position[0] + axis[1] * position[1] + axis[2] * position[2] + axis[3]
        });
        let mins = face.light_map_texture_min;
        let [s, t] = [(s, mins[0], size[0]), (t, mins[1], size[1])]
            .map(|(coord, min, size)| (coord - min as f32).clamp(-0.5, size as f32 - 0.5));
        self.placements[block].uv(&self.atlas, s, t)
    }
}

/// Decodes and packs the lightmaps of the `visible` faces, or `None` if the
/// map has no lighting lump or no lit faces.
fn pack_lightmaps(bsp: &Bsp, bytes: &[u8], visible: &[usize]) -> Option<PackedLightmaps> {
    let lighting = lump(bytes, LUMP_LIGHTING).filter(|data| !data.is_empty())?;
    let mut builder = AtlasBuilder::new();
    let full_bright = builder.add(1, 1, vec![[1.0; 3]]);
    let mut face_blocks = HashMap::new();
    for &index in visible {
        if let Some((width, height, luxels)) = bsp
            .face(index)
            .and_then(|face| face_luxels(&face, lighting))
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

/// Decodes a face's style-0 lightmap: its `width × height` luxels, or `None`
/// if the face has no baked light or its data is out of range.
///
/// Bump-mapped faces store four sample sets per style (flat plus three bump
/// basis directions); the flat set comes first, so reading from
/// `light_offset` yields it in both cases.
fn face_luxels(face: &vbsp::Face, lighting: &[u8]) -> Option<(usize, usize, Vec<Luxel>)> {
    if face.styles[0] == NO_LIGHT_STYLE {
        return None;
    }
    let offset = usize::try_from(face.light_offset).ok()?;
    let width = usize::try_from(face.light_map_texture_size[0]).ok()? + 1;
    let height = usize::try_from(face.light_map_texture_size[1]).ok()? + 1;
    if width > MAX_LIGHTMAP_SIZE || height > MAX_LIGHTMAP_SIZE {
        return None;
    }
    let data = lighting.get(offset..offset.checked_add(width * height * 4)?)?;
    let luxels = data
        .as_chunks::<4>()
        .0
        .iter()
        .map(|&luxel| decode_luxel(luxel))
        .collect();
    Some((width, height, luxels))
}

/// Returns the bytes of lump `index`, or `None` if the header is invalid or the
/// lump lies outside the file. Reads only the lump directory.
fn lump(bytes: &[u8], index: usize) -> Option<&[u8]> {
    if bytes.get(0..4)? != b"VBSP" {
        return None;
    }
    let entry = LUMP_DIRECTORY_OFFSET + index * LUMP_ENTRY_SIZE;
    let read_i32 = |at: usize| -> Option<i32> {
        Some(i32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
    };
    let start = usize::try_from(read_i32(entry)?).ok()?;
    let length = usize::try_from(read_i32(entry + 4)?).ok()?;
    bytes.get(start..start.checked_add(length)?)
}

/// Parses a KeyValues vector such as `"-1024 512 64"`.
fn parse_vector(text: &str) -> Option<[f32; 3]> {
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
        for surface in &geometry.surfaces {
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

        // The embedded pakfile should be a mountable zip (starts with "PK").
        let pak = pakfile_bytes(&bytes).expect("map has a pakfile");
        assert_eq!(&pak[0..2], b"PK", "pakfile is a zip archive");
        eprintln!("pakfile: {} bytes", pak.len());
    }
}
