//! The map and its props together, as movement sees them.
//!
//! [`Scene`] answers collision queries against both the map's
//! [`CollisionWorld`] and the props of a [`PhysicsWorld`], returning
//! whichever is hit first, so player movement collides with props exactly
//! as it does with brushes. Ladders and water come from the map alone.

use glam::Vec3;

use crate::collision::{CollisionQuery, CollisionWorld, Hull, Ladder, Trace};
use crate::movement::MovementWorld;
use crate::physics::{BodyId, PhysicsWorld};

/// The map plus its props, for one player's queries.
#[derive(Debug, Clone, Copy)]
pub struct Scene<'a> {
    /// The map's static collision.
    pub world: &'a CollisionWorld,
    /// The props.
    pub physics: &'a PhysicsWorld,
    /// A prop the querying player passes through: the one they hold.
    pub ignore: Option<BodyId>,
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
        if props.hit() || props.start_solid {
            // Rescale the props' fraction of the shortened move to the
            // whole move.
            let mut props = props;
            props.fraction *= map.fraction;
            props.end = if props.hit() { props.end } else { map.end };
            map.nearest(props)
        } else {
            map
        }
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
}
