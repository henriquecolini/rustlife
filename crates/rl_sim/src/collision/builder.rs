//! Building collision worlds by hand, for tests, benchmarks and tools.
//!
//! A [`WorldBuilder`] collects convex brushes, displacement triangles and
//! ladders and produces a [`CollisionWorld`] whose tree is a single leaf
//! holding every brush. That is slower than a compiled BSP tree for large
//! worlds but traces identically, which is what synthetic test rooms need.
//!
//! Brushes get the axis-aligned bevel planes a map compiler would add, so box
//! traces against sloped brushes do not snag on their edges.

use glam::Vec3;
use source_formats::collision::{
    CollisionBrush, CollisionLeaf, CollisionPlane, CollisionSide, DisplacementMesh, LadderSegment,
    MapCollision, SubModel,
};

use super::CollisionWorld;
use super::contents;

/// Tolerance for deciding a point lies inside a brush when finding its
/// corners.
const INSIDE_EPSILON: f32 = 0.01;
/// Two normals closer than this (component-wise) are the same direction.
const SAME_NORMAL_EPSILON: f32 = 1e-4;

/// Assembles a [`CollisionWorld`] from convex pieces.
#[derive(Debug, Clone, Default)]
pub struct WorldBuilder {
    map: MapCollision,
}

impl WorldBuilder {
    /// An empty world.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a solid axis-aligned box.
    #[must_use]
    pub fn solid_box(self, mins: Vec3, maxs: Vec3) -> Self {
        self.box_of(contents::SOLID, mins, maxs)
    }

    /// Adds an axis-aligned box of the given contents (e.g. water).
    #[must_use]
    pub fn box_of(self, contents: u32, mins: Vec3, maxs: Vec3) -> Self {
        let planes = [
            (Vec3::X, maxs.x),
            (Vec3::NEG_X, -mins.x),
            (Vec3::Y, maxs.y),
            (Vec3::NEG_Y, -mins.y),
            (Vec3::Z, maxs.z),
            (Vec3::NEG_Z, -mins.z),
        ];
        self.brush(contents, &planes)
    }

    /// Adds a convex brush bounded by `planes` (`normal · p <= dist` inside;
    /// normals need not be unit length). Axis-aligned bevels are added for
    /// any bounding direction the brush lacks.
    #[must_use]
    pub fn brush(mut self, contents: u32, planes: &[(Vec3, f32)]) -> Self {
        let mut sides: Vec<CollisionSide> = planes
            .iter()
            .filter_map(|&(normal, dist)| {
                let length = normal.length();
                (length > 0.0).then(|| side(normal / length, dist / length, false))
            })
            .collect();
        if let Some((mins, maxs)) = corner_bounds(&sides) {
            for (axis, low, high) in [
                (Vec3::X, mins.x, maxs.x),
                (Vec3::Y, mins.y, maxs.y),
                (Vec3::Z, mins.z, maxs.z),
            ] {
                for (normal, dist) in [(axis, high), (-axis, -low)] {
                    let present = sides.iter().any(|side| {
                        Vec3::from(side.plane.normal).abs_diff_eq(normal, SAME_NORMAL_EPSILON)
                    });
                    if !present {
                        sides.push(side(normal, dist, true));
                    }
                }
            }
        }
        let first_side = self.map.sides.len() as u32;
        self.map.brushes.push(CollisionBrush {
            contents,
            first_side,
            side_count: sides.len() as u32,
        });
        self.map.sides.extend(sides);
        self
    }

    /// Adds a solid ramp rising along +X: its floor spans `mins..maxs`
    /// horizontally at height `mins.z`, its top slopes from `mins.z` at
    /// `mins.x` up to `maxs.z` at `maxs.x`, and it has a vertical back wall.
    #[must_use]
    pub fn ramp(self, mins: Vec3, maxs: Vec3) -> Self {
        let rise = maxs.z - mins.z;
        let run = maxs.x - mins.x;
        // The slope plane passes through (mins.x, ·, mins.z) and
        // (maxs.x, ·, maxs.z); its outward normal is (-rise, 0, run).
        let slope = Vec3::new(-rise, 0.0, run);
        let slope_dist = slope.dot(Vec3::new(mins.x, 0.0, mins.z));
        let planes = [
            (slope, slope_dist),
            (Vec3::X, maxs.x),
            (Vec3::Y, maxs.y),
            (Vec3::NEG_Y, -mins.y),
            (Vec3::NEG_Z, -mins.z),
        ];
        self.brush(contents::SOLID, &planes)
    }

    /// Adds a solid displacement surface made of `triangles`.
    #[must_use]
    pub fn displacement(mut self, triangles: Vec<[Vec3; 3]>) -> Self {
        let mut mins = Vec3::splat(f32::INFINITY);
        let mut maxs = Vec3::splat(f32::NEG_INFINITY);
        for vertex in triangles.iter().flatten() {
            mins = mins.min(*vertex);
            maxs = maxs.max(*vertex);
        }
        self.map.displacements.push(DisplacementMesh {
            contents: contents::SOLID,
            mins: mins.to_array(),
            maxs: maxs.to_array(),
            triangles: triangles
                .into_iter()
                .map(|triangle| triangle.map(|vertex| vertex.to_array()))
                .collect(),
        });
        self
    }

    /// Adds a ladder between two feet positions.
    #[must_use]
    pub fn ladder(mut self, bottom: Vec3, top: Vec3) -> Self {
        self.map.ladders.push(LadderSegment {
            bottom: bottom.to_array(),
            top: top.to_array(),
            start_disabled: false,
        });
        self
    }

    /// Adds a point where a climber may step off a ladder.
    #[must_use]
    pub fn ladder_dismount(mut self, point: Vec3) -> Self {
        self.map.ladder_dismounts.push(point.to_array());
        self
    }

    /// Finishes the world: one leaf holding every brush.
    #[must_use]
    pub fn build(mut self) -> CollisionWorld {
        let count = self.map.brushes.len() as u32;
        self.map.leaf_brushes = (0..count).collect();
        self.map.leaves = vec![CollisionLeaf {
            contents: contents::EMPTY,
            first_brush: 0,
            brush_count: count,
        }];
        self.map.models = vec![SubModel {
            root: !0,
            mins: [f32::MIN; 3],
            maxs: [f32::MAX; 3],
        }];
        CollisionWorld::new(self.map)
    }
}

fn side(normal: Vec3, dist: f32, bevel: bool) -> CollisionSide {
    CollisionSide {
        plane: CollisionPlane {
            normal: normal.to_array(),
            dist,
        },
        bevel,
    }
}

/// Bounds of a brush's corners — every point where three of its planes meet
/// that lies inside all of them — or `None` if it has none (unbounded).
fn corner_bounds(sides: &[CollisionSide]) -> Option<(Vec3, Vec3)> {
    let planes: Vec<(Vec3, f32)> = sides
        .iter()
        .map(|side| (Vec3::from(side.plane.normal), side.plane.dist))
        .collect();
    let mut bounds: Option<(Vec3, Vec3)> = None;
    for (i, a) in planes.iter().enumerate() {
        for (j, b) in planes.iter().enumerate().skip(i + 1) {
            for c in planes.iter().skip(j + 1) {
                let Some(corner) = intersect(*a, *b, *c) else {
                    continue;
                };
                let inside = planes
                    .iter()
                    .all(|(normal, dist)| normal.dot(corner) <= dist + INSIDE_EPSILON);
                if inside {
                    bounds = Some(bounds.map_or((corner, corner), |(lo, hi)| {
                        (lo.min(corner), hi.max(corner))
                    }));
                }
            }
        }
    }
    bounds
}

/// The point where three planes meet, or `None` if two are parallel.
fn intersect(a: (Vec3, f32), b: (Vec3, f32), c: (Vec3, f32)) -> Option<Vec3> {
    let denominator = a.0.dot(b.0.cross(c.0));
    if denominator.abs() < 1e-6 {
        return None;
    }
    Some((b.0.cross(c.0) * a.1 + c.0.cross(a.0) * b.1 + a.0.cross(b.0) * c.1) / denominator)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn three_planes_meet_at_a_corner() {
        let corner = intersect((Vec3::X, 1.0), (Vec3::Y, 2.0), (Vec3::Z, 3.0));
        assert_eq!(corner, Some(Vec3::new(1.0, 2.0, 3.0)));
        assert_eq!(
            intersect((Vec3::X, 1.0), (Vec3::X, 2.0), (Vec3::Z, 3.0)),
            None
        );
    }

    #[test]
    fn ramps_gain_a_top_bevel() {
        let world = WorldBuilder::new()
            .ramp(Vec3::ZERO, Vec3::new(100.0, 50.0, 100.0))
            .build();
        let up_ray = world.trace_ray(
            Vec3::new(50.0, 25.0, 200.0),
            Vec3::new(50.0, 25.0, 0.0),
            contents::SOLID,
        );
        // The slope is at z = 50 halfway along.
        assert!((up_ray.end.z - 50.0).abs() < 0.1, "{up_ray:?}");
        let normal = Vec3::new(-1.0, 0.0, 1.0).normalize();
        assert!(up_ray.normal.abs_diff_eq(normal, 1e-5));
    }
}
