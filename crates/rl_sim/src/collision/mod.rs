//! Collision: sweeping boxes through a map.
//!
//! The one question movement asks is [`CollisionQuery::trace_hull`]: *if
//! this box moved in a straight line from here to there, how far would it
//! get, and what would it hit?* The answer, a [`Trace`], carries the fraction
//! of the move completed, the end position, the normal of the surface hit and
//! whether the box started (or stayed) inside something.
//!
//! [`CollisionWorld`] answers it for a loaded map, exactly as the Source
//! engine does for brushes: the box descends the BSP tree along its path
//! (`world` module docs) and is clipped against each brush in the leaves it
//! passes through (see the `clip` module). Displacements are clipped as thin
//! triangle prisms with their own bevels, and solid brush entities as their
//! own sub-trees in their own frame. Everything is in Source space: Z up,
//! units of inches.

use glam::Vec3;
use rl_data::HullConfig;

mod builder;
mod clip;
pub mod contents;
mod triangle;
mod world;

pub use builder::WorldBuilder;
pub use world::{CollisionWorld, rotation_from_angles};

pub use clip::DIST_EPSILON;
use clip::Hit;
use contents::MASK_PLAYER_SOLID;

/// Distances, in units, at which [`find_free_position`] looks for room.
const UNSTICK_DISTANCES: [f32; 8] = [1.0, 2.0, 4.0, 8.0, 12.0, 16.0, 24.0, 32.0];

/// An axis-aligned collision box relative to an origin.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Hull {
    /// Minimum corner.
    pub mins: Vec3,
    /// Maximum corner.
    pub maxs: Vec3,
}

impl Hull {
    /// A zero-size hull: traces with it are rays.
    pub const POINT: Self = Self {
        mins: Vec3::ZERO,
        maxs: Vec3::ZERO,
    };

    /// A hull from its corners.
    #[must_use]
    pub fn new(mins: Vec3, maxs: Vec3) -> Self {
        Self { mins, maxs }
    }

    /// Offset of the box's centre from the origin.
    #[must_use]
    pub fn center(&self) -> Vec3 {
        (self.mins + self.maxs) * 0.5
    }

    /// Half the box's size on each axis.
    #[must_use]
    pub fn extents(&self) -> Vec3 {
        (self.maxs - self.mins) * 0.5
    }
}

impl From<&HullConfig> for Hull {
    fn from(config: &HullConfig) -> Self {
        Self::new(Vec3::from(config.mins), Vec3::from(config.maxs))
    }
}

/// The result of a trace.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Trace {
    /// Fraction of the move completed, `0..=1`; `1` means nothing was hit.
    pub fraction: f32,
    /// Where the hull's origin ended up.
    pub end: Vec3,
    /// Normal of the surface hit, pointing out of it; zero if nothing was hit.
    pub normal: Vec3,
    /// `CONTENTS_*` of what was hit.
    pub contents: u32,
    /// The hull started inside something solid.
    pub start_solid: bool,
    /// The hull never left solid space: it is stuck.
    pub all_solid: bool,
}

impl Trace {
    /// Whether the trace hit something before its end.
    #[must_use]
    pub fn hit(&self) -> bool {
        self.fraction < 1.0
    }

    /// Converts a sweep's [`Hit`] into a trace from `start` to `end`.
    fn from_hit(start: Vec3, end: Vec3, hit: Hit) -> Self {
        let end = if hit.fraction >= 1.0 {
            end
        } else {
            start + (end - start) * hit.fraction
        };
        Self {
            fraction: hit.fraction,
            end,
            normal: hit.normal,
            contents: hit.contents,
            start_solid: hit.start_solid,
            all_solid: hit.all_solid,
        }
    }
}

/// A ladder a player can climb: the segment their feet move along.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Ladder {
    /// Lowest point of the climb (feet), Source space.
    pub bottom: Vec3,
    /// Highest point of the climb (feet), Source space.
    pub top: Vec3,
}

/// Anything movement can collide with.
///
/// Implemented by [`CollisionWorld`] for a map's static geometry; later
/// layers (physics props, other players) wrap it and merge their own hits.
pub trait CollisionQuery {
    /// Sweeps `hull`, placed at `start`, to `end`, stopping at the first
    /// contents in `mask`.
    fn trace_hull(&self, start: Vec3, end: Vec3, hull: &Hull, mask: u32) -> Trace;

    /// The `CONTENTS_*` flags at `point`.
    fn point_contents(&self, point: Vec3) -> u32;
}

/// The nearest position to `origin` where `hull` is not inside anything that
/// blocks players: `origin` itself if it is free, else the first free point
/// up, sideways, diagonally or down at increasing distances (up to 32 units).
/// `None` if there is no room nearby.
///
/// Used to place players at spawn points that mappers left overlapping a
/// wall, and to free a player stuck after a hull change.
#[must_use]
pub fn find_free_position(world: &impl CollisionQuery, origin: Vec3, hull: &Hull) -> Option<Vec3> {
    let is_free = |point: Vec3| {
        !world
            .trace_hull(point, point, hull, MASK_PLAYER_SOLID)
            .start_solid
    };
    if is_free(origin) {
        return Some(origin);
    }
    let directions = [
        Vec3::Z,
        Vec3::X,
        Vec3::NEG_X,
        Vec3::Y,
        Vec3::NEG_Y,
        Vec3::new(1.0, 1.0, 0.0),
        Vec3::new(1.0, -1.0, 0.0),
        Vec3::new(-1.0, 1.0, 0.0),
        Vec3::new(-1.0, -1.0, 0.0),
        Vec3::NEG_Z,
    ];
    UNSTICK_DISTANCES
        .iter()
        .flat_map(|&distance| {
            directions
                .iter()
                .map(move |&direction| origin + direction * distance)
        })
        .find(|&point| is_free(point))
}
