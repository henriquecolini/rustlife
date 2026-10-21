//! The BSP leaf lump, read directly.
//!
//! `vbsp` sorts its leaves by visibility cluster, so its leaf indices no longer
//! match the indices the node tree (and the per-leaf ambient lumps) refer to.
//! Everything that walks the tree therefore reads the leaf lump itself, through
//! this module.

use crate::bsp::{lump, lump_version};

/// BSP lump index of the leaves.
pub(crate) const LUMP_LEAVES: usize = 10;
/// Size of a version-1 leaf (no inline ambient cube).
pub(crate) const LEAF_SIZE_V1: usize = 32;
/// Size of a version-0 leaf, which embeds a 24-byte ambient cube at offset 30.
pub(crate) const LEAF_SIZE_V0: usize = 56;
/// `CONTENTS_SOLID`: a leaf (or brush) that nothing can be inside.
pub(crate) const CONTENTS_SOLID: i32 = 1;
/// Offset of a leaf's `first_leaf_brush` and `leaf_brush_count` (`u16` each),
/// the same in leaf versions 0 and 1.
const LEAF_BRUSHES_OFFSET: usize = 24;

/// The fields of a leaf that rustlife uses.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct LeafRecord {
    /// `CONTENTS_*` flags of the leaf.
    pub(crate) contents: i32,
    /// The visibility cluster the leaf belongs to; negative for leaves
    /// outside the map (solid space).
    pub(crate) cluster: i16,
    /// The area the leaf belongs to (areas are what area portals join).
    pub(crate) area: u16,
    /// First entry of the leaf's range in the leaf-face lump.
    pub(crate) first_face: u16,
    /// Number of faces in the leaf.
    pub(crate) face_count: u16,
    /// Leaf bounds, Source space.
    pub(crate) mins: [f32; 3],
    /// Leaf bounds, Source space.
    pub(crate) maxs: [f32; 3],
    /// First entry of the leaf's range in the leaf-brush lump.
    pub(crate) first_brush: u16,
    /// Number of brushes touching the leaf.
    pub(crate) brush_count: u16,
}

impl LeafRecord {
    /// Reads the common prefix of a version 0 or 1 leaf. `leaf` must be at
    /// least [`LEAF_SIZE_V1`] bytes long, which the fixed-size chunking of
    /// [`read_all`] guarantees.
    pub(crate) fn read(leaf: &[u8]) -> Self {
        let u16_at = |at: usize| u16::from_le_bytes([leaf[at], leaf[at + 1]]);
        let i16_at = |at: usize| f32::from(i16::from_le_bytes([leaf[at], leaf[at + 1]]));
        Self {
            contents: i32::from_le_bytes([leaf[0], leaf[1], leaf[2], leaf[3]]),
            cluster: i16::from_le_bytes([leaf[4], leaf[5]]),
            // The low 9 bits; the high 7 are flags.
            area: u16_at(6) & 0x1ff,
            first_face: u16_at(20),
            face_count: u16_at(22),
            mins: [i16_at(8), i16_at(10), i16_at(12)],
            maxs: [i16_at(14), i16_at(16), i16_at(18)],
            first_brush: u16_at(LEAF_BRUSHES_OFFSET),
            brush_count: u16_at(LEAF_BRUSHES_OFFSET + 2),
        }
    }

    /// Whether the leaf lies inside solid space.
    pub(crate) fn is_solid(&self) -> bool {
        self.contents & CONTENTS_SOLID != 0
    }
}

/// Every leaf of the map in leaf-lump (tree) order, or `None` if the lump is
/// missing or has an unknown version.
pub(crate) fn read_all(bytes: &[u8]) -> Option<Vec<LeafRecord>> {
    let data = lump(bytes, LUMP_LEAVES)?;
    let records = match lump_version(bytes, LUMP_LEAVES)? {
        0 => data
            .as_chunks::<LEAF_SIZE_V0>()
            .0
            .iter()
            .map(|leaf| LeafRecord::read(leaf))
            .collect(),
        1 => data
            .as_chunks::<LEAF_SIZE_V1>()
            .0
            .iter()
            .map(|leaf| LeafRecord::read(leaf))
            .collect(),
        _ => return None,
    };
    Some(records)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_leaf_prefix() {
        let mut leaf = [0u8; LEAF_SIZE_V1];
        leaf[0..4].copy_from_slice(&0x21i32.to_le_bytes());
        leaf[4..6].copy_from_slice(&(-1i16).to_le_bytes());
        leaf[6..8].copy_from_slice(&(0xfe00u16 | 5).to_le_bytes());
        leaf[20..22].copy_from_slice(&11u16.to_le_bytes());
        leaf[22..24].copy_from_slice(&2u16.to_le_bytes());
        leaf[8..10].copy_from_slice(&(-64i16).to_le_bytes());
        leaf[18..20].copy_from_slice(&128i16.to_le_bytes());
        leaf[24..26].copy_from_slice(&7u16.to_le_bytes());
        leaf[26..28].copy_from_slice(&3u16.to_le_bytes());
        let record = LeafRecord::read(&leaf);
        assert_eq!(record.contents, 0x21);
        assert!(record.is_solid());
        assert_eq!(record.mins[0], -64.0);
        assert_eq!(record.maxs[2], 128.0);
        assert_eq!((record.first_brush, record.brush_count), (7, 3));
        assert_eq!((record.cluster, record.area), (-1, 5), "flags masked off");
        assert_eq!((record.first_face, record.face_count), (11, 2));
    }

    #[test]
    fn non_bsp_bytes_have_no_leaves() {
        assert_eq!(read_all(b"not a bsp"), None);
    }
}
