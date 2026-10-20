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

/// All visible world geometry of a map, grouped by material.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MapGeometry {
    /// One entry per distinct material referenced by a visible face.
    pub surfaces: Vec<MapSurface>,
}

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

    Ok(MapGeometry { surfaces })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_bsp_bytes_error_without_panicking() {
        assert!(load_geometry(b"not a bsp").is_err());
        assert!(load_geometry(&[]).is_err());
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
    }
}
