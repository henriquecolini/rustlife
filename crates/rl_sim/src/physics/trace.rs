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
use crate::collision::clip::{ClipPlane, DIST_EPSILON, Hit, Sweep, clip_convex};
use crate::collision::contents::SOLID;
use crate::collision::{Hull, Trace};

/// Margin around a sweep's bounds when gathering candidates, so nothing
/// within the clip epsilon is missed.
const CULL_MARGIN: f32 = 1.0;

/// Whether a piece whose lowest point is `piece_bottom` is over the head of
/// a box centred at height `center_z` that overlaps it: it bottoms out
/// above the box's centre.
///
/// Such a piece neither blocks nor pushes: a player may carry a prop on
/// their head (set there with the Manipulator, say), and walks out from
/// under it like from under a hat. One that overlaps the box lower down is
/// something the player is inside, and is not overhead.
fn overhead(piece_bottom: f32, center_z: f32) -> bool {
    piece_bottom >= center_z
}

impl PhysicsWorld {
    /// The props in `groups` whose bounds overlap `mins..maxs`, by id.
    fn candidates(&self, mins: Vec3, maxs: Vec3, groups: u64) -> Vec<BodyId> {
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
        candidates
    }

    /// The moves that take `hull`, standing at `origin`, out of the props it
    /// overlaps (all but debris, and `ignore`), shortest first; empty if it
    /// overlaps none.
    ///
    /// A prop piece's clip planes, grown by the box, bound the volume the
    /// box's centre must stay out of. Inside it, leaving along any one of
    /// the planes works, the nearest being the shortest way out. The others
    /// are offered too, because the shortest may be blocked (a prop resting
    /// on a player's head leaves "down", through the floor, the shortest).
    #[must_use]
    pub fn penetrations(&self, origin: Vec3, hull: &Hull, ignore: Option<BodyId>) -> Vec<Vec3> {
        let center = origin + hull.center();
        let sweep = Sweep::new(center, center, hull.extents(), SOLID);
        let (mins, maxs) = sweep.bounds(CULL_MARGIN);
        let mut exits = Vec::new();
        for id in self.candidates(mins, maxs, GROUP_PROP) {
            if Some(id) == ignore {
                continue;
            }
            let Some(BodyKind::Prop(prop)) = self.bodies.get(id.index()).map(|body| &body.kind)
            else {
                continue;
            };
            for piece in &prop.pieces {
                if !(mins.cmple(piece.maxs).all() && piece.mins.cmple(maxs).all())
                    || overhead(piece.mins.z, center.z)
                {
                    continue;
                }
                // How far past each grown plane the centre is; it is inside
                // only if every one is behind.
                let past = |plane: &ClipPlane| {
                    plane.normal.dot(center) - plane.dist - sweep.support(plane.normal)
                };
                if piece.planes.iter().all(|plane| past(plane) <= 0.0) {
                    exits.extend(
                        piece
                            .planes
                            .iter()
                            .map(|plane| plane.normal * (-past(plane) + 2.0 * DIST_EPSILON)),
                    );
                }
            }
        }
        exits.sort_by(|a, b| a.length_squared().total_cmp(&b.length_squared()));
        exits
    }

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
            for id in self.candidates(mins, maxs, groups) {
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
                    if hit.start_solid && overhead(piece.mins.z, sweep.start.z) {
                        continue;
                    }
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
