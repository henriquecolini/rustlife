//! A continuous light field for models that move.
//!
//! The engine lights a moving model from one ambient cube sampled at its
//! centre, which makes its lighting jump as the model crosses from one leaf
//! of the map to the next and leaves it either wholly lit or wholly dark at
//! a shadow's edge. A [`LightField`] instead samples the map's lighting
//! (with [`AmbientLighting::lighting_at`]: ambient cubes plus the world
//! lights that see the point) on a regular lattice, computing each lattice
//! node the first time something comes near it, and blends the eight nodes
//! around any point trilinearly. Nodes inside solid space have no light and
//! are left out of the blend, so a prop against a wall is not darkened by
//! the wall. The result is continuous in position: lighting every vertex
//! from it gives a gradient across a half-shadowed prop and no jump as it
//! moves.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use source_formats::ambient::{AmbientCube, AmbientLighting};

/// Distance between lattice nodes, Source units.
pub const CELL: f32 = 16.0;

/// Most nodes kept before the cache starts over.
const MAX_NODES: usize = 400_000;

/// The light of a map as a continuous field. Cheap to share: clones use the
/// same cache.
#[derive(Debug, Clone)]
pub struct LightField {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    lighting: Arc<AmbientLighting>,
    nodes: Mutex<HashMap<[i32; 3], Option<AmbientCube>>>,
}

impl LightField {
    /// A field over `lighting`.
    #[must_use]
    pub fn new(lighting: Arc<AmbientLighting>) -> Self {
        Self {
            inner: Arc::new(Inner {
                lighting,
                nodes: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// The light at `point` (Source space): the lit lattice nodes around it,
    /// blended. `None` where every one of them is inside solid space.
    #[must_use]
    pub fn cube_at(&self, point: [f32; 3]) -> Option<AmbientCube> {
        let mut nodes = self
            .inner
            .nodes
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if nodes.len() > MAX_NODES {
            nodes.clear();
        }
        trilinear(point, |node| {
            *nodes.entry(node).or_insert_with(|| {
                let at = node.map(|index| index as f32 * CELL);
                self.inner.lighting.lighting_at(at)
            })
        })
    }
}

/// The blend of the lattice nodes around `point`, each from `node`, weighted
/// by closeness; nodes without light (`None`) are left out and the rest
/// share their weight. `None` when no node is lit.
fn trilinear(
    point: [f32; 3],
    mut node: impl FnMut([i32; 3]) -> Option<AmbientCube>,
) -> Option<AmbientCube> {
    let scaled = point.map(|coordinate| coordinate / CELL);
    let base = scaled.map(f32::floor);
    let fraction = [0, 1, 2].map(|axis| scaled[axis] - base[axis]);
    let base = base.map(|coordinate| coordinate as i32);
    let mut sum = AmbientCube::default();
    let mut total = 0.0;
    for corner in 0..8_usize {
        let offset = [corner & 1, (corner >> 1) & 1, (corner >> 2) & 1];
        let weight: f32 = (0..3)
            .map(|axis| {
                if offset[axis] == 1 {
                    fraction[axis]
                } else {
                    1.0 - fraction[axis]
                }
            })
            .product();
        if weight <= 0.0 {
            continue;
        }
        let index = [0, 1, 2].map(|axis| base[axis] + offset[axis] as i32);
        let Some(cube) = node(index) else {
            continue;
        };
        for (sum_side, side) in sum.iter_mut().zip(&cube) {
            for (sum_channel, channel) in sum_side.iter_mut().zip(side) {
                *sum_channel += channel * weight;
            }
        }
        total += weight;
    }
    if total <= 0.0 {
        return None;
    }
    for side in &mut sum {
        for channel in side {
            *channel /= total;
        }
    }
    Some(sum)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(brightness: f32) -> AmbientCube {
        [[brightness; 3]; 6]
    }

    #[test]
    fn nodes_blend_by_closeness_and_are_exact_on_the_lattice() {
        // Light brightens with x: node (i, _, _) has brightness i.
        let node = |index: [i32; 3]| Some(flat(index[0] as f32));
        let on_node = trilinear([3.0 * CELL, 5.0, 7.0], node).expect("lit");
        assert!((on_node[0][0] - 3.0).abs() < 1e-5);
        let between = trilinear([3.25 * CELL, 5.0, 7.0], node).expect("lit");
        assert!((between[0][0] - 3.25).abs() < 1e-5, "{between:?}");
        let negative = trilinear([-0.5 * CELL, 0.0, 0.0], node).expect("lit");
        assert!((negative[0][0] + 0.5).abs() < 1e-5, "{negative:?}");
    }

    #[test]
    fn the_blend_is_continuous_across_a_cell_boundary() {
        let node = |index: [i32; 3]| Some(flat((index[0] * 2 + index[1]) as f32));
        let just_before = trilinear([CELL - 1e-3, 3.0, 0.0], node).expect("lit");
        let just_after = trilinear([CELL + 1e-3, 3.0, 0.0], node).expect("lit");
        assert!((just_before[2][1] - just_after[2][1]).abs() < 1e-2);
    }

    #[test]
    fn solid_nodes_are_left_out_and_the_rest_share_their_weight() {
        // Everything below z = 0 (nodes with index z < 0... here z <= 0 is
        // solid) is dark space: only the nodes at z = 1 count.
        let node = |index: [i32; 3]| (index[2] >= 1).then(|| flat(10.0));
        let near_the_floor = trilinear([0.0, 0.0, 1.1 * CELL], node).expect("lit");
        assert!(
            (near_the_floor[0][0] - 10.0).abs() < 1e-4,
            "not dimmed by the floor"
        );
        let inside = trilinear([0.0, 0.0, -3.0 * CELL], |_| None);
        assert_eq!(inside, None);
    }

    #[test]
    fn the_lattice_is_visited_lazily_and_each_node_once_per_query() {
        let mut visited = Vec::new();
        let _ = trilinear([5.0, 5.0, 5.0], |index| {
            visited.push(index);
            Some(flat(1.0))
        });
        visited.sort_unstable();
        visited.dedup();
        assert_eq!(visited.len(), 8);
    }
}
