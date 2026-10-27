//! Player traces against props.
//!
//! Movement sweeps the player's box against props exactly as against
//! brushes: each convex piece of a prop is a set of clip planes (built when
//! the prop moves, see `PhysicsWorld::refresh_planes`), and the box is
//! clipped against the pieces near its path. `Box3D`'s broad phase finds the
//! candidate props.

use box3d_rust as b3;
use glam::Vec3;

use super::{BodyId, BodyKind, GROUP_DEBRIS, GROUP_PROP, PhysicsWorld, query_filter, to_b3};
use crate::collision::clip::{Hit, Sweep, clip_convex};
use crate::collision::contents::SOLID;
use crate::collision::{Hull, Trace};

/// Margin around a sweep's bounds when gathering candidates, so nothing
/// within the clip epsilon is missed.
const CULL_MARGIN: f32 = 1.0;

impl PhysicsWorld {
    /// Sweeps `hull` from `start` to `end` against every prop players
    /// collide with (all but debris), skipping `ignore` — the prop the
    /// player holds. Props are solid: only a `mask` including
    /// [`SOLID`] sees them.
    #[must_use]
    pub fn trace_hull(
        &self,
        start: Vec3,
        end: Vec3,
        hull: &Hull,
        mask: u32,
        ignore: Option<BodyId>,
    ) -> Trace {
        self.trace_hull_of(start, end, hull, mask, ignore, GROUP_PROP)
    }

    /// As [`PhysicsWorld::trace_hull`], but also against debris: what the
    /// Manipulator's aim sees, since pickups lie as debris but can be
    /// grabbed.
    #[must_use]
    pub fn trace_hull_including_debris(
        &self,
        start: Vec3,
        end: Vec3,
        hull: &Hull,
        mask: u32,
        ignore: Option<BodyId>,
    ) -> Trace {
        self.trace_hull_of(start, end, hull, mask, ignore, GROUP_PROP | GROUP_DEBRIS)
    }

    /// Sweeps `hull` against the props in `groups`.
    fn trace_hull_of(
        &self,
        start: Vec3,
        end: Vec3,
        hull: &Hull,
        mask: u32,
        ignore: Option<BodyId>,
        groups: u64,
    ) -> Trace {
        let offset = hull.center();
        let sweep = Sweep::new(start + offset, end + offset, hull.extents(), mask);
        let mut best = Hit::default();
        let mut best_body = None;
        if mask & SOLID != 0 {
            let (mins, maxs) = sweep.bounds(CULL_MARGIN);
            let mut candidates: Vec<BodyId> = Vec::new();
            let world = self.b3_world();
            b3::world::world_overlap_aabb(
                &world,
                b3::Aabb {
                    lower_bound: to_b3(mins),
                    upper_bound: to_b3(maxs),
                },
                &query_filter(groups),
                |shape| {
                    let handle = b3::shape::shape_get_body(&world, shape);
                    if let Some(id) = self.body_id(handle) {
                        candidates.push(id);
                    }
                    true
                },
            );
            candidates.sort_unstable();
            candidates.dedup();
            for id in candidates {
                if Some(id) == ignore {
                    continue;
                }
                let Some(BodyKind::Prop(prop)) = self.bodies.get(id.index()).map(|body| &body.kind)
                else {
                    continue;
                };
                for piece in &prop.pieces {
                    if !(mins.cmple(piece.maxs).all() && piece.mins.cmple(maxs).all()) {
                        continue;
                    }
                    let mut hit = Hit::default();
                    clip_convex(&sweep, piece.planes.iter().copied(), SOLID, &mut hit);
                    best.start_solid |= hit.start_solid;
                    best.all_solid |= hit.all_solid;
                    if hit.fraction < best.fraction {
                        best.fraction = hit.fraction;
                        best.normal = hit.normal;
                        best.contents = hit.contents;
                        best_body = Some(id);
                    }
                }
            }
        }
        let mut trace = Trace::from_hit(start, end, best);
        trace.body = best_body.filter(|_| trace.hit() || trace.start_solid);
        trace
    }
}
