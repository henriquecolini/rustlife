//! Baked per-vertex lighting of static props (`sp_<index>.vhv`).
//!
//! When a map is compiled with static-prop lighting, `vrad` traces every
//! vertex of every static prop to the map's lights and stores the result in
//! the pakfile as `sp_<index>.vhv` (`sp_hdr_<index>.vhv` for HDR), named after
//! the prop's index in the `sprp` lump. Unlike the leaf ambient cubes, this
//! includes direct light and the shadows cast onto the prop, so it is what
//! makes a static prop look lit in place.
//!
//! The file is a 40-byte header — version (2), the model's checksum, vertex
//! flags, vertex size (4), total vertex count, mesh count — followed by one
//! 28-byte header per mesh (`lod`, vertex count, byte offset of its colours)
//! and the colours themselves: one BGRA byte quad per vertex of each
//! strip group, in the model's file order (see
//! [`ModelGeometry::strip_vertex_ids`]).

use thiserror::Error;

use crate::mdl::ModelGeometry;

/// The only `.vhv` version `vrad` writes.
const VERSION: i32 = 2;
/// Size of the file header.
const HEADER_SIZE: usize = 40;
/// Size of a per-mesh header.
const MESH_HEADER_SIZE: usize = 28;
/// Display gamma `vrad` encodes colours with (`LinearToVertexLight`).
const GAMMA: f32 = 2.2;
/// Over-brightening factor `vrad` divides colours by before encoding, so that
/// light up to twice nominal brightness fits in a byte.
const OVERBRIGHT: f32 = 2.0;

/// Errors from [`decode`].
#[derive(Debug, Error, PartialEq, Eq)]
pub enum VhvError {
    /// The file is shorter than its headers claim.
    #[error("vertex lighting file is truncated")]
    Truncated,
    /// The header names a version other than 2.
    #[error("unsupported vertex lighting version {0}")]
    Version(i32),
    /// Vertices are not stored as 4-byte colours.
    #[error("unsupported vertex size {0}")]
    VertexSize(u32),
}

/// A static prop's baked lighting, decoded to linear RGB.
#[derive(Debug, Clone, PartialEq)]
pub struct VertexLighting {
    /// Checksum of the model the lighting was baked for.
    pub checksum: u32,
    /// Colours of the highest level of detail, one per strip-group vertex in
    /// file order. `1.0` is nominal full brightness, the scale of luxels.
    pub colors: Vec<[f32; 3]>,
}

impl VertexLighting {
    /// Colour per model vertex (indexed like [`crate::mdl::ModelMesh::vertex_ids`]),
    /// or `None` if the lighting was baked for a different model or does not
    /// match its vertex layout, in which case it must not be used.
    #[must_use]
    pub fn by_vertex(&self, model: &ModelGeometry) -> Option<Vec<[f32; 3]>> {
        self.by_vertex_with(model.checksum, &model.strip_vertex_ids)
    }

    /// [`VertexLighting::by_vertex`] for a model known by its checksum and
    /// [`ModelGeometry::strip_vertex_ids`].
    #[must_use]
    pub fn by_vertex_with(&self, checksum: u32, strip_vertex_ids: &[u32]) -> Option<Vec<[f32; 3]>> {
        if self.checksum != checksum || self.colors.len() != strip_vertex_ids.len() {
            return None;
        }
        let count = strip_vertex_ids
            .iter()
            .max()
            .map_or(0, |&id| id as usize + 1);
        let mut by_vertex = vec![[0.0; 3]; count];
        for (&id, &color) in strip_vertex_ids.iter().zip(&self.colors) {
            by_vertex[id as usize] = color;
        }
        Some(by_vertex)
    }
}

/// Decodes a `.vhv` file, keeping the highest level of detail.
///
/// # Errors
///
/// Returns a [`VhvError`] for a truncated file or an unsupported version or
/// vertex format.
pub fn decode(bytes: &[u8]) -> Result<VertexLighting, VhvError> {
    let header = bytes.get(..HEADER_SIZE).ok_or(VhvError::Truncated)?;
    let word = |at: usize| {
        u32::from_le_bytes([header[at], header[at + 1], header[at + 2], header[at + 3]])
    };
    let version = word(0) as i32;
    if version != VERSION {
        return Err(VhvError::Version(version));
    }
    let vertex_size = word(12);
    if vertex_size != 4 {
        return Err(VhvError::VertexSize(vertex_size));
    }
    let mesh_count = word(20) as usize;

    let mut colors = Vec::new();
    for mesh in 0..mesh_count {
        let at = HEADER_SIZE + mesh * MESH_HEADER_SIZE;
        let mesh_header = bytes.get(at..at + 12).ok_or(VhvError::Truncated)?;
        let field = |index: usize| {
            let at = index * 4;
            u32::from_le_bytes([
                mesh_header[at],
                mesh_header[at + 1],
                mesh_header[at + 2],
                mesh_header[at + 3],
            ]) as usize
        };
        let (lod, count, offset) = (field(0), field(1), field(2));
        if lod != 0 {
            continue;
        }
        let data = offset
            .checked_add(count * 4)
            .and_then(|end| bytes.get(offset..end))
            .ok_or(VhvError::Truncated)?;
        colors.extend(
            data.as_chunks::<4>()
                .0
                .iter()
                .map(|&bgra| decode_color(bgra)),
        );
    }
    Ok(VertexLighting {
        checksum: word(4),
        colors,
    })
}

/// Decodes one BGRA colour: `vrad` stores `(linear / 2)^(1 / 2.2)` per
/// channel, so this undoes the gamma and the over-brightening.
fn decode_color([b, g, r, _]: [u8; 4]) -> [f32; 3] {
    [r, g, b].map(|channel| OVERBRIGHT * (f32::from(channel) / 255.0).powf(GAMMA))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A version-2 file with the given meshes as `(lod, colours)`.
    fn file(checksum: u32, meshes: &[(u32, &[[u8; 4]])]) -> Vec<u8> {
        let mut bytes = Vec::new();
        let total: usize = meshes.iter().map(|(_, colors)| colors.len()).sum();
        for word in [
            2,
            checksum,
            0x10,
            4,
            total as u32,
            meshes.len() as u32,
            0,
            0,
            0,
            0,
        ] {
            bytes.extend(word.to_le_bytes());
        }
        let mut offset = HEADER_SIZE + meshes.len() * MESH_HEADER_SIZE;
        for (lod, colors) in meshes {
            for word in [*lod, colors.len() as u32, offset as u32, 0, 0, 0, 0] {
                bytes.extend(word.to_le_bytes());
            }
            offset += colors.len() * 4;
        }
        for (_, colors) in meshes {
            bytes.extend(colors.iter().flatten());
        }
        bytes
    }

    #[test]
    fn keeps_only_the_highest_detail_colours() {
        let bytes = file(
            7,
            &[
                (0, &[[0, 0, 255, 255], [255, 0, 0, 255]]),
                (1, &[[9, 9, 9, 255]]),
            ],
        );
        let lighting = decode(&bytes).expect("valid file");
        assert_eq!(lighting.checksum, 7);
        assert_eq!(lighting.colors.len(), 2);
        // BGRA: the first colour is pure red at full byte value -> 2.0 linear.
        assert_eq!(lighting.colors[0], [2.0, 0.0, 0.0]);
        assert_eq!(lighting.colors[1], [0.0, 0.0, 2.0]);
    }

    #[test]
    fn mid_grey_undoes_gamma_and_overbright() {
        let [r, _, _] = decode_color([128, 128, 128, 255]);
        let expected = 2.0 * (128.0f32 / 255.0).powf(2.2);
        assert!((r - expected).abs() < 1e-6);
        assert!(r > 0.4 && r < 0.45, "about 0.44: {r}");
    }

    #[test]
    fn rejects_bad_files() {
        assert_eq!(decode(&[0; 10]), Err(VhvError::Truncated));
        let mut bytes = file(0, &[(0, &[[0; 4]])]);
        bytes[0] = 3;
        assert_eq!(decode(&bytes), Err(VhvError::Version(3)));
        let mut bytes = file(0, &[(0, &[[0; 4]])]);
        bytes.truncate(bytes.len() - 2);
        assert_eq!(decode(&bytes), Err(VhvError::Truncated));
    }

    #[test]
    fn maps_colours_onto_model_vertices() {
        let lighting = VertexLighting {
            checksum: 5,
            colors: vec![[1.0; 3], [0.5; 3], [0.25; 3]],
        };
        // Strip-group vertices 0 and 2 both use model vertex 3.
        let by_vertex = lighting
            .by_vertex_with(5, &[3, 0, 3])
            .expect("matching model");
        assert_eq!(by_vertex.len(), 4);
        assert_eq!(by_vertex[0], [0.5; 3]);
        assert_eq!(by_vertex[3], [0.25; 3]);
        assert_eq!(lighting.by_vertex_with(6, &[3, 0, 3]), None, "other model");
        assert_eq!(lighting.by_vertex_with(5, &[0, 1]), None, "other layout");
    }
}
