//! What can be seen from where: the BSP's potentially visible sets.
//!
//! The map compiler (`vvis`) groups leaves into *clusters* and stores, for
//! each cluster, the set of clusters that could possibly be seen from
//! anywhere inside it (the PVS), as a run-length compressed bit row: a zero
//! byte is followed by a count of zero bytes it stands for. A renderer
//! finds the cluster of the camera by walking the BSP tree and draws only
//! geometry in clusters of its set.
//!
//! Leaves also belong to *areas*, the regions area portals join. The 3D
//! skybox — the miniature scenery drawn behind the sky — is an area of its
//! own, usually holding the map's `sky_camera`.
//!
//! [`Visibility`] holds the tree, each leaf's cluster and area and the
//! decompressed sets; [`Visibility::face_regions`] says which clusters and
//! areas each face lies in.

use crate::bsp::lump;
use crate::leaves::{self, LeafRecord};

/// Lump indices.
const LUMP_PLANES: usize = 1;
const LUMP_VISIBILITY: usize = 4;
const LUMP_NODES: usize = 5;
const LUMP_LEAF_FACES: usize = 16;
/// Record sizes.
const PLANE_SIZE: usize = 20;
const NODE_SIZE: usize = 32;
/// Most clusters a map may have: far beyond any real map, and a guard
/// against a corrupt count allocating gigabytes for the sets.
const MAX_CLUSTERS: usize = 65_536;

/// A node of the BSP tree: its splitting plane and children (`c >= 0` a
/// node, `!c` a leaf).
#[derive(Debug, Clone, Copy, PartialEq)]
struct Node {
    normal: [f32; 3],
    dist: f32,
    children: [i32; 2],
}

/// A map's visibility: see the [module docs](self).
#[derive(Debug, Clone, PartialEq)]
pub struct Visibility {
    nodes: Vec<Node>,
    /// Each leaf's cluster (negative: none) and area, in tree order.
    leaves: Vec<(i16, u16)>,
    /// Each leaf's faces, by index into the face lump.
    leaf_faces: Vec<Vec<u16>>,
    clusters: usize,
    /// Bytes per decompressed row.
    row: usize,
    /// Every cluster's set, decompressed, row after row.
    sets: Vec<u8>,
}

/// Which clusters and areas a face lies in.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FaceRegions {
    /// Clusters, sorted, without repeats.
    pub clusters: Vec<u16>,
    /// Areas, sorted, without repeats.
    pub areas: Vec<u16>,
}

impl Visibility {
    /// Reads a map's visibility from its BSP bytes; `None` for a map
    /// without a tree, leaves or visibility data (one compiled without
    /// `vvis` sees everything).
    #[must_use]
    pub fn load(bytes: &[u8]) -> Option<Self> {
        let records = leaves::read_all(bytes)?;
        let planes = lump(bytes, LUMP_PLANES)?;
        let plane = |index: usize| -> Option<([f32; 3], f32)> {
            let at = index.checked_mul(PLANE_SIZE)?;
            let field = |offset: usize| -> Option<f32> {
                Some(f32::from_le_bytes(
                    planes.get(at + offset..at + offset + 4)?.try_into().ok()?,
                ))
            };
            Some(([field(0)?, field(4)?, field(8)?], field(12)?))
        };
        let nodes = lump(bytes, LUMP_NODES)?
            .as_chunks::<NODE_SIZE>()
            .0
            .iter()
            .map(|node| {
                let i32_at = |at: usize| {
                    i32::from_le_bytes([node[at], node[at + 1], node[at + 2], node[at + 3]])
                };
                let (normal, dist) = plane(usize::try_from(i32_at(0)).ok()?)?;
                Some(Node {
                    normal,
                    dist,
                    children: [i32_at(4), i32_at(8)],
                })
            })
            .collect::<Option<Vec<Node>>>()?;
        let faces: Vec<u16> = lump(bytes, LUMP_LEAF_FACES)
            .unwrap_or_default()
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes(*pair))
            .collect();
        let leaf_faces = records
            .iter()
            .map(|leaf: &LeafRecord| {
                let first = usize::from(leaf.first_face);
                faces
                    .get(first..first + usize::from(leaf.face_count))
                    .unwrap_or_default()
                    .to_vec()
            })
            .collect();
        let (clusters, sets) = decompress_sets(lump(bytes, LUMP_VISIBILITY).unwrap_or_default())?;
        Some(Self {
            nodes,
            leaves: records
                .iter()
                .map(|leaf| (leaf.cluster, leaf.area))
                .collect(),
            leaf_faces,
            clusters,
            row: clusters.div_ceil(8),
            sets,
        })
    }

    /// How many clusters the map has.
    #[must_use]
    pub fn clusters(&self) -> usize {
        self.clusters
    }

    /// The leaf containing `point` (Source space), in tree order.
    #[must_use]
    pub fn leaf_at(&self, point: [f32; 3]) -> Option<usize> {
        let mut child = 0i32;
        // A well-formed tree is far shallower than its node count; the
        // bound stops a cyclic one.
        for _ in 0..=self.nodes.len() {
            if child < 0 {
                return usize::try_from(!child)
                    .ok()
                    .filter(|&leaf| leaf < self.leaves.len());
            }
            let node = self.nodes.get(usize::try_from(child).ok()?)?;
            let distance: f32 = node
                .normal
                .iter()
                .zip(point)
                .map(|(n, p)| n * p)
                .sum::<f32>()
                - node.dist;
            child = node.children[usize::from(distance < 0.0)];
        }
        None
    }

    /// The cluster containing `point`, if it is inside the map.
    #[must_use]
    pub fn cluster_at(&self, point: [f32; 3]) -> Option<u16> {
        let (cluster, _) = *self.leaves.get(self.leaf_at(point)?)?;
        u16::try_from(cluster).ok()
    }

    /// The area containing `point`.
    #[must_use]
    pub fn area_at(&self, point: [f32; 3]) -> Option<u16> {
        Some(self.leaves.get(self.leaf_at(point)?)?.1)
    }

    /// Whether cluster `to` may be seen from cluster `from`. Clusters the
    /// sets do not cover count as seen.
    #[must_use]
    pub fn sees(&self, from: u16, to: u16) -> bool {
        let (from, to) = (usize::from(from), usize::from(to));
        if from >= self.clusters || to >= self.clusters {
            return true;
        }
        self.sets
            .get(from * self.row + to / 8)
            .is_none_or(|byte| byte & (1 << (to % 8)) != 0)
    }

    /// The clusters and areas each of `face_count` faces lies in (a face
    /// spanning several leaves lies in all of theirs).
    #[must_use]
    pub fn face_regions(&self, face_count: usize) -> Vec<FaceRegions> {
        let mut regions = vec![FaceRegions::default(); face_count];
        for (&(cluster, area), faces) in self.leaves.iter().zip(&self.leaf_faces) {
            for &face in faces {
                let Some(region) = regions.get_mut(usize::from(face)) else {
                    continue;
                };
                if let Ok(cluster) = u16::try_from(cluster) {
                    region.clusters.push(cluster);
                }
                region.areas.push(area);
            }
        }
        for region in &mut regions {
            region.clusters.sort_unstable();
            region.clusters.dedup();
            region.areas.sort_unstable();
            region.areas.dedup();
        }
        regions
    }
}

/// The cluster count and every cluster's decompressed set from the
/// visibility lump: a cluster count, a pair of offsets (PVS, PAS) per
/// cluster, then the compressed rows. An empty lump is a map without
/// visibility: zero clusters, which see everything.
fn decompress_sets(data: &[u8]) -> Option<(usize, Vec<u8>)> {
    if data.len() < 4 {
        return Some((0, Vec::new()));
    }
    let i32_at = |at: usize| -> Option<i32> {
        Some(i32::from_le_bytes(data.get(at..at + 4)?.try_into().ok()?))
    };
    let clusters = usize::try_from(i32_at(0)?)
        .ok()
        .filter(|&count| count <= MAX_CLUSTERS)?;
    let row = clusters.div_ceil(8);
    let mut sets = vec![0u8; clusters * row];
    for cluster in 0..clusters {
        let offset = usize::try_from(i32_at(4 + cluster * 8)?).ok()?;
        let out = &mut sets[cluster * row..(cluster + 1) * row];
        let mut at = offset;
        let mut written = 0;
        while written < row {
            let byte = *data.get(at)?;
            at += 1;
            if byte == 0 {
                let zeros = usize::from(*data.get(at)?);
                at += 1;
                written += zeros;
            } else {
                out[written] = byte;
                written += 1;
            }
        }
    }
    Some((clusters, sets))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sets_decompress_their_zero_runs() {
        // Ten clusters (two bytes a row). Cluster 0 sees 0 and 9; cluster 1
        // sees nothing but itself, via a zero run.
        let mut lump = 10i32.to_le_bytes().to_vec();
        let header = 4 + 10 * 8;
        let rows = [vec![0b0000_0001u8, 0b0000_0010], vec![0b0000_0010u8, 0, 1]];
        let mut offset = header;
        let mut offsets = Vec::new();
        for row in &rows {
            offsets.push(offset);
            offset += row.len();
        }
        for cluster in 0..10 {
            let row_offset = offsets.get(cluster).copied().unwrap_or(offsets[0]);
            lump.extend_from_slice(&(row_offset as i32).to_le_bytes());
            lump.extend_from_slice(&(row_offset as i32).to_le_bytes());
        }
        for row in &rows {
            lump.extend_from_slice(row);
        }
        let (clusters, sets) = decompress_sets(&lump).expect("sets");
        assert_eq!(clusters, 10);
        let visibility = Visibility {
            nodes: Vec::new(),
            leaves: Vec::new(),
            leaf_faces: Vec::new(),
            clusters,
            row: 2,
            sets,
        };
        assert!(visibility.sees(0, 0) && visibility.sees(0, 9));
        assert!(!visibility.sees(0, 1));
        assert!(visibility.sees(1, 1) && !visibility.sees(1, 9));
        assert!(visibility.sees(1, 500), "out of range: seen");
    }

    #[test]
    fn faces_lie_in_the_regions_of_their_leaves() {
        let visibility = Visibility {
            nodes: vec![Node {
                normal: [1.0, 0.0, 0.0],
                dist: 0.0,
                children: [!0, !1],
            }],
            leaves: vec![(3, 1), (-1, 0)],
            leaf_faces: vec![vec![0, 1], vec![1]],
            clusters: 0,
            row: 0,
            sets: Vec::new(),
        };
        assert_eq!(visibility.leaf_at([5.0, 0.0, 0.0]), Some(0));
        assert_eq!(visibility.cluster_at([-5.0, 0.0, 0.0]), None, "solid leaf");
        assert_eq!(visibility.area_at([5.0, 0.0, 0.0]), Some(1));
        let regions = visibility.face_regions(3);
        assert_eq!(
            regions[0],
            FaceRegions {
                clusters: vec![3],
                areas: vec![1]
            }
        );
        assert_eq!(
            regions[1],
            FaceRegions {
                clusters: vec![3],
                areas: vec![0, 1]
            }
        );
        assert_eq!(regions[2], FaceRegions::default());
    }

    #[test]
    fn garbage_has_no_visibility() {
        assert_eq!(Visibility::load(b"not a bsp"), None);
        assert_eq!(decompress_sets(&[1, 0, 0, 0]), None, "offsets missing");
        assert_eq!(decompress_sets(&[]), Some((0, Vec::new())));
    }
}

#[cfg(test)]
mod real_map {
    /// A real map's visibility and how its surfaces are split. Set
    /// `RUSTLIFE_TEST_BSP`.
    #[test]
    #[ignore = "requires a BSP file; set RUSTLIFE_TEST_BSP"]
    fn a_real_maps_surfaces_know_their_clusters() {
        let Ok(path) = std::env::var("RUSTLIFE_TEST_BSP") else {
            return;
        };
        let bytes = std::fs::read(&path).expect("read bsp");
        let geometry = crate::bsp::load_geometry(&bytes).expect("geometry");
        let visibility = geometry.visibility.as_ref().expect("visibility");
        let surfaces = &geometry.surfaces;
        let unplaced = surfaces.iter().filter(|s| s.clusters.is_empty()).count();
        let skybox = surfaces.iter().filter(|s| s.skybox).count();
        eprintln!(
            "{} clusters, {} surfaces ({unplaced} always drawn, {skybox} skybox), sky camera {:?}",
            visibility.clusters(),
            surfaces.len(),
            geometry.sky_camera
        );
        assert!(visibility.clusters() > 0);
        assert!(unplaced * 4 < surfaces.len(), "most surfaces are placed");
        // A 3D skybox is often all props, leaving it no surfaces.
        if let Some(camera) = geometry.sky_camera {
            assert!(camera.area.is_some(), "the sky camera stands in an area");
        }
    }
}
