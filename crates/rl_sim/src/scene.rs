//! The map and its props together, as movement sees them.
//!
//! [`Scene`] answers collision queries against both the map's
//! [`CollisionWorld`] and the props of a [`PhysicsWorld`], returning
//! whichever is hit first, so player movement collides with props exactly
//! as it does with brushes. The mover's fellow players, given as
//! [`PlayerBox`]es, block it too. Ladders and water come from the map alone.

use glam::Vec3;

use crate::collision::clip::DIST_EPSILON;

use crate::collision::clip::{ClipPlane, Hit, Sweep, clip_convex};
use crate::collision::contents::{MASK_PLAYER_SOLID, SOLID};
use crate::collision::{CollisionQuery, CollisionWorld, Hull, Ladder, Trace};
use crate::movement::MovementWorld;
use crate::physics::{BodyId, PhysicsWorld};

/// Another player, as the player moving sees them: a solid box.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlayerBox {
    /// Minimum corner, world space.
    pub mins: Vec3,
    /// Maximum corner, world space.
    pub maxs: Vec3,
    /// Which way the mover leaves this box if their centres coincide
    /// exactly. The two players are given opposite directions, so they come
    /// apart rather than staying one inside the other.
    pub tie_break: Vec3,
}

impl PlayerBox {
    /// The box's six faces, as clip planes.
    fn planes(&self) -> [ClipPlane; 6] {
        let plane = |normal: Vec3, dist| ClipPlane {
            normal,
            dist,
            bevel: false,
            material: None,
        };
        [
            plane(Vec3::X, self.maxs.x),
            plane(Vec3::NEG_X, -self.mins.x),
            plane(Vec3::Y, self.maxs.y),
            plane(Vec3::NEG_Y, -self.mins.y),
            plane(Vec3::Z, self.maxs.z),
            plane(Vec3::NEG_Z, -self.mins.z),
        ]
    }

    /// Sweeps `hull` from `start` to `end` against the box. Boxes need no
    /// bevels: grown by an axis-aligned box they stay boxes.
    fn trace(&self, start: Vec3, end: Vec3, hull: &Hull, mask: u32) -> Trace {
        let offset = hull.center();
        let sweep = Sweep::new(start + offset, end + offset, hull.extents(), mask);
        let mut hit = Hit::default();
        clip_convex(&sweep, self.planes(), SOLID, &mut hit);
        Trace::from_hit(start, end, hit)
    }

    /// The moves taking `hull`, standing at `origin`, out of the box, one
    /// per face, if it is inside it.
    fn penetrations(&self, origin: Vec3, hull: &Hull) -> Vec<Vec3> {
        let center = origin + hull.center();
        let box_center = (self.mins + self.maxs) * 0.5;
        let reach = (self.maxs - self.mins) * 0.5 + hull.extents();
        let delta = center - box_center;
        let depth = reach - delta.abs();
        if depth.min_element() <= 0.0 {
            return Vec::new();
        }
        let mut exits = Vec::with_capacity(6);
        for axis in 0..3 {
            // Where the centre is off the box's own, it leaves that side
            // first; exactly central, the tie-break chooses.
            let near = if delta[axis] == 0.0 {
                self.tie_break[axis]
            } else {
                delta[axis].signum()
            };
            // Out of the near face, or all the way across to the far one.
            for (side, distance) in [
                (near, depth[axis]),
                (-near, 2.0 * reach[axis] - depth[axis]),
            ] {
                let mut out = Vec3::ZERO;
                out[axis] = side * (distance + 2.0 * DIST_EPSILON);
                exits.push(out);
            }
        }
        exits
    }
}

/// The map plus its props and the other players, for one player's queries.
#[derive(Debug, Clone, Copy)]
pub struct Scene<'a> {
    /// The map's static collision.
    pub world: &'a CollisionWorld,
    /// The props.
    pub physics: &'a PhysicsWorld,
    /// A prop the querying player passes through: the one they hold.
    pub ignore: Option<BodyId>,
    /// The players the querying player collides with.
    pub others: &'a [PlayerBox],
}

/// How far up a player is pushed out of a prop at most; farther means they
/// are under it, and should leave sideways.
const MAX_LIFT: f32 = 24.0;

impl Scene<'_> {
    /// Whether `hull` at `origin` is clear of everything.
    fn is_clear(&self, origin: Vec3, hull: &Hull) -> bool {
        !self
            .trace_hull(origin, origin, hull, MASK_PLAYER_SOLID)
            .start_solid
    }

    /// `ahead`, the trace against the map and props, cut short by the
    /// other players in its way.
    fn trace_others(&self, start: Vec3, ahead: Trace, hull: &Hull, mask: u32) -> Trace {
        if mask & SOLID == 0 || ahead.all_solid {
            return ahead;
        }
        let mut best = ahead;
        for other in self.others {
            let mut trace = other.trace(start, ahead.end, hull, mask);
            if !(trace.hit() || trace.start_solid) {
                continue;
            }
            trace.fraction *= ahead.fraction;
            if !trace.hit() {
                trace.end = ahead.end;
            }
            best = best.nearest(trace);
        }
        best
    }
}

impl CollisionQuery for Scene<'_> {
    fn trace_hull(&self, start: Vec3, end: Vec3, hull: &Hull, mask: u32) -> Trace {
        let map = self.world.trace_hull(start, end, hull, mask);
        if map.all_solid {
            return map;
        }
        // Nothing beyond the map's hit matters: trace props only that far.
        let props = self
            .physics
            .trace_hull(start, map.end, hull, mask, self.ignore);
        let ahead = if props.hit() || props.start_solid {
            // Rescale the props' fraction of the shortened move to the
            // whole move.
            let mut props = props;
            props.fraction *= map.fraction;
            props.end = if props.hit() { props.end } else { map.end };
            map.nearest(props)
        } else {
            map
        };
        self.trace_others(start, ahead, hull, mask)
    }

    fn point_contents(&self, point: Vec3) -> u32 {
        self.world.point_contents(point)
    }
}

impl MovementWorld for Scene<'_> {
    fn ladders(&self) -> &[Ladder] {
        self.world.ladders()
    }

    fn ladder_dismounts(&self) -> &[Vec3] {
        self.world.ladder_dismounts()
    }

    fn penetration(&self, origin: Vec3, hull: &Hull, max_distance: f32) -> Option<Vec3> {
        let mut exits = self.physics.penetrations(origin, hull, self.ignore);
        for other in self.others {
            exits.extend(other.penetrations(origin, hull));
        }
        exits.sort_by(|a, b| a.length_squared().total_cmp(&b.length_squared()));
        // The shortest way out that ends in the clear and that the map
        // leaves open; failing that, the one that gets farthest.
        let mut best: Option<Vec3> = None;
        for exit in exits {
            let step = exit.clamp_length_max(max_distance);
            let map = self
                .world
                .trace_hull(origin, origin + step, hull, MASK_PLAYER_SOLID);
            if map.all_solid {
                continue;
            }
            let moved = map.end - origin;
            // Out of a prop's far side is no way out for something stood
            // on the floor: only lift players a little.
            let lifts_too_far = exit.z > MAX_LIFT;
            if map.fraction >= 1.0 && !lifts_too_far && self.is_clear(origin + exit, hull) {
                return Some(moved);
            }
            if best.is_none_or(|b| moved.length_squared() > b.length_squared()) {
                best = Some(moved);
            }
        }
        best.filter(|moved| *moved != Vec3::ZERO)
    }
}
