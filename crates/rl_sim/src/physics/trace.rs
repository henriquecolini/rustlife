//! Player traces against props.
//!
//! Movement sweeps the player's box against props exactly as against
//! brushes: each convex piece of a prop is a set of clip planes (built when
//! the prop moves, see `PhysicsWorld::refresh_planes`), and the box is
//! clipped against the pieces near its path. Rapier's broad phase finds the
//! candidate props.

use glam::Vec3;
use rapier3d::prelude::{Aabb, InteractionGroups, InteractionTestMode, QueryFilter};

use super::{BodyId, BodyKind, GROUP_PLAYER, GROUP_PROP, PhysicsWorld};
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
        let offset = hull.center();
        let sweep = Sweep::new(start + offset, end + offset, hull.extents(), mask);
        let mut best = Hit::default();
        let mut best_body = None;
        if mask & SOLID != 0 {
            let (mins, maxs) = sweep.bounds(CULL_MARGIN);
            let groups = InteractionGroups::new(GROUP_PLAYER, GROUP_PROP, InteractionTestMode::And);
            let filter = QueryFilter::new().groups(groups);
            for (_, collider) in self
                .rapier
                .intersect_aabb_conservative(Aabb::new(mins, maxs), filter)
            {
                let id = BodyId(collider.user_data as u32);
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
