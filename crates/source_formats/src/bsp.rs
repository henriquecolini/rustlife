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

use std::collections::HashMap;

use thiserror::Error;
use vbsp::Bsp;

/// BSP lump index of the embedded pakfile (a zip archive of map-specific
/// content: patched cubemap materials, custom textures and models).
const LUMP_PAKFILE: usize = 40;
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

    let mut by_material: HashMap<String, usize> = HashMap::new();
    let mut surfaces: Vec<MapSurface> = Vec::new();

    for index in 0..bsp.faces.len() {
        let Some(face) = bsp.face(index) else {
            continue;
        };
        if !face.is_visible() {
            continue;
        }

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
            });
            surfaces.len() - 1
        });
        let surface = &mut surfaces[surface_index];

        // `vertex_positions` yields an already-triangulated vertex stream
        // (triangle list), tessellating displacement faces along the way.
        for position in face.vertex_positions() {
            surface.positions.push([position.x, position.y, position.z]);
            surface.normals.push(normal);
            surface.uvs.push(texture.uv(position));
        }
    }

    let spawns = bsp
        .entities
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
        .collect();

    Ok(MapGeometry { surfaces, spawns })
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
    if bytes.len() < LUMP_DIRECTORY_OFFSET || &bytes[0..4] != b"VBSP" {
        return None;
    }
    let entry = LUMP_DIRECTORY_OFFSET + LUMP_PAKFILE * LUMP_ENTRY_SIZE;
    let file_offset = i32::from_le_bytes(bytes.get(entry..entry + 4)?.try_into().ok()?);
    let file_length = i32::from_le_bytes(bytes.get(entry + 4..entry + 8)?.try_into().ok()?);

    let start = usize::try_from(file_offset).ok()?;
    let length = usize::try_from(file_length).ok()?;
    if length == 0 {
        return None;
    }
    let end = start.checked_add(length)?;
    Some(bytes.get(start..end)?.to_vec())
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
        assert!(!geometry.spawns.is_empty(), "a DM map has spawn points");

        // The embedded pakfile should be a mountable zip (starts with "PK").
        let pak = pakfile_bytes(&bytes).expect("map has a pakfile");
        assert_eq!(&pak[0..2], b"PK", "pakfile is a zip archive");
        eprintln!("pakfile: {} bytes", pak.len());
    }
}
