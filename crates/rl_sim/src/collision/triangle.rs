//! Displacement triangles as convex volumes.
//!
//! Displacements collide as their tessellated triangles. Each triangle is
//! turned into a thin prism — the triangle and a copy [`THICKNESS`] behind
//! it — so the same exact box clip used for brushes ([`super::clip`]) applies.
//!
//! For a box sweep to be exact against a convex polytope, the clip must see
//! every *separating axis* of box and polytope as a plane: the polytope's
//! face normals, the box's face normals (the world axes) and the cross
//! products of their edges. So beside the prism's five faces, each triangle
//! gets axis-aligned bevels at its bounds and a pair of bevels for every
//! edge × axis cross product, each placed at the prism's support distance
//! along it. Bevels do not change the prism's shape, only how a box catches
//! on its edges; rays skip them.

use glam::Vec3;

use super::clip::ClipPlane;

/// Depth of a triangle's prism behind its surface, units. Thin enough to be
/// invisible, thick enough that a ray crossing the surface is caught.
pub(crate) const THICKNESS: f32 = 1.0;

/// Upper bound on a prism's planes: 2 faces + 3 sides + 6 axial bevels +
/// 18 edge bevels.
pub(crate) const MAX_PLANES: usize = 29;

/// Squared length below which a cross product is treated as degenerate.
const DEGENERATE_SQ: f32 = 1e-10;

/// The planes of a triangle's prism, in a fixed-size buffer.
#[derive(Debug, Clone, Copy)]
pub(crate) struct TrianglePlanes {
    planes: [ClipPlane; MAX_PLANES],
    count: usize,
}

impl TrianglePlanes {
    /// The planes, faces first.
    pub(crate) fn as_slice(&self) -> &[ClipPlane] {
        &self.planes[..self.count]
    }

    fn push(&mut self, normal: Vec3, dist: f32, bevel: bool) {
        if self.count < MAX_PLANES {
            self.planes[self.count] = ClipPlane {
                normal,
                dist,
                bevel,
                material: None,
            };
            self.count += 1;
        }
    }

    /// Marks every plane with the displacement's `material`.
    pub(crate) fn set_material(&mut self, material: Option<u16>) {
        for plane in &mut self.planes[..self.count] {
            plane.material = material;
        }
    }
}

/// The prism planes of triangle `[a, b, c]`, or `None` if it has no area.
pub(crate) fn prism_planes(triangle: [Vec3; 3]) -> Option<TrianglePlanes> {
    let [a, b, c] = triangle;
    let normal = (b - a).cross(c - a).try_normalize()?;
    let back = triangle.map(|vertex| vertex - normal * THICKNESS);
    let vertices = [a, b, c, back[0], back[1], back[2]];
    let support = |axis: Vec3| {
        vertices
            .iter()
            .map(|vertex| axis.dot(*vertex))
            .fold(f32::NEG_INFINITY, f32::max)
    };

    let mut planes = TrianglePlanes {
        planes: [ClipPlane {
            normal: Vec3::Z,
            dist: 0.0,
            bevel: true,
            material: None,
        }; MAX_PLANES],
        count: 0,
    };
    // The two faces.
    planes.push(normal, normal.dot(a), false);
    planes.push(-normal, support(-normal), false);
    // The three sides, perpendicular to the surface through each edge. The
    // cross product points away from the opposite vertex for either winding,
    // since `normal` was derived from the same winding.
    let edges = [b - a, c - b, a - c];
    for (edge, start) in edges.iter().zip([a, b, c]) {
        if let Some(side) = edge.cross(normal).try_normalize() {
            planes.push(side, side.dot(start), false);
        }
    }
    // Axis-aligned bevels.
    for axis in [Vec3::X, Vec3::Y, Vec3::Z] {
        for direction in [axis, -axis] {
            planes.push(direction, support(direction), true);
        }
    }
    // Edge × axis bevels.
    for edge in edges {
        for axis in [Vec3::X, Vec3::Y, Vec3::Z] {
            let cross = edge.cross(axis);
            if cross.length_squared() < DEGENERATE_SQ {
                continue;
            }
            let direction = cross.normalize();
            planes.push(direction, support(direction), true);
            planes.push(-direction, support(-direction), true);
        }
    }
    Some(planes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn floor_triangle() -> [Vec3; 3] {
        [
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(100.0, 0.0, 0.0),
            Vec3::new(0.0, 100.0, 0.0),
        ]
    }

    #[test]
    fn every_vertex_lies_on_or_behind_every_plane() {
        let triangle = floor_triangle();
        let planes = prism_planes(triangle).expect("planes");
        for plane in planes.as_slice() {
            for vertex in triangle {
                assert!(
                    plane.normal.dot(vertex) <= plane.dist + 1e-3,
                    "{plane:?} cuts {vertex}"
                );
            }
        }
    }

    #[test]
    fn faces_come_first_and_are_not_bevels() {
        let planes = prism_planes(floor_triangle()).expect("planes");
        let faces: Vec<_> = planes.as_slice().iter().take(5).collect();
        assert!(faces.iter().all(|plane| !plane.bevel));
        assert_eq!(faces[0].normal, Vec3::Z);
        assert!(planes.as_slice()[5..].iter().all(|plane| plane.bevel));
    }

    #[test]
    fn degenerate_triangles_have_no_planes() {
        assert!(prism_planes([Vec3::ZERO, Vec3::X, Vec3::X * 2.0]).is_none());
    }
}
