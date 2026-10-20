//! Baked ambient lighting for models: the BSP's per-leaf *ambient cubes*.
//!
//! World faces carry lightmaps, but models (props, players, weapons) are lit
//! at run time. For them, `vrad` bakes the light arriving at sample points
//! throughout the map's empty space as *ambient cubes*: six colours, one per
//! axis direction (`+X, -X, +Y, -Y, +Z, -Z`). A surface with normal `n` is
//! lit by `Σ nᵢ² · cube[side of nᵢ]` (Mitchell, *Shading in Valve's Source
//! Engine*, 2006).
//!
//! Version 20 maps store the samples in their own lumps: an index per BSP leaf
//! (lump 52, or 51 for HDR) pointing into a list of samples (lump 56, or 55),
//! each with a position inside the leaf's bounds. Version 19 maps store one
//! cube inline in each leaf instead. Both become an [`AmbientLighting`],
//! which finds the leaf containing a point by walking the BSP tree and blends
//! that leaf's samples by distance.
//!
//! [`AmbientLighting::grid`] resamples the whole map onto a regular voxel grid,
//! which the renderer uploads as an irradiance volume: the same ambient-cube
//! representation, interpolated in hardware.
//!
//! The cubes hold only indirect light. For a model, the engine adds every
//! world light ([`crate::lights`]) with a clear line of sight to it;
//! [`AmbientLighting::lighting_at`] does the same, tracing segments through
//! the BSP tree's solid leaves, and lets sunlight through only where the trace
//! reaches a sky brush.
//!
//! `vbsp` sorts its leaves by cluster, so its leaf indices no longer match the
//! node tree or the ambient index lump; this module therefore reads the leaf
//! lump itself (through [`crate::leaves`]).

use vbsp::Bsp;

use crate::bsp::{lump, lump_version};
use crate::leaves::{LEAF_SIZE_V0, LEAF_SIZE_V1, LUMP_LEAVES, LeafRecord};
use crate::lightmap::{Luxel, decode_luxel};
use crate::lights::{self, LightKind, WorldLight};

/// Light arriving from each axis direction, in Source space: `+X, -X, +Y, -Y,
/// +Z, -Z`. Linear RGB, `1.0` being nominal full brightness (as for luxels).
pub type AmbientCube = [Luxel; 6];

/// Leaf ambient index lumps (one entry per leaf), LDR then HDR.
const LUMP_AMBIENT_INDEX: [usize; 2] = [52, 51];
/// Leaf ambient sample lumps, LDR then HDR (matching [`LUMP_AMBIENT_INDEX`]).
const LUMP_AMBIENT_LIGHTING: [usize; 2] = [56, 55];
/// Offset of the inline ambient cube in a version-0 leaf.
const LEAF_V0_CUBE_OFFSET: usize = 30;
/// Size of one ambient sample: a cube of six `ColorRGBExp32`, then the
/// sample's position in the leaf as three `u8` fractions, then padding.
const SAMPLE_SIZE: usize = 28;
/// Size of a compressed ambient cube.
const CUBE_SIZE: usize = 24;
/// Scale from a decoded `ColorRGBExp32` ambient colour to the luxel scale.
///
/// The engine decodes ambient cubes with `ColorRGBExp32ToVector`, which
/// multiplies by 255 on top of the `TexLightToLinear` decode used for
/// lightmaps, and `vrad` writes the cubes to match. With the factor, ambient
/// light and lightmaps share one scale, so props sit in the world at the same
/// brightness as the surfaces around them.
const AMBIENT_SCALE: f32 = 255.0;
/// How far a sunlight trace runs towards the sun: beyond the largest
/// possible map (±16384 units on each axis).
const SUN_TRACE_LENGTH: f32 = 65_536.0;
/// Deepest tree a trace descends before giving up: real BSP trees are a few
/// dozen levels deep, so this only stops corrupt (cyclic) trees.
const MAX_TRACE_DEPTH: usize = 512;

/// One baked sample: an ambient cube at a point, Source space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AmbientSample {
    /// Where the sample was taken.
    pub position: [f32; 3],
    /// The light arriving there.
    pub cube: AmbientCube,
}

/// A node of the BSP tree: a splitting plane and its two children. A child
/// `>= 0` is a node index; a negative child `c` is the leaf `!c`.
#[derive(Debug, Clone, Copy, PartialEq)]
struct TreeNode {
    normal: [f32; 3],
    dist: f32,
    children: [i32; 2],
}

/// What lighting needs to know about one BSP leaf.
#[derive(Debug, Clone, Default, PartialEq)]
struct Leaf {
    /// Ambient samples inside the leaf (none for solid leaves).
    samples: Vec<AmbientSample>,
    /// Whether the leaf is inside a solid brush, blocking light.
    solid: bool,
    /// Whether one of the leaf's brushes is a sky brush, so a sunlight trace
    /// ending here has reached the sky.
    sky: bool,
}

impl Leaf {
    /// An open leaf with the given samples.
    #[cfg(test)]
    fn open(samples: Vec<AmbientSample>) -> Self {
        Self {
            samples,
            ..Self::default()
        }
    }
}

/// A map's baked ambient lighting and light sources, queryable at any point.
#[derive(Debug, Clone, PartialEq)]
pub struct AmbientLighting {
    nodes: Vec<TreeNode>,
    /// Every leaf, in leaf-lump order.
    leaves: Vec<Leaf>,
    /// The lights the engine adds to models.
    lights: Vec<WorldLight>,
    /// Bounds of the world, `(min, max)`, Source space.
    bounds: ([f32; 3], [f32; 3]),
}

/// Ambient cubes resampled onto a regular grid over the map, Source space.
#[derive(Debug, Clone, PartialEq)]
pub struct AmbientGrid {
    /// Centre of voxel `(0, 0, 0)`.
    pub origin: [f32; 3],
    /// Distance between neighbouring voxel centres.
    pub spacing: f32,
    /// Voxel count along X, Y and Z.
    pub size: [usize; 3],
    /// `size[0] * size[1] * size[2]` cubes, X fastest, then Y, then Z.
    pub cubes: Vec<AmbientCube>,
}

impl AmbientGrid {
    /// Index into [`AmbientGrid::cubes`] of voxel `(x, y, z)`.
    #[must_use]
    pub fn index(&self, x: usize, y: usize, z: usize) -> usize {
        (z * self.size[1] + y) * self.size[0] + x
    }
}

impl AmbientLighting {
    /// Builds the sampler from its parts. Mostly useful for tests; maps are
    /// read with [`crate::bsp::load_geometry`].
    #[cfg(test)]
    fn from_parts(nodes: Vec<TreeNode>, leaves: Vec<Leaf>, bounds: ([f32; 3], [f32; 3])) -> Self {
        Self {
            nodes,
            leaves,
            lights: Vec::new(),
            bounds,
        }
    }

    /// Bounds of the world, `(min, max)`, Source space.
    #[must_use]
    pub fn bounds(&self) -> ([f32; 3], [f32; 3]) {
        self.bounds
    }

    /// Index of the leaf containing `point`, or `None` if the tree is
    /// malformed.
    fn leaf_at(&self, point: [f32; 3]) -> Option<usize> {
        let mut node = 0usize;
        // A well-formed tree reaches a leaf in fewer steps than it has nodes;
        // the bound stops a cyclic (corrupt) tree from looping forever.
        for _ in 0..=self.nodes.len() {
            let current = self.nodes.get(node)?;
            let distance = dot(current.normal, point) - current.dist;
            let child = current.children[usize::from(distance < 0.0)];
            if child < 0 {
                return usize::try_from(!child).ok();
            }
            node = usize::try_from(child).ok()?;
        }
        None
    }

    /// The ambient light at `point`: the containing leaf's samples, weighted
    /// by inverse squared distance. `None` inside solid space or a leaf
    /// without samples.
    #[must_use]
    pub fn sample(&self, point: [f32; 3]) -> Option<AmbientCube> {
        let leaf = self.leaves.get(self.leaf_at(point)?)?;
        blend(&leaf.samples, point)
    }

    /// The light a model at `point` receives, as the engine computes it: the
    /// ambient [`sample`](Self::sample) plus every world light that reaches
    /// the point unobstructed, folded into the cube by direction. `None`
    /// where [`sample`](Self::sample) is.
    #[must_use]
    pub fn lighting_at(&self, point: [f32; 3]) -> Option<AmbientCube> {
        let mut cube = self.sample(point)?;
        for light in &self.lights {
            let Some((color, towards)) = light.arriving_at(point) else {
                continue;
            };
            let visible = match light.kind {
                LightKind::Sun { .. } => {
                    let end = [0, 1, 2].map(|axis| point[axis] + towards[axis] * SUN_TRACE_LENGTH);
                    self.first_solid(point, end)
                        .is_none_or(|leaf| self.leaves.get(leaf).is_some_and(|leaf| leaf.sky))
                }
                LightKind::Point | LightKind::Spot { .. } => {
                    self.first_solid(point, light.origin).is_none()
                }
            };
            if visible {
                lights::add_to_cube(&mut cube, color, towards);
            }
        }
        Some(cube)
    }

    /// The first solid leaf the segment `from → to` enters, or `None` if the
    /// whole segment lies in open space.
    fn first_solid(&self, from: [f32; 3], to: [f32; 3]) -> Option<usize> {
        self.trace(0, from, to, 0)
    }

    /// Recursive segment trace (as in Quake's `RecursiveHullCheck`): a
    /// segment on one side of a node's plane descends that side; one that
    /// crosses it is split, and the half nearer `from` is searched first.
    fn trace(&self, child: i32, from: [f32; 3], to: [f32; 3], depth: usize) -> Option<usize> {
        if child < 0 {
            let leaf = usize::try_from(!child).ok()?;
            return self
                .leaves
                .get(leaf)
                .is_some_and(|leaf| leaf.solid)
                .then_some(leaf);
        }
        if depth > MAX_TRACE_DEPTH {
            return None;
        }
        let node = self.nodes.get(usize::try_from(child).ok()?)?;
        let start = dot(node.normal, from) - node.dist;
        let end = dot(node.normal, to) - node.dist;
        if start >= 0.0 && end >= 0.0 {
            return self.trace(node.children[0], from, to, depth + 1);
        }
        if start < 0.0 && end < 0.0 {
            return self.trace(node.children[1], from, to, depth + 1);
        }
        let near = usize::from(start < 0.0);
        let fraction = start / (start - end);
        let middle = [0, 1, 2].map(|axis| from[axis] + (to[axis] - from[axis]) * fraction);
        self.trace(node.children[near], from, middle, depth + 1)
            .or_else(|| self.trace(node.children[1 - near], middle, to, depth + 1))
    }

    /// Resamples the lighting onto a grid covering the world bounds with
    /// voxels `spacing` units apart.
    ///
    /// Voxels in solid space take the average of their lit neighbours,
    /// spreading outwards until every voxel has a value, so interpolation
    /// near walls does not pull light towards black. Returns `None` if no
    /// voxel is lit at all, or if `spacing` is not positive.
    #[must_use]
    pub fn grid(&self, spacing: f32) -> Option<AmbientGrid> {
        if spacing.is_nan() || spacing <= 0.0 {
            return None;
        }
        let (min, max) = self.bounds;
        let size = [0, 1, 2].map(|axis| {
            let extent = (max[axis] - min[axis]).max(0.0);
            (extent / spacing).ceil() as usize + 1
        });
        // Centre the grid on the bounds so the margins are even.
        let origin = [0, 1, 2].map(|axis| {
            let covered = (size[axis] - 1) as f32 * spacing;
            f32::midpoint(min[axis], max[axis]) - covered / 2.0
        });

        let mut grid = AmbientGrid {
            origin,
            spacing,
            size,
            cubes: Vec::with_capacity(size[0] * size[1] * size[2]),
        };
        let mut lit = Vec::with_capacity(grid.cubes.capacity());
        for z in 0..size[2] {
            for y in 0..size[1] {
                for x in 0..size[0] {
                    let point = [x, y, z].map(|i| i as f32 * spacing);
                    let point = [0, 1, 2].map(|axis| origin[axis] + point[axis]);
                    let cube = self.sample(point);
                    lit.push(cube.is_some());
                    grid.cubes.push(cube.unwrap_or_default());
                }
            }
        }
        fill_unlit(&mut grid, &mut lit).then_some(grid)
    }
}

/// Spreads lit voxels into unlit ones, one ring per pass, each unlit voxel
/// taking the mean of its lit face neighbours. Returns `false` if nothing was
/// lit to begin with.
fn fill_unlit(grid: &mut AmbientGrid, lit: &mut [bool]) -> bool {
    if !lit.iter().any(|&lit| lit) {
        return false;
    }
    let [sx, sy, sz] = grid.size;
    loop {
        let mut filled = Vec::new();
        for z in 0..sz {
            for y in 0..sy {
                for x in 0..sx {
                    let index = grid.index(x, y, z);
                    if lit[index] {
                        continue;
                    }
                    let neighbours = [
                        (x > 0).then(|| grid.index(x - 1, y, z)),
                        (x + 1 < sx).then(|| grid.index(x + 1, y, z)),
                        (y > 0).then(|| grid.index(x, y - 1, z)),
                        (y + 1 < sy).then(|| grid.index(x, y + 1, z)),
                        (z > 0).then(|| grid.index(x, y, z - 1)),
                        (z + 1 < sz).then(|| grid.index(x, y, z + 1)),
                    ];
                    let lit_neighbours: Vec<usize> = neighbours
                        .into_iter()
                        .flatten()
                        .filter(|&n| lit[n])
                        .collect();
                    if !lit_neighbours.is_empty() {
                        let weight = 1.0 / lit_neighbours.len() as f32;
                        let mut cube = AmbientCube::default();
                        for &n in &lit_neighbours {
                            add_scaled(&mut cube, &grid.cubes[n], weight);
                        }
                        filled.push((index, cube));
                    }
                }
            }
        }
        if filled.is_empty() {
            return true;
        }
        for (index, cube) in filled {
            grid.cubes[index] = cube;
            lit[index] = true;
        }
    }
}

/// Blends `samples` at `point` by inverse squared distance (offset by one so a
/// sample exactly at the point does not divide by zero), as the engine does.
fn blend(samples: &[AmbientSample], point: [f32; 3]) -> Option<AmbientCube> {
    if samples.is_empty() {
        return None;
    }
    let mut cube = AmbientCube::default();
    let mut total = 0.0;
    for sample in samples {
        let offset = [0, 1, 2].map(|axis| sample.position[axis] - point[axis]);
        let weight = 1.0 / (dot(offset, offset) + 1.0);
        add_scaled(&mut cube, &sample.cube, weight);
        total += weight;
    }
    for side in &mut cube {
        for channel in side {
            *channel /= total;
        }
    }
    Some(cube)
}

fn add_scaled(sum: &mut AmbientCube, cube: &AmbientCube, weight: f32) {
    for (sum_side, side) in sum.iter_mut().zip(cube) {
        for (sum_channel, channel) in sum_side.iter_mut().zip(side) {
            *sum_channel += channel * weight;
        }
    }
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// Reads the ambient lighting of a parsed map, or `None` if it has none (no
/// ambient lumps, or an unknown leaf layout).
pub(crate) fn load(bsp: &Bsp, bytes: &[u8]) -> Option<AmbientLighting> {
    let nodes = bsp
        .nodes
        .iter()
        .map(|node| {
            let plane = bsp.planes.get(usize::try_from(node.plane_index).ok()?)?;
            Some(TreeNode {
                normal: [plane.normal.x, plane.normal.y, plane.normal.z],
                dist: plane.dist,
                children: node.children,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    let world = bsp.models.first()?;
    let bounds = (
        [world.mins.x, world.mins.y, world.mins.z],
        [world.maxs.x, world.maxs.y, world.maxs.z],
    );

    let leaf_lump = lump(bytes, LUMP_LEAVES)?;
    let (records, samples) = match lump_version(bytes, LUMP_LEAVES)? {
        0 => {
            let leaves = leaf_lump.as_chunks::<LEAF_SIZE_V0>().0;
            (
                leaves.iter().map(|leaf| LeafRecord::read(leaf)).collect(),
                inline_cube_leaves(leaves),
            )
        }
        1 => {
            let leaves = leaf_lump.as_chunks::<LEAF_SIZE_V1>().0;
            let records: Vec<LeafRecord> =
                leaves.iter().map(|leaf| LeafRecord::read(leaf)).collect();
            let samples = sampled_leaves(bytes, &records)?;
            (records, samples)
        }
        _ => return None,
    };
    if samples.iter().all(Vec::is_empty) {
        return None;
    }
    let leaves = records
        .iter()
        .zip(samples)
        .map(|(record, samples)| Leaf {
            samples,
            solid: record.is_solid(),
            sky: record.is_solid() && has_sky_brush(bsp, record),
        })
        .collect();
    Some(AmbientLighting {
        nodes,
        leaves,
        lights: lights::load(bytes),
        bounds,
    })
}

/// Whether any brush in `leaf` has a sky side (2D or 3D sky).
fn has_sky_brush(bsp: &Bsp, leaf: &LeafRecord) -> bool {
    let sky = vbsp::TextureFlags::SKY | vbsp::TextureFlags::SKY2D;
    let first = usize::from(leaf.first_brush);
    let brushes = bsp
        .leaf_brushes
        .get(first..first + usize::from(leaf.brush_count))
        .unwrap_or_default();
    brushes.iter().any(|leaf_brush| {
        let Some(brush) = bsp.brushes.get(usize::from(leaf_brush.brush)) else {
            return false;
        };
        let first_side = brush.brush_side as usize;
        bsp.brush_sides
            .get(first_side..first_side + brush.num_brush_sides as usize)
            .unwrap_or_default()
            .iter()
            .any(|side| {
                usize::try_from(side.texture_info)
                    .ok()
                    .and_then(|index| bsp.textures_info.get(index))
                    .is_some_and(|info| info.flags.intersects(sky))
            })
    })
}

/// Version-0 leaves: one inline cube per leaf, sampled at the leaf's centre.
fn inline_cube_leaves(leaves: &[[u8; LEAF_SIZE_V0]]) -> Vec<Vec<AmbientSample>> {
    leaves
        .iter()
        .map(|leaf| {
            let record = LeafRecord::read(leaf);
            if record.is_solid() {
                return Vec::new();
            }
            let cube = &leaf[LEAF_V0_CUBE_OFFSET..LEAF_V0_CUBE_OFFSET + CUBE_SIZE];
            vec![AmbientSample {
                position: [0, 1, 2].map(|axis| f32::midpoint(record.mins[axis], record.maxs[axis])),
                cube: decode_cube(cube),
            }]
        })
        .collect()
}

/// Version-1 leaves: samples from the ambient lumps, LDR preferred (matching
/// the LDR lightmaps the world is drawn with), HDR as a fallback.
fn sampled_leaves(bytes: &[u8], records: &[LeafRecord]) -> Option<Vec<Vec<AmbientSample>>> {
    let (index, samples) = LUMP_AMBIENT_INDEX
        .into_iter()
        .zip(LUMP_AMBIENT_LIGHTING)
        .find_map(|(index, samples)| {
            let index = lump(bytes, index).filter(|data| !data.is_empty())?;
            let samples = lump(bytes, samples).filter(|data| !data.is_empty())?;
            Some((index, samples))
        })?;
    let samples = samples.as_chunks::<SAMPLE_SIZE>().0;
    let leaves = records
        .iter()
        .zip(index.as_chunks::<4>().0)
        .map(|(record, entry)| {
            if record.is_solid() {
                return Vec::new();
            }
            let count = usize::from(u16::from_le_bytes([entry[0], entry[1]]));
            let first = usize::from(u16::from_le_bytes([entry[2], entry[3]]));
            samples
                .get(first..first.saturating_add(count))
                .unwrap_or_default()
                .iter()
                .map(|sample| AmbientSample {
                    position: [0, 1, 2].map(|axis| {
                        let fraction = f32::from(sample[CUBE_SIZE + axis]) / 255.0;
                        record.mins[axis] + (record.maxs[axis] - record.mins[axis]) * fraction
                    }),
                    cube: decode_cube(&sample[..CUBE_SIZE]),
                })
                .collect()
        })
        .collect();
    Some(leaves)
}

/// Decodes a compressed ambient cube: six `ColorRGBExp32` values, scaled by
/// [`AMBIENT_SCALE`].
fn decode_cube(bytes: &[u8]) -> AmbientCube {
    let mut cube = AmbientCube::default();
    for (side, color) in cube.iter_mut().zip(bytes.as_chunks::<4>().0) {
        *side = decode_luxel(*color).map(|channel| channel * AMBIENT_SCALE);
    }
    cube
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A uniform cube of brightness `value` on every side.
    fn uniform(value: f32) -> AmbientCube {
        [[value; 3]; 6]
    }

    /// A tree with one plane `x = 0`: leaf 0 in front (`x >= 0`), leaf 1
    /// behind. Leaf 1 is solid (no samples).
    fn split_world(samples: Vec<AmbientSample>) -> AmbientLighting {
        AmbientLighting::from_parts(
            vec![TreeNode {
                normal: [1.0, 0.0, 0.0],
                dist: 0.0,
                children: [!0, !1],
            }],
            vec![
                Leaf::open(samples),
                Leaf {
                    solid: true,
                    ..Leaf::default()
                },
            ],
            ([-100.0, -100.0, -100.0], [100.0, 100.0, 100.0]),
        )
    }

    #[test]
    fn walks_the_tree_to_the_containing_leaf() {
        let world = split_world(Vec::new());
        assert_eq!(world.leaf_at([10.0, 0.0, 0.0]), Some(0));
        assert_eq!(world.leaf_at([-10.0, 0.0, 0.0]), Some(1));
        assert_eq!(world.leaf_at([0.0, 5.0, 5.0]), Some(0), "on-plane is front");
    }

    #[test]
    fn a_cyclic_tree_does_not_hang() {
        let world = AmbientLighting::from_parts(
            vec![TreeNode {
                normal: [1.0, 0.0, 0.0],
                dist: 0.0,
                children: [0, 0],
            }],
            Vec::new(),
            ([0.0; 3], [0.0; 3]),
        );
        assert_eq!(world.leaf_at([1.0, 0.0, 0.0]), None);
    }

    #[test]
    fn traces_stop_at_the_first_solid_leaf() {
        let world = split_world(Vec::new());
        assert_eq!(world.first_solid([10.0, 0.0, 0.0], [20.0, 5.0, 0.0]), None);
        assert_eq!(
            world.first_solid([10.0, 0.0, 0.0], [-10.0, 0.0, 0.0]),
            Some(1)
        );
        assert_eq!(
            world.first_solid([-5.0, 0.0, 0.0], [10.0, 0.0, 0.0]),
            Some(1)
        );
    }

    /// The split world with one sample in the open half and `light`.
    fn lit_world(light: WorldLight, solid_is_sky: bool) -> AmbientLighting {
        let mut world = split_world(vec![AmbientSample {
            position: [50.0, 0.0, 0.0],
            cube: uniform(0.1),
        }]);
        world.leaves[1].sky = solid_is_sky;
        world.lights.push(light);
        world
    }

    fn point_light(origin: [f32; 3]) -> WorldLight {
        WorldLight {
            kind: LightKind::Point,
            origin,
            intensity: [100.0; 3],
            radius: 0.0,
            attenuation: [0.0, 0.0, 1.0],
        }
    }

    #[test]
    fn visible_lights_add_to_the_sides_facing_them() {
        let world = lit_world(point_light([60.0, 0.0, 0.0]), false);
        let cube = world.lighting_at([50.0, 0.0, 0.0]).expect("open leaf");
        assert!(
            (cube[0][0] - (0.1 + 100.0 / 100.0)).abs() < 1e-4,
            "+X faces the light"
        );
        assert!((cube[1][0] - 0.1).abs() < 1e-6, "-X faces away");
    }

    #[test]
    fn walls_block_lights() {
        // The light sits beyond the solid half; the segment crosses it.
        let world = lit_world(point_light([-60.0, 0.0, 0.0]), false);
        let cube = world.lighting_at([50.0, 0.0, 0.0]).expect("open leaf");
        assert_eq!(cube, uniform(0.1));
    }

    #[test]
    fn sunlight_needs_a_view_of_the_sky() {
        // The sun shines along +X, so it is seen looking towards -X, through
        // the solid half.
        let sun = WorldLight {
            kind: LightKind::Sun {
                direction: [1.0, 0.0, 0.0],
            },
            ..point_light([0.0; 3])
        };
        let blocked = lit_world(sun, false)
            .lighting_at([50.0, 0.0, 0.0])
            .expect("open");
        assert_eq!(blocked, uniform(0.1), "a plain wall is in the way");
        let lit = lit_world(sun, true)
            .lighting_at([50.0, 0.0, 0.0])
            .expect("open");
        assert!((lit[1][0] - 100.1).abs() < 1e-3, "-X sees the sky");
    }

    #[test]
    fn samples_blend_by_inverse_squared_distance() {
        let world = split_world(vec![
            AmbientSample {
                position: [10.0, 0.0, 0.0],
                cube: uniform(1.0),
            },
            AmbientSample {
                position: [90.0, 0.0, 0.0],
                cube: uniform(0.0),
            },
        ]);
        let near = world.sample([10.0, 0.0, 0.0]).expect("lit leaf");
        assert!(near[0][0] > 0.99, "dominated by the coincident sample");
        let middle = world.sample([50.0, 0.0, 0.0]).expect("lit leaf");
        assert!(
            (middle[0][0] - 0.5).abs() < 1e-5,
            "equidistant samples average"
        );
        assert_eq!(world.sample([-10.0, 0.0, 0.0]), None, "solid leaf");
    }

    #[test]
    fn grid_fills_solid_voxels_from_their_neighbours() {
        let world = split_world(vec![AmbientSample {
            position: [50.0, 0.0, 0.0],
            cube: uniform(0.75),
        }]);
        let grid = world.grid(50.0).expect("some voxels are lit");
        assert_eq!(grid.size, [5, 5, 5]);
        assert_eq!(grid.cubes.len(), 125);
        // Every voxel, including the solid half (x < 0), ends up at the only
        // light value in the map.
        for cube in &grid.cubes {
            assert!((cube[3][1] - 0.75).abs() < 1e-5);
        }
        assert_eq!(grid.origin, [-100.0, -100.0, -100.0]);
    }

    #[test]
    fn grid_of_an_unlit_world_is_none() {
        let world = split_world(Vec::new());
        assert_eq!(world.grid(50.0), None);
        assert_eq!(split_world(Vec::new()).grid(0.0), None);
    }

    #[test]
    fn decodes_compressed_cubes_in_side_order() {
        let mut bytes = [0u8; CUBE_SIZE];
        // Side 4 (+Z): red 128 at exponent -8 -> 128 / 256 = 0.5 after the
        // engine's extra factor of 255.
        bytes[16] = 128;
        bytes[19] = (-8i8).to_le_bytes()[0];
        let cube = decode_cube(&bytes);
        assert_eq!(cube[4], [0.5, 0.0, 0.0]);
        assert_eq!(cube[0], [0.0; 3]);
    }

    /// Checks a real map's ambient lighting. Set `RUSTLIFE_TEST_BSP`.
    #[test]
    #[ignore = "requires a BSP file; set RUSTLIFE_TEST_BSP"]
    fn reads_real_map_ambient_lighting() {
        let Ok(path) = std::env::var("RUSTLIFE_TEST_BSP") else {
            return;
        };
        let bytes = std::fs::read(&path).expect("read bsp");
        let bsp = Bsp::read(&bytes).expect("parse bsp");
        let ambient = load(&bsp, &bytes).expect("map has ambient lighting");
        let sampled = ambient
            .leaves
            .iter()
            .filter(|l| !l.samples.is_empty())
            .count();
        let sky = ambient.leaves.iter().filter(|l| l.sky).count();
        eprintln!("{} lights, {sky} sky leaves", ambient.lights.len());
        assert!(sky > 0, "a stock map has sky brushes");
        let grid = ambient.grid(64.0).expect("grid");
        let brightest = grid
            .cubes
            .iter()
            .flat_map(|cube| cube.iter().flatten())
            .fold(0.0f32, |a, &b| a.max(b));
        eprintln!(
            "{path}: {} leaves ({sampled} sampled), grid {:?}, brightest {brightest}",
            ambient.leaves.len(),
            grid.size
        );
        assert!(sampled > 0);
        assert!(brightest > 0.0, "a lit map has some ambient light");
    }
}
