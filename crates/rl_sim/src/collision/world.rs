//! The collision world of a loaded map and its traces.

use glam::{Mat3, Vec3};
use source_formats::collision::{BrushEntityCollision, MapCollision};

use super::clip::{ClipPlane, Hit, Sweep, clip_convex};
use super::hull::plane_corners;
use super::triangle::prism_planes;
use super::{CollisionQuery, Hull, Ladder, Trace, contents};

/// Deepest tree a trace descends before giving up. Real BSP trees are a few
/// dozen levels deep, so this only stops corrupt (cyclic) trees.
const MAX_TREE_DEPTH: usize = 512;

/// Margin added around a sweep's bounds when culling displacements and
/// entities, so nothing within the clip epsilon is skipped.
const CULL_MARGIN: f32 = 1.0;

/// Brush entity classes whose geometry blocks movement. Anything else with a
/// sub-model (triggers, `func_illusionary`, area portals, physics-only clips,
/// visual effects) is passed through.
const SOLID_BRUSH_CLASSES: [&str; 22] = [
    "func_brush",
    "func_breakable",
    "func_breakable_surf",
    "func_button",
    "func_conveyor",
    "func_door",
    "func_door_rotating",
    "func_guntarget",
    "func_lod",
    "func_lookdoor",
    "func_monitor",
    "func_movelinear",
    "func_physbox",
    "func_physbox_multiplayer",
    "func_plat",
    "func_platrot",
    "func_reflective_glass",
    "func_rot_button",
    "func_rotating",
    "func_tracktrain",
    "func_train",
    "func_wall",
];

/// `func_brush` `Solidity` values.
const SOLIDITY_NEVER: i32 = 1;
const SOLIDITY_ALWAYS: i32 = 2;

/// A solid brush entity's sub-model, placed in the world.
#[derive(Debug, Clone, Copy)]
struct PlacedModel {
    /// Root of the sub-model's tree.
    root: i32,
    origin: Vec3,
    /// Local-to-world rotation; `None` when unrotated (the common case,
    /// traced without any transform).
    rotation: Option<Mat3>,
    /// World-space bounds, for culling.
    mins: Vec3,
    maxs: Vec3,
}

/// Everything a map's players collide with, in Source space: the world's
/// brushes and displacements and its solid brush entities (at their initial
/// placement), plus the map's ladders.
///
/// Built once per map from the BSP's [`MapCollision`]. Queries take `&self`
/// and allocate nothing, so one world can serve every player's movement.
#[derive(Debug, Clone)]
pub struct CollisionWorld {
    map: MapCollision,
    entities: Vec<PlacedModel>,
    ladders: Vec<Ladder>,
    dismounts: Vec<Vec3>,
}

impl CollisionWorld {
    /// Builds the world from a map's collision model. Brush entities of
    /// non-solid classes, `func_brush`es that are never solid or start
    /// disabled, and disabled ladders are left out.
    #[must_use]
    pub fn new(map: MapCollision) -> Self {
        let entities = map
            .brush_entities
            .iter()
            .filter(|entity| is_solid(entity))
            .filter_map(|entity| place(&map, entity))
            .collect();
        let ladders = map
            .ladders
            .iter()
            .filter(|ladder| !ladder.start_disabled)
            .map(|ladder| Ladder {
                bottom: Vec3::from(ladder.bottom),
                top: Vec3::from(ladder.top),
            })
            .collect();
        let dismounts = map
            .ladder_dismounts
            .iter()
            .copied()
            .map(Vec3::from)
            .collect();
        Self {
            map,
            entities,
            ladders,
            dismounts,
        }
    }

    /// Sweeps `hull`, placed at `start`, to `end`, stopping at the first
    /// contents in `mask`.
    #[must_use]
    pub fn trace_hull(&self, start: Vec3, end: Vec3, hull: &Hull, mask: u32) -> Trace {
        let offset = hull.center();
        let sweep = Sweep::new(start + offset, end + offset, hull.extents(), mask);
        let hit = self.sweep(&sweep);
        Trace::from_hit(start, end, hit)
    }

    /// Traces a ray (a zero-size hull) from `start` to `end`.
    #[must_use]
    pub fn trace_ray(&self, start: Vec3, end: Vec3, mask: u32) -> Trace {
        self.trace_hull(start, end, &Hull::POINT, mask)
    }

    /// The contents at `point`: those of the world leaf containing it and of
    /// every world brush containing it.
    #[must_use]
    pub fn point_contents(&self, point: Vec3) -> u32 {
        let Some(root) = self.map.models.first().map(|model| model.root) else {
            return contents::EMPTY;
        };
        let Some(leaf_index) = self.leaf_at(root, point) else {
            return contents::EMPTY;
        };
        let Some(leaf) = self.map.leaves.get(leaf_index) else {
            return contents::EMPTY;
        };
        let mut found = leaf.contents;
        for brush in self.leaf_brushes(leaf_index) {
            let inside = self
                .brush_planes(brush)
                .all(|plane| plane.normal.dot(point) <= plane.dist);
            if inside {
                found |= self
                    .map
                    .brushes
                    .get(brush)
                    .map_or(0, |brush| brush.contents);
            }
        }
        found
    }

    /// The world-space bounds of BSP model `model` (a brush entity's,
    /// never 0) placed at `origin` with `angles` (`[pitch, yaw, roll]`,
    /// degrees); `None` for a missing model.
    #[must_use]
    pub fn model_bounds(
        &self,
        model: usize,
        origin: Vec3,
        angles: [f32; 3],
    ) -> Option<(Vec3, Vec3)> {
        if model == 0 {
            return None;
        }
        let model = self.map.models.get(model)?;
        let rotation = angles
            .iter()
            .any(|&angle| angle != 0.0)
            .then(|| rotation_from_angles(angles));
        Some(placed_bounds(
            Vec3::from(model.mins),
            Vec3::from(model.maxs),
            origin,
            rotation,
        ))
    }

    /// The map's enabled ladders.
    #[must_use]
    pub fn ladders(&self) -> &[Ladder] {
        &self.ladders
    }

    /// Points where a climber may step off a ladder.
    #[must_use]
    pub fn ladder_dismounts(&self) -> &[Vec3] {
        &self.dismounts
    }

    /// The corners of every brush whose contents are in `mask` — the
    /// world's and those of the solid brush entities, placed — in world
    /// space, one list per brush (with repeats where more than three sides
    /// meet). The physics world builds its static colliders from these.
    #[must_use]
    pub fn convex_solids(&self, mask: u32) -> Vec<Vec<Vec3>> {
        let mut solids = Vec::new();
        let mut collect = |root: i32, place: &dyn Fn(Vec3) -> Vec3| {
            for brush in self.model_brushes(root) {
                let Some(contents) = self.map.brushes.get(brush).map(|brush| brush.contents) else {
                    continue;
                };
                if contents & mask == 0 {
                    continue;
                }
                let planes: Vec<(Vec3, f32)> = self
                    .brush_planes(brush)
                    .map(|plane| (plane.normal, plane.dist))
                    .collect();
                let corners: Vec<Vec3> = plane_corners(&planes).into_iter().map(place).collect();
                if corners.len() >= 4 {
                    solids.push(corners);
                }
            }
        };
        if let Some(world) = self.map.models.first() {
            collect(world.root, &|point| point);
        }
        for entity in &self.entities {
            let rotation = entity.rotation.unwrap_or(Mat3::IDENTITY);
            collect(entity.root, &|point| entity.origin + rotation * point);
        }
        solids
    }

    /// The map's displacement surfaces whose contents are in `mask`, as
    /// world-space triangles, one list per displacement.
    pub fn displacement_triangles(&self, mask: u32) -> impl Iterator<Item = &[[[f32; 3]; 3]]> {
        self.map
            .displacements
            .iter()
            .filter(move |displacement| displacement.contents & mask != 0)
            .map(|displacement| displacement.triangles.as_slice())
    }

    /// Every brush listed by a leaf of the tree rooted at `root`, once each.
    fn model_brushes(&self, root: i32) -> Vec<usize> {
        let mut seen = vec![false; self.map.brushes.len()];
        let mut brushes = Vec::new();
        let mut stack = vec![root];
        // Each node is pushed at most once by a well-formed tree; the budget
        // stops corrupt (cyclic) ones.
        let mut budget = self.map.nodes.len() + self.map.leaves.len() + 1;
        while let Some(child) = stack.pop() {
            if budget == 0 {
                break;
            }
            budget -= 1;
            if child < 0 {
                for brush in self.leaf_brushes((!child) as usize) {
                    if let Some(seen) = seen.get_mut(brush)
                        && !*seen
                    {
                        *seen = true;
                        brushes.push(brush);
                    }
                }
            } else if let Some(node) = self.map.nodes.get(child as usize) {
                stack.extend(node.children);
            }
        }
        brushes.sort_unstable();
        brushes
    }

    /// Sweeps against the world and every solid entity.
    fn sweep(&self, sweep: &Sweep) -> Hit {
        let mut hit = Hit::default();
        if let Some(world) = self.map.models.first() {
            self.trace_tree(
                world.root,
                sweep,
                0.0,
                1.0,
                sweep.start,
                sweep.end,
                &mut hit,
                0,
            );
        }
        self.trace_displacements(sweep, &mut hit);
        let (mins, maxs) = sweep.bounds(CULL_MARGIN);
        for entity in &self.entities {
            if overlaps(mins, maxs, entity.mins, entity.maxs) {
                hit.merge(self.trace_entity(entity, sweep));
            }
        }
        hit
    }

    /// Sweeps against one brush entity, in its own frame.
    fn trace_entity(&self, entity: &PlacedModel, sweep: &Sweep) -> Hit {
        let mut hit = Hit::default();
        let local = match entity.rotation {
            None => Sweep {
                start: sweep.start - entity.origin,
                end: sweep.end - entity.origin,
                ..*sweep
            },
            Some(rotation) => {
                let inverse = rotation.transpose();
                // The box turns with the frame; trace its bounding box there,
                // a slightly conservative fit for rotated entities.
                let extents = abs_matrix(inverse) * sweep.extents;
                Sweep {
                    start: inverse * (sweep.start - entity.origin),
                    end: inverse * (sweep.end - entity.origin),
                    extents,
                    ..*sweep
                }
            }
        };
        self.trace_tree(
            entity.root,
            &local,
            0.0,
            1.0,
            local.start,
            local.end,
            &mut hit,
            0,
        );
        if let Some(rotation) = entity.rotation {
            hit.normal = rotation * hit.normal;
        }
        hit
    }

    /// Descends the tree from `child` along the part of the sweep between
    /// fractions `p1f` (at `p1`) and `p2f` (at `p2`), clipping against the
    /// brushes of each leaf it reaches. As in Quake's `CM_TraceThroughTree`,
    /// a node's plane is thickened by the box's support distance: a sweep
    /// wholly on one side descends that side only; one that straddles it is
    /// split, near side first, so the search can stop as soon as a hit is
    /// nearer than the remaining part of the sweep.
    #[allow(clippy::too_many_arguments)]
    fn trace_tree(
        &self,
        child: i32,
        sweep: &Sweep,
        p1f: f32,
        p2f: f32,
        p1: Vec3,
        p2: Vec3,
        hit: &mut Hit,
        depth: usize,
    ) {
        if hit.fraction <= p1f || depth > MAX_TREE_DEPTH {
            return;
        }
        if child < 0 {
            self.trace_leaf((!child) as usize, sweep, hit);
            return;
        }
        let Some(node) = self.map.nodes.get(child as usize) else {
            return;
        };
        let normal = Vec3::from(node.plane.normal);
        let t1 = normal.dot(p1) - node.plane.dist;
        let t2 = normal.dot(p2) - node.plane.dist;
        let offset = sweep.support(normal);
        let [front, back] = node.children;
        if t1 >= offset && t2 >= offset {
            self.trace_tree(front, sweep, p1f, p2f, p1, p2, hit, depth + 1);
            return;
        }
        if t1 < -offset && t2 < -offset {
            self.trace_tree(back, sweep, p1f, p2f, p1, p2, hit, depth + 1);
            return;
        }

        // Straddling: `near` is the side the segment starts on. `split1` ends
        // the near part a little past the plane, `split2` starts the far part
        // a little before it, so nothing on the plane is missed.
        let epsilon = super::clip::DIST_EPSILON;
        let (near, far, split1, split2) = if t1 < t2 {
            let inverse = 1.0 / (t1 - t2);
            (
                back,
                front,
                (t1 - offset + epsilon) * inverse,
                (t1 + offset + epsilon) * inverse,
            )
        } else if t1 > t2 {
            let inverse = 1.0 / (t1 - t2);
            (
                front,
                back,
                (t1 + offset + epsilon) * inverse,
                (t1 - offset - epsilon) * inverse,
            )
        } else {
            (front, back, 1.0, 0.0)
        };
        let split1 = split1.clamp(0.0, 1.0);
        let middle_fraction = p1f + (p2f - p1f) * split1;
        let middle = p1 + (p2 - p1) * split1;
        self.trace_tree(
            near,
            sweep,
            p1f,
            middle_fraction,
            p1,
            middle,
            hit,
            depth + 1,
        );

        let split2 = split2.clamp(0.0, 1.0);
        let middle_fraction = p1f + (p2f - p1f) * split2;
        let middle = p1 + (p2 - p1) * split2;
        self.trace_tree(far, sweep, middle_fraction, p2f, middle, p2, hit, depth + 1);
    }

    /// Clips the whole sweep against every brush of leaf `leaf` whose
    /// contents it is blocked by.
    fn trace_leaf(&self, leaf: usize, sweep: &Sweep, hit: &mut Hit) {
        for brush_index in self.leaf_brushes(leaf) {
            let Some(brush) = self.map.brushes.get(brush_index) else {
                continue;
            };
            if brush.contents & sweep.mask == 0 {
                continue;
            }
            clip_convex(sweep, self.brush_planes(brush_index), brush.contents, hit);
        }
    }

    /// Clips the sweep against every displacement triangle near it.
    fn trace_displacements(&self, sweep: &Sweep, hit: &mut Hit) {
        let (mins, maxs) = sweep.bounds(CULL_MARGIN);
        for displacement in &self.map.displacements {
            if displacement.contents & sweep.mask == 0
                || !overlaps(
                    mins,
                    maxs,
                    Vec3::from(displacement.mins),
                    Vec3::from(displacement.maxs),
                )
            {
                continue;
            }
            for triangle in &displacement.triangles {
                let triangle = triangle.map(Vec3::from);
                let triangle_mins = triangle[0].min(triangle[1]).min(triangle[2]);
                let triangle_maxs = triangle[0].max(triangle[1]).max(triangle[2]);
                // The prism extends `THICKNESS` behind the surface, which the
                // cull margin covers.
                if !overlaps(mins, maxs, triangle_mins, triangle_maxs) {
                    continue;
                }
                if let Some(planes) = prism_planes(triangle) {
                    clip_convex(
                        sweep,
                        planes.as_slice().iter().copied(),
                        displacement.contents,
                        hit,
                    );
                }
            }
        }
    }

    /// The brush indices listed by leaf `leaf`.
    fn leaf_brushes(&self, leaf: usize) -> impl Iterator<Item = usize> + '_ {
        let range = self.map.leaves.get(leaf).map_or(0..0, |leaf| {
            let first = leaf.first_brush as usize;
            first..first + leaf.brush_count as usize
        });
        self.map
            .leaf_brushes
            .get(range)
            .unwrap_or_default()
            .iter()
            .map(|&brush| brush as usize)
    }

    /// The planes of brush `brush`.
    fn brush_planes(&self, brush: usize) -> impl Iterator<Item = ClipPlane> + '_ {
        let range = self.map.brushes.get(brush).map_or(0..0, |brush| {
            let first = brush.first_side as usize;
            first..first + brush.side_count as usize
        });
        self.map
            .sides
            .get(range)
            .unwrap_or_default()
            .iter()
            .map(|side| ClipPlane {
                normal: Vec3::from(side.plane.normal),
                dist: side.plane.dist,
                bevel: side.bevel,
            })
    }

    /// The leaf of the tree rooted at `root` that contains `point`.
    fn leaf_at(&self, root: i32, point: Vec3) -> Option<usize> {
        let mut child = root;
        for _ in 0..=self.map.nodes.len() {
            if child < 0 {
                return Some((!child) as usize);
            }
            let node = self.map.nodes.get(child as usize)?;
            let distance = Vec3::from(node.plane.normal).dot(point) - node.plane.dist;
            child = node.children[usize::from(distance < 0.0)];
        }
        None
    }
}

impl CollisionQuery for CollisionWorld {
    fn trace_hull(&self, start: Vec3, end: Vec3, hull: &Hull, mask: u32) -> Trace {
        CollisionWorld::trace_hull(self, start, end, hull, mask)
    }

    fn point_contents(&self, point: Vec3) -> u32 {
        CollisionWorld::point_contents(self, point)
    }
}

/// Whether a brush entity blocks movement.
fn is_solid(entity: &BrushEntityCollision) -> bool {
    if !SOLID_BRUSH_CLASSES.contains(&entity.classname.as_str()) {
        return false;
    }
    if entity.classname == "func_brush" {
        return match entity.solidity {
            Some(SOLIDITY_NEVER) => false,
            Some(SOLIDITY_ALWAYS) => true,
            // Toggled by map logic; solid while enabled.
            _ => !entity.start_disabled,
        };
    }
    true
}

/// Places a brush entity's sub-model, or `None` if it does not exist.
fn place(map: &MapCollision, entity: &BrushEntityCollision) -> Option<PlacedModel> {
    let model = map.models.get(entity.model)?;
    let origin = Vec3::from(entity.origin);
    let rotation = entity
        .angles
        .iter()
        .any(|&angle| angle != 0.0)
        .then(|| rotation_from_angles(entity.angles));
    let (mins, maxs) = placed_bounds(
        Vec3::from(model.mins),
        Vec3::from(model.maxs),
        origin,
        rotation,
    );
    Some(PlacedModel {
        root: model.root,
        origin,
        rotation,
        mins,
        maxs,
    })
}

/// World-space bounds of a box `local_mins..local_maxs` moved to `origin`
/// and turned by `rotation`.
fn placed_bounds(
    local_mins: Vec3,
    local_maxs: Vec3,
    origin: Vec3,
    rotation: Option<Mat3>,
) -> (Vec3, Vec3) {
    match rotation {
        None => (origin + local_mins, origin + local_maxs),
        Some(rotation) => {
            let center = (local_mins + local_maxs) * 0.5;
            let half = (local_maxs - local_mins) * 0.5;
            let world_center = origin + rotation * center;
            let world_half = abs_matrix(rotation) * half;
            (world_center - world_half, world_center + world_half)
        }
    }
}

/// Source Euler angles `[pitch, yaw, roll]` (degrees) as a rotation matrix:
/// `Rz(yaw) · Ry(pitch) · Rx(roll)`, the composition of Source's
/// `AngleMatrix`, in which positive pitch tips +X downwards.
#[must_use]
pub fn rotation_from_angles(angles: [f32; 3]) -> Mat3 {
    let [pitch, yaw, roll] = angles.map(f32::to_radians);
    Mat3::from_rotation_z(yaw) * Mat3::from_rotation_y(pitch) * Mat3::from_rotation_x(roll)
}

/// The matrix of absolute values, which maps a box's half-size to the
/// half-size of its rotated bounding box.
fn abs_matrix(matrix: Mat3) -> Mat3 {
    Mat3::from_cols(
        matrix.x_axis.abs(),
        matrix.y_axis.abs(),
        matrix.z_axis.abs(),
    )
}

fn overlaps(a_min: Vec3, a_max: Vec3, b_min: Vec3, b_max: Vec3) -> bool {
    a_min.cmple(b_max).all() && b_min.cmple(a_max).all()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positive_pitch_tips_forward_down() {
        let forward = rotation_from_angles([90.0, 0.0, 0.0]) * Vec3::X;
        assert!((forward - Vec3::NEG_Z).length() < 1e-5, "{forward}");
        let left = rotation_from_angles([0.0, 90.0, 0.0]) * Vec3::X;
        assert!((left - Vec3::Y).length() < 1e-5, "{left}");
    }

    fn entity(
        classname: &str,
        solidity: Option<i32>,
        start_disabled: bool,
    ) -> BrushEntityCollision {
        BrushEntityCollision {
            classname: classname.to_owned(),
            model: 1,
            origin: [0.0; 3],
            angles: [0.0; 3],
            solidity,
            start_disabled,
        }
    }

    #[test]
    fn convex_solids_list_matching_brushes_with_their_corners() {
        let world = super::super::WorldBuilder::new()
            .solid_box(Vec3::ZERO, Vec3::ONE * 10.0)
            .box_of(contents::WATER, Vec3::ONE * 20.0, Vec3::ONE * 30.0)
            .build();
        let solids = world.convex_solids(contents::SOLID);
        assert_eq!(solids.len(), 1, "water is not solid");
        let (mins, maxs) = solids[0].iter().fold(
            (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY)),
            |(lo, hi), &p| (lo.min(p), hi.max(p)),
        );
        assert_eq!((mins, maxs), (Vec3::ZERO, Vec3::ONE * 10.0));
    }

    #[test]
    fn only_solid_classes_collide() {
        assert!(is_solid(&entity("func_door", None, false)));
        assert!(!is_solid(&entity("trigger_hurt", None, false)));
        assert!(!is_solid(&entity("func_illusionary", None, false)));
        assert!(!is_solid(&entity("func_brush", Some(1), false)));
        assert!(!is_solid(&entity("func_brush", Some(0), true)));
        assert!(is_solid(&entity("func_brush", Some(2), true)));
    }
}
