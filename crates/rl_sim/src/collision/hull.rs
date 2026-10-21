//! Convex hulls — the collision pieces of props — as clip planes.
//!
//! A prop's collision model is a handful of convex hulls given as points and
//! triangles. To sweep a player's box against one exactly, with the same
//! clip as brushes ([`super::clip`]), the hull is turned into planes: its
//! face planes, plus *bevels* along every other separating axis of box and
//! hull — the world axes and each hull edge crossed with them (see
//! [`super::triangle`] for why). Faces and edges are kept as directions in
//! the hull's own frame; the planes are built in world space for a given
//! placement, since the bevels depend on the world axes.

use glam::{Quat, Vec3};

use super::clip::ClipPlane;

/// Tolerance for a point to count as inside a plane when finding corners.
const INSIDE_EPSILON: f32 = 0.01;
/// Two unit directions closer than this (component-wise) are the same.
const SAME_DIRECTION: f32 = 1e-3;
/// Squared length below which a cross product is degenerate.
const DEGENERATE_SQ: f32 = 1e-8;

/// A convex volume: its corners, face normals and edge directions in its
/// own frame.
#[derive(Debug, Clone, PartialEq)]
pub struct ConvexHull {
    vertices: Vec<Vec3>,
    normals: Vec<Vec3>,
    edges: Vec<Vec3>,
}

impl ConvexHull {
    /// The hull of a closed triangle mesh around a convex volume, or `None`
    /// if it has no volume. The triangles' winding does not matter: normals
    /// are turned to face away from the centre.
    #[must_use]
    pub fn from_triangles(vertices: Vec<Vec3>, triangles: &[[u16; 3]]) -> Option<Self> {
        if vertices.len() < 4 {
            return None;
        }
        let centre = vertices.iter().copied().sum::<Vec3>() / vertices.len() as f32;
        let mut normals = Vec::new();
        let mut edges = Vec::new();
        for triangle in triangles {
            let [a, b, c] = triangle.map(|index| vertices.get(usize::from(index)).copied());
            let (Some(a), Some(b), Some(c)) = (a, b, c) else {
                continue;
            };
            if let Some(normal) = (b - a).cross(c - a).try_normalize() {
                let outward = if normal.dot(a - centre) < 0.0 {
                    -normal
                } else {
                    normal
                };
                push_unique(&mut normals, outward, false);
            }
            for edge in [b - a, c - b, a - c] {
                if let Some(direction) = edge.try_normalize() {
                    push_unique(&mut edges, direction, true);
                }
            }
        }
        (normals.len() >= 4).then_some(Self {
            vertices,
            normals,
            edges,
        })
    }

    /// A ball of `radius` about the origin, approximated by an icosahedron
    /// (twenty faces: round enough to roll, cheap enough to collide).
    #[must_use]
    pub fn ball(radius: f32) -> Self {
        const FACES: [[u16; 3]; 20] = [
            [0, 11, 5],
            [0, 5, 1],
            [0, 1, 7],
            [0, 7, 10],
            [0, 10, 11],
            [1, 5, 9],
            [5, 11, 4],
            [11, 10, 2],
            [10, 7, 6],
            [7, 1, 8],
            [3, 9, 4],
            [3, 4, 2],
            [3, 2, 6],
            [3, 6, 8],
            [3, 8, 9],
            [4, 9, 5],
            [2, 4, 11],
            [6, 2, 10],
            [8, 6, 7],
            [9, 8, 1],
        ];
        let phi = f32::midpoint(1.0, 5.0_f32.sqrt());
        let scale = radius / (1.0 + phi * phi).sqrt();
        let vertices: Vec<Vec3> = [
            (-1.0, phi, 0.0),
            (1.0, phi, 0.0),
            (-1.0, -phi, 0.0),
            (1.0, -phi, 0.0),
            (0.0, -1.0, phi),
            (0.0, 1.0, phi),
            (0.0, -1.0, -phi),
            (0.0, 1.0, -phi),
            (phi, 0.0, -1.0),
            (phi, 0.0, 1.0),
            (-phi, 0.0, -1.0),
            (-phi, 0.0, 1.0),
        ]
        .into_iter()
        .map(|(x, y, z)| Vec3::new(x, y, z) * scale)
        .collect();
        // The icosahedron is a closed convex mesh: it always has a hull.
        Self::from_triangles(vertices, &FACES)
            .unwrap_or_else(|| Self::from_box(Vec3::splat(-radius), Vec3::splat(radius)))
    }

    /// An axis-aligned box.
    #[must_use]
    pub fn from_box(mins: Vec3, maxs: Vec3) -> Self {
        let vertices = (0..8)
            .map(|corner| {
                Vec3::new(
                    if corner & 1 == 0 { mins.x } else { maxs.x },
                    if corner & 2 == 0 { mins.y } else { maxs.y },
                    if corner & 4 == 0 { mins.z } else { maxs.z },
                )
            })
            .collect();
        Self {
            vertices,
            normals: vec![
                Vec3::X,
                Vec3::NEG_X,
                Vec3::Y,
                Vec3::NEG_Y,
                Vec3::Z,
                Vec3::NEG_Z,
            ],
            edges: vec![Vec3::X, Vec3::Y, Vec3::Z],
        }
    }

    /// The hull scaled uniformly about its frame's origin.
    #[must_use]
    pub fn scaled(mut self, scale: f32) -> Self {
        for vertex in &mut self.vertices {
            *vertex *= scale;
        }
        self
    }

    /// The corners, in the hull's frame.
    #[must_use]
    pub fn vertices(&self) -> &[Vec3] {
        &self.vertices
    }

    /// Bounds of the corners in the hull's frame.
    #[must_use]
    pub fn bounds(&self) -> (Vec3, Vec3) {
        self.vertices.iter().fold(
            (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY)),
            |(min, max), &vertex| (min.min(vertex), max.max(vertex)),
        )
    }

    /// Writes the hull's clip planes, placed by `rotation` then
    /// `translation`, into `planes` (replacing its contents), and returns
    /// the placed hull's world bounds.
    pub(crate) fn world_planes(
        &self,
        rotation: Quat,
        translation: Vec3,
        planes: &mut Vec<ClipPlane>,
    ) -> (Vec3, Vec3) {
        let world: Vec<Vec3> = self
            .vertices
            .iter()
            .map(|&vertex| rotation * vertex + translation)
            .collect();
        let support = |direction: Vec3| {
            world
                .iter()
                .map(|vertex| direction.dot(*vertex))
                .fold(f32::NEG_INFINITY, f32::max)
        };
        planes.clear();
        for &normal in &self.normals {
            let normal = rotation * normal;
            planes.push(ClipPlane {
                normal,
                dist: support(normal),
                bevel: false,
            });
        }
        let mut directions: Vec<Vec3> = planes.iter().map(|plane| plane.normal).collect();
        let mut bevel = |direction: Vec3, planes: &mut Vec<ClipPlane>| {
            if directions
                .iter()
                .any(|known| known.abs_diff_eq(direction, SAME_DIRECTION))
            {
                return;
            }
            directions.push(direction);
            planes.push(ClipPlane {
                normal: direction,
                dist: support(direction),
                bevel: true,
            });
        };
        for axis in [Vec3::X, Vec3::Y, Vec3::Z] {
            bevel(axis, planes);
            bevel(-axis, planes);
        }
        for &edge in &self.edges {
            let edge = rotation * edge;
            for axis in [Vec3::X, Vec3::Y, Vec3::Z] {
                let cross = edge.cross(axis);
                if cross.length_squared() < DEGENERATE_SQ {
                    continue;
                }
                let direction = cross.normalize();
                bevel(direction, planes);
                bevel(-direction, planes);
            }
        }
        world.iter().fold(
            (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY)),
            |(min, max), &vertex| (min.min(vertex), max.max(vertex)),
        )
    }
}

/// The corners of the convex volume `normal · p <= dist` bounded by
/// `planes`: every point where three planes meet that lies inside all of
/// them. Corners shared by more than three planes appear more than once.
/// Empty for an empty volume or one with no corners at all (a slab); an
/// otherwise unbounded volume yields only the corners it has.
pub(crate) fn plane_corners(planes: &[(Vec3, f32)]) -> Vec<Vec3> {
    let mut corners = Vec::new();
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
                    corners.push(corner);
                }
            }
        }
    }
    corners
}

/// The point where three planes meet, or `None` if two are parallel.
fn intersect(a: (Vec3, f32), b: (Vec3, f32), c: (Vec3, f32)) -> Option<Vec3> {
    let denominator = a.0.dot(b.0.cross(c.0));
    if denominator.abs() < 1e-6 {
        return None;
    }
    Some((b.0.cross(c.0) * a.1 + c.0.cross(a.0) * b.1 + a.0.cross(b.0) * c.1) / denominator)
}

/// Adds `direction` unless already present; with `either_sign`, its
/// opposite counts as present too (edge directions have no sign).
fn push_unique(directions: &mut Vec<Vec3>, direction: Vec3, either_sign: bool) {
    let known = directions.iter().any(|&known| {
        known.abs_diff_eq(direction, SAME_DIRECTION)
            || (either_sign && known.abs_diff_eq(-direction, SAME_DIRECTION))
    });
    if !known {
        directions.push(direction);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collision::clip::{Hit, Sweep, clip_convex};

    #[test]
    fn a_ball_is_a_closed_hull_of_its_radius() {
        let ball = ConvexHull::ball(4.0);
        assert_eq!(ball.vertices().len(), 12);
        for vertex in ball.vertices() {
            assert!((vertex.length() - 4.0).abs() < 1e-4, "{vertex}");
        }
        let (mins, maxs) = ball.bounds();
        assert!(mins.max_element() < 0.0 && maxs.min_element() > 0.0);
        assert!(maxs.max_element() <= 4.0 + 1e-4);
        // Every face's plane is where an icosahedron's inscribed sphere is.
        let inscribed = 4.0 * 0.794_654_5;
        assert!(maxs.max_element() > inscribed);
    }

    /// A tetrahedron with its right angle at the origin.
    fn tetrahedron() -> ConvexHull {
        let vertices = vec![Vec3::ZERO, Vec3::X * 10.0, Vec3::Y * 10.0, Vec3::Z * 10.0];
        ConvexHull::from_triangles(vertices, &[[0, 1, 2], [0, 3, 1], [0, 2, 3], [1, 3, 2]])
            .expect("hull")
    }

    #[test]
    fn faces_point_outward_whatever_the_winding() {
        let hull = tetrahedron();
        let mut planes = Vec::new();
        hull.world_planes(Quat::IDENTITY, Vec3::ZERO, &mut planes);
        let faces: Vec<_> = planes.iter().filter(|plane| !plane.bevel).collect();
        assert_eq!(faces.len(), 4);
        let slanted = Vec3::ONE.normalize();
        assert!(
            faces
                .iter()
                .any(|plane| plane.normal.abs_diff_eq(slanted, 1e-5))
        );
        assert!(faces.iter().any(|plane| plane.normal == Vec3::NEG_Z));
    }

    #[test]
    fn every_plane_supports_the_placed_hull() {
        let hull = tetrahedron();
        let rotation = Quat::from_euler(glam::EulerRot::XYZ, 0.3, 1.1, -0.4);
        let offset = Vec3::new(5.0, -3.0, 2.0);
        let mut planes = Vec::new();
        let (mins, maxs) = hull.world_planes(rotation, offset, &mut planes);
        for plane in &planes {
            let farthest = hull
                .vertices()
                .iter()
                .map(|&v| plane.normal.dot(rotation * v + offset))
                .fold(f32::NEG_INFINITY, f32::max);
            assert!((farthest - plane.dist).abs() < 1e-3, "{plane:?}");
        }
        for vertex in hull.vertices() {
            let placed = rotation * *vertex + offset;
            assert!(placed.cmpge(mins - 1e-4).all() && placed.cmple(maxs + 1e-4).all());
        }
    }

    #[test]
    fn a_box_swept_past_a_slanted_face_is_not_caught_by_its_bounds() {
        // A box moving down beside the tetrahedron's slanted face, inside
        // its bounding box but clear of the volume: the edge bevels must
        // let it pass where a bounding box would stop it.
        let hull = tetrahedron();
        let mut planes = Vec::new();
        hull.world_planes(Quat::IDENTITY, Vec3::ZERO, &mut planes);
        let sweep = Sweep::new(
            Vec3::new(8.0, 8.0, 20.0),
            Vec3::new(8.0, 8.0, 4.0),
            Vec3::splat(1.0),
            u32::MAX,
        );
        let mut hit = Hit::default();
        clip_convex(&sweep, planes.iter().copied(), 1, &mut hit);
        assert_eq!(hit.fraction, 1.0, "{hit:?}");
        // Straight down onto the hull is a hit.
        let onto = Sweep::new(
            Vec3::new(1.0, 1.0, 20.0),
            Vec3::new(1.0, 1.0, 0.0),
            Vec3::ONE,
            1,
        );
        let mut hit = Hit::default();
        clip_convex(&onto, planes.iter().copied(), 1, &mut hit);
        assert!(hit.fraction < 1.0);
    }

    #[test]
    fn boxes_have_axis_faces_and_no_bevels_unrotated() {
        let hull = ConvexHull::from_box(Vec3::splat(-1.0), Vec3::ONE).scaled(2.0);
        let mut planes = Vec::new();
        let (mins, maxs) = hull.world_planes(Quat::IDENTITY, Vec3::ZERO, &mut planes);
        assert_eq!(planes.len(), 6, "edge × axis bevels coincide with faces");
        assert_eq!((mins, maxs), (Vec3::splat(-2.0), Vec3::splat(2.0)));
    }

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
    fn a_cube_has_eight_corners() {
        let planes: Vec<(Vec3, f32)> = [Vec3::X, Vec3::Y, Vec3::Z]
            .into_iter()
            .flat_map(|axis| [(axis, 1.0), (-axis, 1.0)])
            .collect();
        let corners = plane_corners(&planes);
        assert_eq!(corners.len(), 8);
        assert!(corners.iter().all(|corner| corner.abs() == Vec3::ONE));
        assert_eq!(plane_corners(&planes[..5]).len(), 4, "open at the bottom");
        assert!(
            plane_corners(&planes[..2]).is_empty(),
            "a slab has no corners"
        );
    }

    #[test]
    fn flat_meshes_are_not_hulls() {
        let square = vec![Vec3::ZERO, Vec3::X, Vec3::Y, Vec3::new(1.0, 1.0, 0.0)];
        assert!(ConvexHull::from_triangles(square, &[[0, 1, 2], [1, 3, 2]]).is_none());
    }
}
